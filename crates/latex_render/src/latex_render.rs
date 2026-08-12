//! Renders LaTeX math to SVG by driving a bundled, headless MathJax instance in
//! a warm Node.js subprocess.
//!
//! The MathJax bundle (`vendor/mathjax-svg.js`) is embedded at compile time and
//! written to a temp file on first use; the only runtime requirement is a
//! Node.js binary, obtained via [`NodeRuntime`]. A single long-lived process
//! serves all requests, multiplexed by a per-request id.
//!
//! [`LatexRenderer`] is registered as a GPUI global so that lower-level crates
//! (such as `markdown`) can reach it without depending on `node_runtime`. When
//! the global is absent, or Node.js is unavailable, callers should fall back to
//! showing the raw LaTeX source.

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{App, BackgroundExecutor, Global};
use node_runtime::NodeRuntime;
use serde::{Deserialize, Serialize};
use smol::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use smol::lock::Mutex;
use smol::process::{Command, Stdio};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const RENDER_SCRIPT: &str = include_str!("../vendor/mathjax-svg.js");

/// A single LaTeX equation rendered to SVG, sized in device-independent pixels.
#[derive(Clone, Debug)]
pub struct RenderedMath {
    /// The SVG document, with `width`/`height` already in pixels.
    pub svg: String,
    pub width: f32,
    pub height: f32,
    /// Baseline offset for inline placement. MathJax reports this negative
    /// (the box extends below the baseline by this much).
    pub vertical_align: f32,
}

/// Parameters for a single render.
pub struct RenderRequest<'a> {
    pub tex: &'a str,
    /// `true` for block/display math (`$$...$$`), `false` for inline (`$...$`).
    pub display: bool,
    /// CSS color (e.g. `#rrggbb`) the equation should be drawn in.
    pub color: Option<&'a str>,
    /// How many pixels one `ex` maps to, derived from the surrounding font size.
    pub ex_px: f32,
}

#[derive(Clone)]
pub struct LatexRenderer(Arc<LatexRendererState>);

struct GlobalLatexRenderer(LatexRenderer);

impl Global for GlobalLatexRenderer {}

struct LatexRendererState {
    node: NodeRuntime,
    executor: BackgroundExecutor,
    next_id: AtomicU64,
    process: Mutex<Option<Arc<Process>>>,
    script_path: Mutex<Option<PathBuf>>,
}

struct Process {
    stdin: Mutex<smol::process::ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<RenderedMath>>>>>,
    alive: Arc<AtomicBool>,
    _child: smol::process::Child,
    _reader: gpui::Task<()>,
}

impl LatexRenderer {
    pub fn new(node: NodeRuntime, cx: &App) -> Self {
        Self(Arc::new(LatexRendererState {
            node,
            executor: cx.background_executor().clone(),
            next_id: AtomicU64::new(0),
            process: Mutex::new(None),
            script_path: Mutex::new(None),
        }))
    }

    /// Registers the renderer as a GPUI global.
    pub fn init(node: NodeRuntime, cx: &mut App) {
        let renderer = Self::new(node, cx);
        cx.set_global(GlobalLatexRenderer(renderer));
    }

    pub fn try_global(cx: &App) -> Option<Self> {
        cx.try_global::<GlobalLatexRenderer>()
            .map(|global| global.0.clone())
    }

    pub async fn render(&self, request: RenderRequest<'_>) -> Result<RenderedMath> {
        let process = self.0.ensure_process().await?;
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);

        let (sender, receiver) = oneshot::channel();
        process.pending.lock().await.insert(id, sender);

        let payload = serde_json::to_string(&WireRequest {
            id,
            tex: request.tex,
            display: request.display,
            color: request.color,
            ex_px: request.ex_px,
        })?;

