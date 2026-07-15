---
title: AI Privacy
description: Understand Flint's provider-owned AI and authentication boundaries.
---

# AI Privacy

Flint has no account system, hosted model proxy, AI telemetry service, or conversation store. It does not receive or retain prompts, responses, or code context.

## Agent Threads

Agent Threads can use supported third-party tools such as Codex CLI and Claude Code. Flint reuses their existing login state rather than creating a Flint account. It may read provider credentials from the locations those tools use and contact the provider's usage API to display plan usage. Prompts, code context, tool calls, model routing, and retention are governed by the selected tool and provider.

Signing out or changing provider authentication is done through the provider's CLI. Flint does not upload those credentials to Flint or Zed.

## Local and External Services

Local models, external agents, extensions, and MCP servers can read or transmit data according to their own configuration and permissions. Review each service before granting access to a worktree or running tools.

## Diagnostics

Flint stores logs, crash artifacts, hang traces, and input-latency reports locally. It does not upload them. See [Local Diagnostics](../telemetry.md) for locations and self-debugging instructions.

Flint also has no AI feedback, ratings, or training-data upload path. See [Feedback and Training Data](./ai-improvement.md).
