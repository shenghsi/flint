// Headless MathJax tex -> SVG driver.
//
// Reads newline-delimited JSON requests from stdin and writes newline-delimited
// JSON responses to stdout, keeping the process warm so repeated renders are
// cheap. This file is bundled (together with mathjax-full) into
// `vendor/mathjax-svg.js` by esbuild; see vendor/README.md for regeneration.
//
// Request:  {"id": <number>, "tex": "<latex>", "display": <bool>, "color": "#rrggbb", "exPx": <number>}
// Response: {"id": <number>, "svg": "<svg...>", "widthPx": <number>, "heightPx": <number>, "verticalAlignPx": <number>}
//           or {"id": <number>, "error": "<msg>"}
//
// MathJax emits SVG sized in `ex` units; `exPx` is how many device-independent
// pixels one `ex` should map to (derived from the surrounding font size). The
// driver rewrites the SVG to absolute pixels so the rasterizer needs no font
// context, and returns the baseline offset for inline placement.

import { mathjax } from "mathjax-full/js/mathjax.js";
import { TeX } from "mathjax-full/js/input/tex.js";
import { SVG } from "mathjax-full/js/output/svg.js";
import { liteAdaptor } from "mathjax-full/js/adaptors/liteAdaptor.js";
import { RegisterHTMLHandler } from "mathjax-full/js/handlers/html.js";
import { AllPackages } from "mathjax-full/js/input/tex/AllPackages.js";
import * as readline from "node:readline";

const adaptor = liteAdaptor();
RegisterHTMLHandler(adaptor);

const tex = new TeX({ packages: AllPackages });
// `fontCache: "none"` keeps each SVG self-contained (no cross-document <defs>
// references), which matters because each equation is rasterized in isolation.
const svg = new SVG({ fontCache: "none" });
const doc = mathjax.document("", { InputJax: tex, OutputJax: svg });

function renderRequest(request) {
  const node = doc.convert(request.tex ?? "", { display: !!request.display });
  const svgNode = adaptor.firstChild(node) ?? node;

  const exPx = Number(request.exPx) > 0 ? Number(request.exPx) : 8;
  const widthEx = parseFloat(adaptor.getAttribute(svgNode, "width")) || 0;
  const heightEx = parseFloat(adaptor.getAttribute(svgNode, "height")) || 0;
  const style = adaptor.getAttribute(svgNode, "style") || "";
  const verticalAlignMatch = style.match(/vertical-align:\s*(-?[\d.]+)ex/);
  const verticalAlignEx = verticalAlignMatch ? parseFloat(verticalAlignMatch[1]) : 0;

  const widthPx = widthEx * exPx;
  const heightPx = heightEx * exPx;
  const verticalAlignPx = verticalAlignEx * exPx;

  adaptor.setAttribute(svgNode, "width", String(widthPx));
  adaptor.setAttribute(svgNode, "height", String(heightPx));

  let svg = adaptor.outerHTML(svgNode);
  if (request.color) {
    // MathJax draws with `currentColor`. Substituting the concrete color makes
    // the SVG self-coloring, so the rasterizer needs no `color` context (some
    // rasterizers do not resolve `currentColor`).
    svg = svg.split("currentColor").join(request.color);
  }

  return {
    svg,
    widthPx,
    heightPx,
    verticalAlignPx,
  };
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (rawLine) => {
  const line = rawLine.trim();
  if (!line) return;

  let request;
  try {
    request = JSON.parse(line);
  } catch (error) {
    process.stdout.write(JSON.stringify({ error: "invalid request json" }) + "\n");
    return;
  }

  try {
    const rendered = renderRequest(request);
    process.stdout.write(
      JSON.stringify({
        id: request.id,
        svg: rendered.svg,
        widthPx: rendered.widthPx,
        heightPx: rendered.heightPx,
        verticalAlignPx: rendered.verticalAlignPx,
      }) + "\n",
    );
  } catch (error) {
    const message = (error && error.message) || String(error);
    process.stdout.write(JSON.stringify({ id: request.id, error: message }) + "\n");
  }
});
