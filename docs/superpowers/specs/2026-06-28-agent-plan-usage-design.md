# Agent plan usage in the Agent Threads panel

## Summary

Show the active Codex and Claude Code plan usage beside each corresponding
heading in the Agent Threads panel:

```text
Codex  5H:24% W:61%
Claude 5H:8%  W:42%
```

Official Codex and Claude Code subscriptions are supported, along with the
known third-party coding plans from Kimi, Zhipu GLM, MiniMax, ZenMux, and
Volcengine. One Agent Threads setting controls the feature and defaults to
enabled.

## Goals

- Display percentage used, not percentage remaining, for the five-hour and
  weekly windows.
- Work for local Flint installations on Windows, Linux, and macOS.
- Query official Codex and Claude Code subscriptions as well as the five known
  third-party provider families.
- Stop all usage-query work when the setting is disabled.
- Keep usage failures isolated from thread launching and history scanning.

## Non-goals

- Custom usage scripts or arbitrary third-party provider APIs.
- Balance, token count, monthly quota, reset countdown, or overage display.
- Login, OAuth refresh, or credential management. Users authenticate with the
  provider's CLI as they do today.
- Remote-project credential lookup. The first version reads the local CLI
  configuration used by Flint's local terminal threads.

## Settings

Add `agent_threads.show_plan_usage`, a boolean with a default of `true`, to
`AgentThreadSettingsContent`, `AgentThreadSettings`, and
`assets/settings/default.json`.

Expose it in the Agent Threads section of Settings UI as **Show Plan Usage**:

> Show five-hour and weekly plan usage beside Codex and Claude headings.

Changing the value takes effect without restarting Flint. Turning it off:

1. drops the polling task, cancelling an in-flight request where cancellation
   is supported by the HTTP future;
2. prevents activation and timer callbacks from starting another request; and
3. clears cached values so neither heading renders usage.

Turning it on while the panel is active starts an immediate refresh.

## Usage model

`plan_usage.rs` owns the small provider-neutral model:

```rust
struct PlanUsage {
    five_hour_percent: Option<u8>,
    weekly_percent: Option<u8>,
}
```

Provider responses are rounded to the nearest whole percent and clamped to
`0..=100` at the model boundary. A response with only one supported window is
valid; the renderer shows only the available label.

No access token or API key is retained in `PlanUsage`, panel state, logs, or
error strings.

## Provider and credential resolution

Resolution begins with the effective launch configuration for each section:
the `AgentLaunchCommand.env` overrides, then the matching CLI configuration
directory (`CODEX_HOME` / `CLAUDE_CONFIG_DIR`, falling back to `~/.codex` /
`~/.claude`).

For Claude, `ANTHROPIC_BASE_URL` and the first non-empty value among
`ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_API_KEY` identify a third-party
provider. For Codex, the selected model provider's `base_url` and API key are
read from `config.toml` and `auth.json`, with launch-environment overrides
taking precedence where present.

An absent official base URL selects the official provider. A non-official URL
must match one of the known hosts below; otherwise usage is unavailable and no
request is sent.

Official OAuth credentials are read from the CLI credential file on all three
operating systems. On macOS, the existing generic-password entries
`Claude Code-credentials` and `Codex Auth` are tried first because current CLI
versions may store credentials there instead of in files. Missing, malformed,
API-key-only, or expired OAuth credentials result in no displayed usage.

Volcengine is the one provider whose control-plane quota API cannot use its
inference key. It reads `VOLCENGINE_ACCESS_KEY_ID` and
`VOLCENGINE_SECRET_ACCESS_KEY` from the corresponding agent launch
environment. Without both values, Volcengine usage remains hidden.

## Query adapters

All adapters use Flint's existing HTTP client with a 15-second timeout. They
return `PlanUsage` and do not expose provider response types to the panel.

### Official Claude Code

- Read `claudeAiOauth.accessToken` (also accepting the legacy
  `claude.ai_oauth` key).
- `GET https://api.anthropic.com/api/oauth/usage` with bearer authorization
  and `anthropic-beta: oauth-2025-04-20`.
- Map `five_hour.utilization` and `seven_day.utilization`.

### Official Codex

- Require ChatGPT OAuth mode and read the access token and optional account ID.
- `GET https://chatgpt.com/backend-api/wham/usage` with bearer authorization,
  `User-Agent: codex-cli`, and `ChatGPT-Account-Id` when available.
- Map `primary_window` and `secondary_window` by
  `limit_window_seconds`: 18,000 is five hours and 604,800 is one week.

### Kimi

- Detect `api.kimi.com/coding`.
- `GET https://api.kimi.com/coding/v1/usages` with bearer authorization.
- Convert `limits[].detail` to five-hour usage and `usage` to weekly usage by
  calculating `(limit - remaining) / limit`.

### Zhipu GLM

