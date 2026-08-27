---
title: Wayland Surface Reconfiguration Design
description: "Design for nonblocking wgpu surface recovery on Linux Wayland."
---

# Wayland Surface Reconfiguration Design

## Status {#wayland-reconfiguration-status}

- State: Implemented; runtime validation pending
- Date: 2026-08-27
- Scope: Linux Wayland with the wgpu Vulkan backend
- Main code areas: `gpui_wgpu` and `gpui_linux`

The current implementation moves Wayland runtime configuration to a dedicated
worker, serializes it with rendering, coalesces configuration generations,
wakes calloop after completion, and recreates a `Lost` surface before it
configures it. Closing a window now defers native Wayland-object destruction
until an active surface operation completes.

## Summary {#wayland-reconfiguration-summary}

Flint can stop processing user input when wgpu reconfigures a Wayland surface.
The main thread calls `wgpu::Surface::configure` from a frame callback. The
NVIDIA Vulkan driver can make a synchronous Wayland round trip during this
call. If the compositor is slow, the main thread waits in `poll` with no
timeout.

Flint will move runtime surface configuration to a dedicated worker. A
device-wide coordinator will serialize configuration with GPU submissions.
The main thread will use only nonblocking coordinator operations. It will skip
frames while configuration is active, but it will continue to process input and
window events.

The first implementation will also use a valid `Suboptimal` frame before it
requests configuration. It will recreate a `Lost` surface instead of treating
it as an `Outdated` surface.

## Terms {#wayland-reconfiguration-terms}

This document uses these terms:

- **Surface**: A `wgpu::Surface` that connects a native window to a swapchain.
- **Surface configuration**: The size, format, present mode, alpha mode, usage,
  and frame-latency values that Flint gives to `Surface::configure`.
- **Configuration request**: A request to apply a surface configuration or to
  recreate and configure a surface.
- **Configuration generation**: A number that identifies one requested surface
  configuration. A later generation replaces an earlier pending generation.
- **Surface worker**: A dedicated operating system thread that performs
  blocking surface operations.
- **Device coordinator**: Shared state that prevents GPU submissions while the
  surface worker configures a surface on the same device.
- **Device permit**: A shared permit for normal GPU work or an exclusive permit
  for surface configuration.
- **Ready state**: The surface is configured for the latest generation and can
  acquire frames.
- **Recovery state**: The surface needs configuration or recreation and must
  not acquire frames.

## Problem {#wayland-reconfiguration-problem}

`WgpuRenderer::draw` calls `Surface::configure` immediately when
`get_current_texture` returns `Suboptimal`, `Outdated`, or `Lost`:

```text
Wayland frame callback
  -> WgpuRenderer::draw
  -> Surface::configure
  -> wgpu_core::Device::configure_surface
  -> Vulkan surface_capabilities
  -> NVIDIA Wayland round trip
  -> poll with no timeout
```

The call runs on the main thread. A slow compositor therefore stops input,
window events, drawing, and application commands.

The observed incidents have these properties:

- Each incident has the same main-thread stack.
- The main thread waits in the NVIDIA Wayland Vulkan path.
- Other Flint threads are idle or parked.
- No Flint lock owner blocks the main thread.
- The host has sustained storage pressure.
- The compositor can wait on Mesa shader-cache file I/O at the same time.

This is an availability fault at a synchronous system boundary. The compositor
can recover, but Flint must not make its event loop depend on prompt compositor
service.

## Current Behavior {#wayland-reconfiguration-current-behavior}

Runtime surface configuration occurs in more than one path:

- `WgpuRenderer::draw` configures after `Suboptimal`, `Outdated`, and `Lost`.
- `WgpuRenderer::update_drawable_size` waits for the device with no timeout and
  then configures the surface.
- `WgpuRenderer::update_transparency` configures the surface.
- `WgpuRenderer::replace_surface` configures a replacement surface.
- Device recovery and initial renderer creation also configure a surface.

The draw path drops a valid `Suboptimal` frame before configuration. wgpu
defines this frame as usable. The application should configure again for best
operation, but it does not have to discard the frame immediately.

The draw path also handles `Lost` and `Outdated` in the same branch. wgpu
requires surface recreation after `Lost`. Configuration of the old surface is
not sufficient.

## Constraints {#wayland-reconfiguration-constraints}

