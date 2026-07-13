# macOS Document Icon Synchronization

## Purpose

Keep the Finder document icon bundled with Flint aligned with the stable application icon.

## Design

`script/generate-app-icons` will regenerate `crates/flint/resources/Document.icns` from the
stable 1024px PNG after producing the channel PNG and Windows ICO assets. It will create the
standard macOS iconset sizes (16px through 1024px) and convert that iconset to `Document.icns`.

The existing macOS bundle step will continue copying `Document.icns` to the application bundle.

## Verification

The generator will be run and the generated `Document.icns` will be extracted with `iconutil`.
Its 1024px representation will be compared with the stable 1024px source PNG.
