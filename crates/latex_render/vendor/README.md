# Bundled MathJax renderer

`mathjax-svg.js` is a self-contained Node.js bundle that renders LaTeX to SVG. It
is committed to the repository and embedded into the `latex_render` crate at
build time via `include_str!`, so the application never installs anything at
runtime — it only needs a Node.js binary to execute the bundle.

## Contents

- `src/render.js` — the driver: reads newline-delimited JSON render requests from
  stdin and writes newline-delimited JSON responses to stdout.
- `mathjax-svg.js` — the committed bundle (`src/render.js` + `mathjax-full`),
  produced by esbuild.
- `package.json` — pinned build tooling (`esbuild`, `mathjax-full`).

## Pinned versions

- `mathjax-full` 3.2.2
- `esbuild` 0.25.0

## Regenerating the bundle

Run from this directory:

```sh
npm install
npm run build
```

This rewrites `mathjax-svg.js`. Commit the regenerated bundle. `node_modules/`
and `package-lock.json` are intentionally not committed.