### Vulkan has no cancellation contract

{#wayland-reconfiguration-vulkan-cancellation}

`vkGetPhysicalDeviceSurfaceCapabilitiesKHR` is synchronous. It has no timeout
or cancellation argument. `VK_KHR_get_surface_capabilities2` does not add one.
The Wayland round trip occurs inside the Vulkan driver. The Rust
`wayland-client` event queue cannot control it.

Flint can detect a slow call, but it cannot safely cancel the call. It must not
start a second operation on the same surface while the first operation can
still complete.

### Configuration synchronizes the device

{#wayland-reconfiguration-device-synchronization}

wgpu waits for the device to become idle during `Surface::configure`. A queue
submission during this wait can cause a validation error. Flint shares one
`wgpu::Device` between windows. A background configuration task must therefore
coordinate with all renderers that use that device.

### The main thread must not wait

{#wayland-reconfiguration-main-thread}

The main thread must not wait for:

- A device permit.
- A surface-worker response.
- Device idle state.
- Surface configuration.
- Worker shutdown.

If a required resource is busy, the main thread must skip the frame or defer
the operation.

### The native window must stay valid

{#wayland-reconfiguration-window-lifetime}

A Vulkan surface can refer to a native `wl_surface`. Flint must keep the native
window and its raw handles valid until a configuration operation completes.
Window close must not destroy the native surface while the worker can still use
it.

## Goals {#wayland-reconfiguration-goals}

- Keep the Wayland event loop responsive during a slow surface configuration.
- Preserve correct resize, transparency, and recovery behavior.
- Serialize surface configuration with GPU submissions on the shared device.
- Coalesce rapid resize requests and apply the newest requested size.
- Use valid `Suboptimal` frames before recovery starts.
- Recreate a `Lost` surface.
- Provide enough telemetry to identify the reason and duration of each
  configuration.
- Keep the change limited to the wgpu renderer and Linux platform integration.

## Non-Goals {#wayland-reconfiguration-non-goals}

- Cancel a Vulkan or driver call after it starts.
- Guarantee new rendered frames while the driver is blocked.
- Fix compositor storage I/O behavior.
- Add process or cgroup resource controls.
- Change Vulkan drivers.
- Add a wgpu capability cache in the first implementation.
- Move all GPUI rendering to a render thread in the first implementation.
- Change the X11, macOS, Windows, or web renderer behavior unless shared code
  requires a small interface change.

## Proposed Architecture {#wayland-reconfiguration-architecture}

### Device coordinator {#wayland-reconfiguration-device-coordinator}

Add one thread-safe device coordinator to `WgpuContext`. All renderers that
share the device also share this coordinator.

The coordinator provides two permit operations:

- `try_begin_gpu_work` returns a shared permit or returns immediately with no
  permit.
- `begin_surface_configuration` waits on the surface worker and returns an
  exclusive permit.

The main thread uses only `try_begin_gpu_work`. It holds the shared permit for
all frame work, including atlas work, texture acquisition, command encoding,
queue submission, and presentation. If it cannot get the permit, `draw`
returns `false`.

The surface worker takes the exclusive permit before it calls
`Surface::configure`. It holds the permit until the call and its error handling
complete. This stops new queue submissions during configuration.

All device and queue use outside `WgpuRenderer::draw` must be audited. A path
that can submit work during configuration must also use a shared permit or move
behind the worker boundary.

### Surface worker {#wayland-reconfiguration-surface-worker}

Create one dedicated surface worker for each `WgpuContext`. Do not run blocking
surface operations on the general GPUI executor pool.

The worker receives configuration requests through a channel. It performs only
one request at a time. A request contains:

```text
window identifier
surface handle or surface creation data
device handle
surface configuration
configuration generation
request reason
native-window lifetime lease
```

The worker takes the exclusive device permit, performs the operation, records
its duration, and sends a completion event to the main event loop. The
completion event must wake the calloop loop. The main thread must not depend on
a later Wayland frame callback to observe completion.

The worker must own or share all objects that it uses. `wgpu::Surface` is
`Send + Sync` but is not cloneable. The renderer can hold it in an `Arc`, or it
can transfer exclusive surface ownership to a worker-managed surface slot. An
`Arc` gives the smaller change. The recovery state prevents the main thread
from using the surface during configuration.

### Configuration generations

{#wayland-reconfiguration-configuration-generations}

Each renderer stores a monotonically increasing configuration generation and
the latest desired configuration.

When Flint receives a resize or another configuration change:

1. Update the desired configuration.
2. Increment its generation.
3. Mark the surface as needing configuration.
4. Submit or update the pending request.

The worker can already be processing an older generation. It must not process
all intermediate resize sizes. After one operation completes, the coordinator
compares the completed generation with the latest generation:

- If they match, mark the surface `Ready`.
- If the latest generation is newer, configure the newest generation next.

This policy gives the compositor the latest size and prevents a resize queue
from growing without a limit.

### Renderer state {#wayland-reconfiguration-renderer-state}

Replace the single `surface_configured` Boolean with an explicit state:

```text
Ready {
    generation
}

ConfigurationPending {
    generation,
    reason
}

Configuring {
    generation,
    reason,
    started_at
}

RecreatePending {
    generation
}

Unavailable {
    error,
    retry_at
}

Unconfigured
```

`Unconfigured` keeps the current mobile lifecycle meaning. The Linux recovery
states must not reuse it because Linux recovery needs worker completion and
retry information.

Only `Ready` can call `get_current_texture`. All other states return `false`
from `draw`.

## Surface Result Policy {#wayland-reconfiguration-result-policy}

### Success {#wayland-reconfiguration-success}

Use the frame normally.

### Suboptimal {#wayland-reconfiguration-suboptimal}

Use, submit, and present the returned frame. At the end of the draw, request
configuration for the current desired generation. This order ensures that no
live `SurfaceTexture` exists when configuration starts.

Repeated `Suboptimal` results must not create repeated requests. One pending or
active request is sufficient.

### Outdated {#wayland-reconfiguration-outdated}

Do not draw. Request configuration for the latest desired generation. Return
to the event loop immediately.

### Lost {#wayland-reconfiguration-lost}

Do not configure the old surface. Request surface recreation with current
native-window data, then configure the new surface. Replace the renderer's
surface only after successful completion on the main thread.

The recreation request must hold a native-window lifetime lease. This lease
prevents destruction of the `wl_surface` and display data while the worker uses
the raw handles.

### Timeout and Occluded {#wayland-reconfiguration-timeout-occluded}

Skip the frame. Do not configure only because of these results.

### Validation {#wayland-reconfiguration-validation}

Record the validation error through the existing GPU error path. Do not start
an immediate configuration loop. Apply bounded retry policy after the error is
known.

## Resize and Configuration Changes

{#wayland-reconfiguration-resize}

`update_drawable_size` will update desired state and enqueue a configuration
request. It will not call `Device::poll` or `Surface::configure` on the main
thread.

The worker does not need the current explicit unlimited `Device::poll` before
configuration. wgpu already waits for the device during configuration.

Transparency changes use the same request path. Pipeline replacement must
occur only after the matching surface configuration succeeds. A generation
must therefore include all values that affect both the surface and its
pipelines.

Surface replacement and device recovery use the same coordinator, but they can
use different worker operations. Initial renderer creation can remain
synchronous because the application window is not yet interactive. It should
still record configuration duration for diagnosis.

## Completion and Redraw {#wayland-reconfiguration-completion}

The worker sends a completion event with:

- Window identifier.
- Completed generation.
- Request reason.
- Duration.
- New surface for a recreation request.
- Success or error status.

The event handler runs on the main thread. It performs these steps:

1. Ignore the event if its window no longer accepts completions.
2. Install a recreated surface when applicable.
3. Compare the completed and desired generations.
4. Invalidate size-dependent intermediate textures.
5. Mark the surface `Ready`, or enqueue the latest generation.
6. Request a new GPUI frame.

The handler must never wait for the worker.

## Error and Retry Policy {#wayland-reconfiguration-errors}

`Surface::configure` returns no direct result in the public wgpu API. Flint
currently receives configuration errors through the device error callback. The
implementation must associate such errors with the active configuration when
possible. An error scope can be used if the selected wgpu API permits the
worker to collect the result without main-thread work.

Use bounded retry delays for returned configuration errors:

- First retry: next eligible frame or completion cycle.
- Later retries: increasing delay with a maximum delay.
- A new resize or surface recreation request resets the retry delay.
- Device loss uses the existing device recovery path.

A slow call is not a returned error. A watchdog can report it, but it must not
start another configuration call.

## Watchdog and Telemetry {#wayland-reconfiguration-telemetry}

Record these fields for every configuration:

- Window identifier.
- Request reason: resize, suboptimal, outdated, lost, transparency, replacement,
  or device recovery.
- Generation.
- Requested width and height.
- GPU adapter, backend, and driver.
- Queue delay before the worker starts.
- Device-permit wait duration.
- Configuration duration.
- Completion result.
- Whether a newer generation replaced the result.

Emit a warning when configuration exceeds a short threshold, such as 250 ms.
Emit a high-severity warning after a longer threshold, such as 2 seconds. The
watchdog reports state only. It does not cancel the worker.

Add counters for:

- Configuration requests by reason.
- Coalesced requests.
- Slow configurations.
- Failed configurations.
- Frames skipped because configuration owns the device permit.

## Window Close and Shutdown {#wayland-reconfiguration-shutdown}

Window close creates a lifetime problem if the surface worker is blocked.
Flint must not destroy the native `wl_surface` while Vulkan can use it.

Use this policy:

1. Mark the renderer as closing and reject new requests.
2. Remove pending requests that have not started.
3. If no request is active, destroy renderer and Wayland objects in the current
   order.
4. If a request is active, defer native surface destruction until its lifetime
   lease returns.
5. Do not block the main thread or join the worker.

If a driver call never returns, the deferred native objects remain alive until
process exit. This is a bounded safety leak for one closing window. It is safer
than destroying a native object that the driver still uses.

Application shutdown must not join a blocked surface worker. Process exit can
end the worker.

## Capability Cache {#wayland-reconfiguration-capability-cache}

wgpu queries surface capabilities during every configuration. The Vulkan HAL
then creates the swapchain without another explicit capability query. A cache
inside the Flint wgpu fork could avoid the exact capability call in the
observed stack.

Do not include this cache in the first fix. Surface capabilities can change,
and `Outdated` reports a surface change. `vkCreateSwapchainKHR` can also block,
so a cache does not remove the need for a worker.

A later wgpu experiment can reuse capabilities only when all of these values
match:

- Native surface identity.
- Adapter identity.
- Format.
- Present mode.
- Alpha mode.
- Usage.
- Desired frame latency.

Surface recreation and `Lost` must invalidate the cache. The experiment must
fall back to a fresh query after any configuration error.

## Alternatives {#wayland-reconfiguration-alternatives}

### Keep configuration on the main thread

{#wayland-reconfiguration-alternative-main-thread}

This keeps the current simple ownership model. It does not meet the main goal.
Any compositor or driver delay still stops the event loop.

### Add a timeout around `Surface::configure`

{#wayland-reconfiguration-alternative-timeout}

A timeout can stop waiting for a worker result, but it cannot cancel the Vulkan
call. Starting another call can race with the first call. This option is useful
only as a watchdog policy.

### Use a general executor task

{#wayland-reconfiguration-alternative-executor}

A general executor task keeps the main thread free but can occupy an executor
thread without a limit. It also does not serialize shared-device submissions.
A dedicated worker and coordinator are required.

### Defer configuration to another main-loop callback

{#wayland-reconfiguration-alternative-defer}

This can make one frame callback short, but a later callback still blocks the
event loop. It does not contain the fault.

### Cache capabilities only

{#wayland-reconfiguration-alternative-cache}

This can avoid one driver query. It cannot prevent another synchronous Vulkan
operation from blocking. It also creates stale-capability risk.

### Move all GPU work to a render thread

{#wayland-reconfiguration-alternative-render-thread}

This gives the strongest isolation. One thread owns all device, queue, surface,
and swapchain work. The main thread sends scenes and configuration updates.
This model also removes shared-device races.

It is a much larger GPUI change. Scene transfer, atlas access, frame completion,
multiwindow scheduling, and shutdown all need new thread-safe interfaces. Keep
it as a later architecture option if the dedicated surface worker does not give
enough isolation.

### Use one device per window

{#wayland-reconfiguration-alternative-device-per-window}

This prevents one window's configuration from stopping rendering on another
window. It increases GPU memory use and duplicates pipelines, atlases, and
device state. It also does not keep the main thread safe unless configuration
runs on a worker.

## Test Plan {#wayland-reconfiguration-test-plan}

### Unit tests {#wayland-reconfiguration-unit-tests}

Use a fake surface operation and a controllable worker:

- A slow configuration does not block a main-thread heartbeat.
- `draw` skips a frame when it cannot get a shared device permit.
- Rapid resize requests coalesce to the newest generation.
- A completion for an old generation starts the newest generation.
- `Suboptimal` presents its frame before it requests configuration.
- `Outdated` requests configuration and does not draw.
- `Lost` requests recreation, not configuration of the old surface.
- Only one configuration can run for a device.
- Closing a window rejects new requests.
- Closing during an active request defers native surface destruction.

### Integration tests {#wayland-reconfiguration-integration-tests}

Add a test hook that delays the surface operation. It must work without a real
slow compositor. Verify these cases:

- Input and application commands continue during the delay.
- Completion wakes the event loop and requests a frame.
- Interactive resize applies the final size.
- Transparency change and resize produce compatible final pipelines.
- Two windows do not submit GPU work while one configuration has the exclusive
  permit.
- A closed window does not receive or install a late surface.
- Application shutdown does not wait for a blocked worker.

### Driver fault test {#wayland-reconfiguration-driver-test}

Use a Vulkan layer or a test interposer to delay
`vkGetPhysicalDeviceSurfaceCapabilitiesKHR`. Run this test on Wayland with the
Vulkan backend. Check that:

- The delay appears in worker telemetry.
- The main thread continues to dispatch events.
- No second configuration starts during the delay.
- Rendering resumes after the delayed call returns.

For a local debug build, Flint also supports a worker-delay hook. It delays the
surface worker while it holds the same device permit as a real WSI stall:

```sh
FLINT_TEST_SURFACE_CONFIGURE_DELAY_MS=3000 cargo run -p flint
```

Resize a Wayland window while this variable is set. Input and window events
must continue while drawing waits for the worker. The hook is not compiled into
release builds, and it limits the delay to 60 seconds.

Also test resize, scale changes, minimize and restore, monitor movement, and
window close on NVIDIA, Mesa Intel, and Mesa AMD drivers when these systems are
available.

## Rollout {#wayland-reconfiguration-rollout}

Implement the change in stages:

1. Add reason and duration telemetry to all surface configuration calls.
2. Use `Suboptimal` frames and request deferred configuration.
3. Add the device coordinator and the surface worker for Wayland runtime
   configuration.
4. Route resize and transparency changes through the worker.
5. Add correct `Lost` surface recreation.
6. Route surface replacement and runtime device recovery through the same
   coordinator.
7. Run multiwindow and delayed-driver tests.
8. Evaluate a capability cache only after the worker is stable.

The worker path can use a Linux Wayland compile-time boundary during initial
rollout. After validation, shared platform interfaces can move into
`gpui_wgpu` without changing other platform behavior.

## Acceptance Criteria {#wayland-reconfiguration-acceptance}

The design is complete when all these statements are true:

- No runtime Wayland surface configuration runs on the main thread.
- No unlimited device wait runs on the main thread during resize.
- A delayed Vulkan capability query does not stop input or event dispatch.
- No queue submission races with surface configuration on the same device.
- Rapid resize applies the latest requested size.
- `Suboptimal` frames remain usable until recovery starts.
- `Lost` recreates the surface.
- Worker completion requests a new frame.
- Window close does not destroy native state that an active worker uses.
- Shutdown does not wait for a blocked driver call.
- Logs identify the request reason, generation, and configuration duration.

## Open Questions {#wayland-reconfiguration-open-questions}

- Which existing GPUI foreground channel should carry worker completion and
  wake calloop?
- Does any atlas or queue path submit work outside `WgpuRenderer::draw`?
- Should the coordinator be one worker per device or one worker per window with
  a shared device permit? One worker per device gives simpler serialization.
- Can the current Wayland window wrapper provide an owned lifetime lease for
  raw handles, or does it need a new lease type?
- Can wgpu error scopes return a configuration result on the worker without an
  additional main-thread poll?
- Should a worker that remains blocked for a long threshold disable rendering
  for all windows on that device, or should Flint offer a controlled device
  recovery action?
- Does the first implementation support only Vulkan, or does the same worker
  also handle the Wayland GLES backend?
