# Flint Extensions

This directory contains extensions for Flint that are largely maintained by the Flint team. They currently live in the Flint repository for ease of maintenance.

If you are looking for the Flint extension registry, see the [`zed-industries/extensions`](https://github.com/zed-industries/extensions) repo.

## Structure

Currently, Flint includes support for a number of languages without requiring installing an extension. Those languages can be found under [`crates/languages/src`](https://github.com/zed-industries/flint/tree/main/crates/languages/src).

Support for all other languages is done via extensions. This directory ([extensions/](https://github.com/zed-industries/flint/tree/main/extensions/)) contains some of the officially maintained extensions. These extensions use the same [flint_extension_api](https://docs.rs/flint_extension_api/latest/flint_extension_api/) available to all [Flint Extensions](https://flint.dev/extensions) for providing [language servers](https://flint.dev/docs/extensions/languages#language-servers), [tree-sitter grammars](https://flint.dev/docs/extensions/languages#grammar) and [tree-sitter queries](https://flint.dev/docs/extensions/languages#tree-sitter-queries).

You can find the other officially maintained extensions in the [flint-extensions organization](https://github.com/flint-extensions).

## Dev Extensions

See the docs for [Developing an Extension Locally](https://flint.dev/docs/extensions/developing-extensions#developing-an-extension-locally) for how to work with one of these extensions.