- Detect `open.bigmodel.cn`, `bigmodel.cn`, or `api.z.ai` and keep the matching
  regional host.
- `GET /api/monitor/usage/quota/limit` with the raw API key in
  `Authorization` (no `Bearer` prefix).
- From `TOKENS_LIMIT` entries, map `unit == 3` to five hours and `unit == 6`
  to weekly. If `unit` is absent, fill the missing windows using reset-time
  ordering, treating an entry without a reset as five-hour first.

### MiniMax

- Detect `api.minimaxi.com` or `api.minimax.io`.
- `GET /v1/api/openplatform/coding_plan/remains` with bearer authorization.
- Select `model_name == "general"`. Convert remaining percentages to used
  percentages. Include weekly usage only when `current_weekly_status == 1`.

### ZenMux

- Detect a base URL whose host contains `zenmux`.
- Query the configured quota URL with bearer authorization.
- Map `data.quota_5_hour` and `data.quota_7_day`, converting fractional
  `usage_percentage` values to percentages.

### Volcengine

- Detect `volces.com/api/coding`.
- Query the Ark control-plane endpoint at `open.volcengineapi.com`, deriving
  the region from the configured inference URL.
- Sign requests with Volcengine Signature V4 using the separate AccessKey ID
  and secret. Probe Agent Plan (`GetAFPUsage`) first, then Coding Plan
  (`GetCodingPlanUsage`) when the account is not subscribed to Agent Plan.
- Map the returned five-hour and weekly tiers; ignore monthly tiers.

## Panel lifecycle and data flow

`AgentThreadsPanel` holds one optional polling task and one optional
`PlanUsage` per visible kind. "Active" means the dock has called
`Panel::set_active(true)` because the Agent Threads panel is the deployed,
visible panel. Merely constructing the panel, keeping its dock closed, or
showing a different panel in that dock does not count. When active and the
setting is enabled, the task immediately performs one query for each agent
kind concurrently on the background executor and updates panel state on the
foreground executor. After that first query completes, it waits five minutes
using the GPUI background-executor timer, queries again, and repeats while the
panel remains active.

Each successful provider response replaces that kind's value. A later
transport, authentication, or parse failure retains the last successful value
for the current polling task. The initial failure has no label. Changing the
setting off or changing effective agent configuration drops the task and
clears the associated values so data from an old account or provider is never
shown.

Only the panel's task owns the polling future. `Panel::set_active(false)` drops
the task immediately, cancelling either its timer or its in-flight request;
reactivating the panel performs a fresh query. There is no process-global
service, background timer, or usage request while the panel is inactive.

## Rendering

The labels appear after the existing thread count in each section heading.
Five-hour and weekly values are separate `XSmall` labels so each can have its
own color:

- `0..=19`: theme success green
- `20..=39`: a theme-derived blend between success and warning
- `40..=59`: theme warning yellow
- `60..=79`: a theme-derived blend between warning and error
- `80..=100`: theme error red

Using theme colors and blends preserves contrast across light, dark, and custom
themes. Missing windows are omitted rather than rendered as zero.

## Error handling

- Credential and configuration reads are fallible and never panic.
- HTTP status, response parsing, and signing failures return errors to the
  polling task. They are logged without response bodies or credentials and do
  not affect agent threads.
- `401` and `403` are treated as unavailable credentials until the next
  scheduled refresh or configuration change.
- Provider response fields are optional; malformed or absent windows are
  omitted.

## Testing

Use test-first changes in the existing crate tests plus focused tests in
`plan_usage.rs`:

- settings content defaults `show_plan_usage` to true and Settings UI writes
  the field;
- provider detection selects official, each known third party, or unsupported;
- credential parsing covers Claude and Codex file formats without including
  live secrets;
- fixture responses normalize five-hour and weekly usage for all seven query
  adapters, including GLM window classification and MiniMax remaining-to-used
  conversion;
- percentage normalization covers rounding and clamping;
- color selection covers all five boundaries;
- disabling the setting drops the polling task, clears values, and prevents a
  refresh from being scheduled;
- panel rendering includes both labels when available and omits unavailable
  windows.

Run the focused `agent_threads` and `settings_ui` tests, then `./script/clippy`
for the affected workspace crates. Build `/tmp/Flint-Local.app` with
`./script/bundle-tmp-app` for the final manual verification.

## Files

- `crates/agent_threads/src/plan_usage.rs` — resolution, adapters, parsing,
  normalization, and color bands.
- `crates/agent_threads/src/agent_threads.rs` — module registration and runtime
  setting.
- `crates/agent_threads/src/panel.rs` — polling lifecycle and heading labels.
- `crates/agent_threads/Cargo.toml` — reuse the existing workspace HTTP client.
- `crates/settings_content/src/agent_threads.rs` — serialized setting.
- `crates/settings_ui/src/page_data.rs` — Settings UI toggle.
- `assets/settings/default.json` — enabled default.
