---
title: Subprocessor List
slug: subprocessors
---

Flint uses select third-party subprocessors to deliver core product functionality. Each subprocessor processes customer personal data only as necessary to provide its service, and all are subject to appropriate data protection agreements.

### How Flint Uses Subprocessors

To provide fast, reliable, and secure functionality, Flint relies on a small number of carefully vetted third-party subprocessors. These vendors help us deliver essential capabilities such as hosting, billing, analytics, real-time collaboration, and hosted AI features.

Each subprocessor only processes customer personal data as needed to provide its service.

Flint maintains contracts and data protection agreements with all subprocessors, including GDPR-compliant terms where applicable. We do not sell customer data, and we do not share customer personal data with vendors for advertising or marketing purposes.

### AI Subprocessors

Flint offers three modes for AI:

1. **Bring your own API key** — data goes directly from the customer to the model provider; Flint does not process or store it.
2. [**External Agents**](https://flint.dev/docs/ai/external-agents) — Flint uses ACP to provide an enhanced experience with terminal-based AI code agents like Claude Code or OpenAI Codex. Data is not processed or stored by Flint when using external agents.
3. **Flint-hosted models** — Flint sends customer prompts to one of its AI providers (listed below). These vendors act as subprocessors only for customers who choose this mode.

### Ongoing Updates

**Last Updated**: March 2, 2026

This subprocessor list is reviewed regularly. Flint will notify customers of material changes in accordance with our [Terms](https://flint.dev/terms) and [Privacy Policy](https://flint.dev/privacy-policy).

---

## Infrastructure & Hosting

| Subprocessor            | Purpose                                  | Data Location |
| ----------------------- | ---------------------------------------- | ------------- |
| **Cloudflare**          | Network services, Cloudflare Workers     | Global        |
| **Amazon Web Services** | Telemetry ingestion pipeline, S3 buckets | United States |
| **DigitalOcean**        | Application database hosting             | United States |
| **Vercel**              | Website and edge infrastructure hosting  | United States |

---

## Billing & Payments

| Subprocessor | Purpose                                                      | Data Location |
| ------------ | ------------------------------------------------------------ | ------------- |
| **Stripe**   | Payment processing                                           | United States |
| **Orb**      | Usage tracking, subscription management, and metered billing | United States |

---

## Operational Tools

| Subprocessor | Purpose                               | Data Location |
| ------------ | ------------------------------------- | ------------- |
| **Day.ai**   | Customer relationship management      | United States |
| **Linear**   | Issue tracking and project management | United States |

---

## Email & Communication

| Subprocessor   | Purpose                                                    | Data Location |
| -------------- | ---------------------------------------------------------- | ------------- |
| **ConvertKit** | Product update and feature announcement emails             | United States |
| **Loops**      | Email marketing and product communications                 | United States |
| **Plain**      | Consolidated platform for end-user support across channels | United States |

---

## Analytics & Data Processing

| Subprocessor         | Purpose                                                                                  | Data Location |
| -------------------- | ---------------------------------------------------------------------------------------- | ------------- |
| **Amplitude**        | Product analytics                                                                        | United States |
| **Axiom**            | Application telemetry, observability, and logs                                           | United States |
| **Fivetran**         | Automates data pipeline integration (extract, transformation, and load services) for Flint | United States |
| **Hex Technologies** | Analytics and debugging                                                                  | United States |
| **Snowflake**        | Data warehouse                                                                           | United States |

---

## Collaboration Services

| Subprocessor | Purpose                                                        | Data Location |
| ------------ | -------------------------------------------------------------- | ------------- |
| **LiveKit**  | Real-time audio/video and collaborative session infrastructure | United States |

---

## AI Services (Flint-Hosted Models)

_These subprocessors apply only when customers opt to use Flint's hosted AI models. When users supply their own API keys, or use external agents, data is sent directly to the provider and does not pass through Flint's infrastructure._

| Subprocessor        | Purpose                                                                                                                                                                                                          | Data Location |
| ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------- |
| **Anthropic**       | Requests may be sent to Anthropic even if you have another provider's model selected in chat (e.g. for summarization or generating git commit messages). We have a zero data retention agreement with Anthropic. | United States |
| **Baseten**         | Inference infrastructure for Edit Predictions                                                                                                                                                                    | United States |
| **Exa Labs**        | AI-powered contextual search and retrieval                                                                                                                                                                       | United States |
| **Google (Vertex)** | Requests may be sent to Google even if you have another provider's model selected in chat (e.g. for summarization). We have a zero data retention agreement with Google.                                         | United States |
| **OpenAI**          | Requests may be sent to OpenAI even if you have another provider's model selected in chat (e.g. for summarization or generating git commit messages). We have a zero data retention agreement with OpenAI.       | United States |
| **xAI**             | Requests may be sent to xAI even if you have another provider's model selected in chat (e.g. for summarization or generating git commit messages). We have a zero data retention agreement with xAI.             | United States |
