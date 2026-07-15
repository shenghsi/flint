---
title: Feedback and Training Data
description: Understand Flint's AI feedback and training-data boundaries.
---

# Feedback and Training Data

Flint has no AI feedback, ratings, telemetry, or training-data service. It does not upload conversations, prompts, responses, code context, or edit-acceptance data to Flint or Zed.

Agent Threads use the authentication and provider configuration owned by supported third-party tools such as Codex CLI and Claude Code. Those tools and their model providers process data under their own settings, terms, and privacy policies. Review the provider configuration before using an agent with sensitive code.

Local models and MCP servers follow the configuration of the local or self-hosted service. Flint does not add a separate collection path.

For the complete boundary, see [AI Privacy](./privacy-and-security.md) and [Local Diagnostics](../telemetry.md).