        {
            let mut stdin = process.stdin.lock().await;
            stdin.write_all(payload.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        receiver.await.context("latex render request was dropped")?
    }
}

impl LatexRendererState {
    async fn ensure_process(self: &Arc<Self>) -> Result<Arc<Process>> {
        let mut guard = self.process.lock().await;
        if let Some(process) = guard.as_ref()
            && process.alive.load(Ordering::SeqCst)
        {
            return Ok(process.clone());
        }
        let process = Arc::new(self.spawn_process().await?);
        *guard = Some(process.clone());
        Ok(process)
    }

    async fn spawn_process(self: &Arc<Self>) -> Result<Process> {
        let script_path = self.ensure_script().await?;
        let node_path = self
            .node
            .binary_path()
            .await
            .context("locating node binary for latex rendering")?;

        let mut command = Command::new(&node_path);
        command
            .arg(&script_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        #[cfg(target_os = "windows")]
        {
            use smol::process::windows::CommandExt as _;

            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command
            .spawn()
            .context("spawning node for latex rendering")?;

        let stdin = child.stdin.take().context("node child stdin missing")?;
        let stdout = child.stdout.take().context("node child stdout missing")?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<RenderedMath>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        let reader = self.executor.spawn({
            let pending = pending.clone();
            let alive = alive.clone();
            async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Some(line) = lines.next().await {
                    let Ok(line) = line else { break };
                    let Ok(response) = serde_json::from_str::<WireResponse>(&line) else {
                        continue;
                    };
                    let Some(id) = response.id else { continue };
                    if let Some(sender) = pending.lock().await.remove(&id) {
                        let _ = sender.send(response.into_result());
                    }
                }
                // The process ended; fail any in-flight requests so callers fall
                // back instead of hanging.
                alive.store(false, Ordering::SeqCst);
                for (_, sender) in pending.lock().await.drain() {
                    let _ = sender.send(Err(anyhow!("latex renderer process exited")));
                }
            }
        });

        Ok(Process {
            stdin: Mutex::new(stdin),
            pending,
            alive,
            _child: child,
            _reader: reader,
        })
    }

    async fn ensure_script(&self) -> Result<PathBuf> {
        let mut guard = self.script_path.lock().await;
        if let Some(path) = guard.as_ref()
            && smol::fs::metadata(path).await.is_ok()
        {
            return Ok(path.clone());
        }

        // Name the file by a content hash so a stale bundle from a previous
        // version is never reused, and concurrent processes share one file.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(RENDER_SCRIPT, &mut hasher);
        let hash = std::hash::Hasher::finish(&hasher);
        let path = std::env::temp_dir().join(format!("flint-mathjax-svg-{hash:x}.mjs"));

        if smol::fs::metadata(&path).await.is_err() {
            smol::fs::write(&path, RENDER_SCRIPT)
                .await
                .context("writing latex render script")?;
        }

        *guard = Some(path.clone());
        Ok(path)
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    id: u64,
    tex: &'a str,
    display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'a str>,
    #[serde(rename = "exPx")]
    ex_px: f32,
}

#[derive(Deserialize)]
struct WireResponse {
    id: Option<u64>,
    svg: Option<String>,
    #[serde(rename = "widthPx")]
    width_px: Option<f32>,
    #[serde(rename = "heightPx")]
    height_px: Option<f32>,
    #[serde(rename = "verticalAlignPx")]
    vertical_align_px: Option<f32>,
    error: Option<String>,
}

impl WireResponse {
    fn into_result(self) -> Result<RenderedMath> {
        if let Some(error) = self.error {
            return Err(anyhow!("{error}"));
        }
        Ok(RenderedMath {
            svg: self.svg.context("latex renderer returned no svg")?,
            width: self.width_px.unwrap_or(0.0),
            height: self.height_px.unwrap_or(0.0),
            vertical_align: self.vertical_align_px.unwrap_or(0.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wire_response_into_result_ok() {
        let response: WireResponse = serde_json::from_str(
            r#"{"id":1,"svg":"<svg></svg>","widthPx":40.5,"heightPx":12.0,"verticalAlignPx":-2.5}"#,
        )
        .unwrap();
        let rendered = response.into_result().unwrap();
        assert_eq!(rendered.svg, "<svg></svg>");
        assert_eq!(rendered.width, 40.5);
        assert_eq!(rendered.height, 12.0);
        assert_eq!(rendered.vertical_align, -2.5);
    }

    #[test]
    fn test_wire_response_into_result_error() {
        let response: WireResponse = serde_json::from_str(r#"{"id":2,"error":"bad tex"}"#).unwrap();
        assert!(response.into_result().is_err());
    }

    // A real MathJax SVG (the rendering of `x^2`, colored red) as produced by the
    // bundled driver. This guards against the rasterizer choking on MathJax's
    // path/transform constructs, and confirms the substituted color survives
    // rasterization.
    const SAMPLE_MATHJAX_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="18.256" height="15.296" viewBox="0 -833.9 1008.6 844.9"><g stroke="#ff0000" fill="#ff0000" stroke-width="0" transform="scale(1,-1)"><g data-mml-node="math"><g data-mml-node="msup"><g data-mml-node="mi"><path d="M52 289Q59 331 106 386T222 442Q257 442 286 424T329 379Q371 442 430 442Q467 442 494 420T522 361Q522 332 508 314T481 292T458 288Q439 288 427 299T415 328Q415 374 465 391Q454 404 425 404Q412 404 406 402Q368 386 350 336Q290 115 290 78Q290 50 306 38T341 26Q378 26 414 59T463 140Q466 150 469 151T485 153H489Q504 153 504 145Q504 144 502 134Q486 77 440 33T333 -11Q263 -11 227 52Q186 -10 133 -10H127Q78 -10 57 16T35 71Q35 103 54 123T99 143Q142 143 142 101Q142 81 130 66T107 46T94 41L91 40Q91 39 97 36T113 29T132 26Q168 26 194 71Q203 87 217 139T245 247T261 313Q266 340 266 352Q266 380 251 392T217 404Q177 404 142 372T93 290Q91 281 88 280T72 278H58Q52 284 52 289Z"></path></g><g data-mml-node="mn" transform="translate(605,363) scale(0.707)"><path d="M109 429Q82 429 66 447T50 491Q50 562 103 614T235 666Q326 666 387 610T449 465Q449 422 429 383T381 315T301 241Q265 210 201 149L142 93L218 92Q375 92 385 97Q392 99 409 186V189H449V186Q448 183 436 95T421 3V0H50V19V31Q50 38 56 46T86 81Q115 113 136 137Q145 147 170 174T204 211T233 244T261 278T284 308T305 340T320 369T333 401T340 431T343 464Q343 527 309 573T212 619Q179 619 154 602T119 569T109 550Q109 549 114 549Q132 549 151 535T170 489Q170 464 154 447T109 429Z"></path></g></g></g></g></svg>"##;

    #[gpui::test]
    fn test_mathjax_svg_rasterizes_with_color(cx: &mut gpui::TestAppContext) {
        let image = cx.update(|cx| {
            cx.svg_renderer()
                .render_single_frame(SAMPLE_MATHJAX_SVG.as_bytes(), 1.0)
                .expect("MathJax SVG should rasterize")
        });

        let size = image.size(0);
        assert!(
            size.width.0 > 10 && size.height.0 > 10,
            "rasterized math should have real dimensions, got {size:?}"
        );

        // Pixels are BGRA; confirm some opaque, clearly-red pixel exists, proving
        // the substituted color rendered.
        let bytes = image.as_bytes(0).expect("image should have a frame");
        let has_red = bytes.chunks_exact(4).any(|pixel| {
            let [blue, green, red, alpha] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            alpha > 128 && red > 150 && green < 100 && blue < 100
        });
        assert!(has_red, "expected red pixels from the substituted color");
    }
}
