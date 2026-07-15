---
title: Privacy Policy
slug: privacy-policy
---

**Last Updated**: July 15, 2026

Flint is an open-source, community-maintained fork of Zed. There is no company behind it, no account system, and no telemetry backend of its own.

## What Flint Collects

Flint does not collect or transmit usage data, diagnostics, crash dumps, hang reports, or telemetry. It has no telemetry setting or upload endpoint. Logs and reliability artifacts are stored locally so you can inspect and debug them yourself; see [Local Diagnostics](../docs/src/telemetry.md).

## AI Features

AI features are off by default. If you enable them and configure a model provider (for example, with your own API key), your prompts and code context are sent directly to that provider under its own privacy policy. Flint itself does not see, store, or process that data.

## Extensions

Installing an extension fetches it from Zed's extension registry, since Flint does not run its own. That request is handled under [Zed's Privacy Policy](https://zed.dev/privacy-policy).

Checking for and downloading Flint updates uses GitHub Releases. Those requests are handled under [GitHub's Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

## Source Code

Flint does not transmit your source code anywhere on its own. Any transmission only happens as a direct result of features you explicitly use, such as Git operations against a remote you configure, or the AI features described above.

## Changes

This policy may change as the project evolves. Changes will be reflected on this page.

## Contact

Questions about this policy can be raised as an issue on the [project's GitHub repository](https://github.com/shenghsi/flint).
