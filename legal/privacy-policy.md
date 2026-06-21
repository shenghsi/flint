---
title: Privacy Policy
slug: privacy-policy
---

**Last Updated**: June 21, 2026

Flint is an open-source, community-maintained fork of Zed. There is no company behind it, no account system, and no telemetry backend of its own.

## What Flint Collects

By default, Flint does not collect or transmit any usage data, diagnostics, or telemetry. This is controlled by the `telemetry.diagnostics` and `telemetry.metrics` settings, both of which are off by default.

If you turn telemetry on, anonymized diagnostics or usage metrics are sent to Zed's servers, since Flint does not run its own telemetry backend. That data is handled under [Zed's Privacy Policy](https://zed.dev/privacy-policy), not this one.

## AI Features

AI features are off by default. If you enable them and configure a model provider (for example, with your own API key), your prompts and code context are sent directly to that provider under its own privacy policy. Flint itself does not see, store, or process that data.

## Extensions

Installing an extension fetches it from Zed's extension registry, since Flint does not run its own. That request is handled under [Zed's Privacy Policy](https://zed.dev/privacy-policy).

## Source Code

Flint does not transmit your source code anywhere on its own. Any transmission only happens as a direct result of features you explicitly use, such as Git operations against a remote you configure, or the AI features described above.

## Changes

This policy may change as the project evolves. Changes will be reflected on this page.

## Contact

Questions about this policy can be raised as an issue on the [project's GitHub repository](https://github.com/shenghsi/flint).
