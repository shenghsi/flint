use anyhow::Context as _;
use collections::HashMap;
use futures::AsyncReadExt as _;
use hmac::{Hmac, Mac as _};
use http_client::{AsyncBody, HttpClient, Request};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{path::PathBuf, sync::Arc};

use crate::AgentLaunchCommand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanProvider {
    Official,
    Kimi,
    Zhipu,
    MiniMax,
    ZenMux,
    Volcengine,
    Unsupported,
}

impl PlanProvider {
    pub(crate) fn from_base_url(base_url: Option<&str>) -> Self {
        let Some(base_url) = base_url.filter(|url| !url.trim().is_empty()) else {
            return Self::Official;
        };
        let base_url = base_url.to_ascii_lowercase();
        if base_url.contains("api.kimi.com/coding") {
            Self::Kimi
        } else if base_url.contains("bigmodel.cn") || base_url.contains("api.z.ai") {
            Self::Zhipu
        } else if base_url.contains("api.minimaxi.com") || base_url.contains("api.minimax.io") {
            Self::MiniMax
        } else if base_url.contains("zenmux") {
            Self::ZenMux
        } else if base_url.contains("volces.com/api/coding") {
            Self::Volcengine
        } else {
            Self::Unsupported
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UsagePercent(u8);

impl UsagePercent {
    pub(crate) fn new(value: f64) -> Self {
        let value = if value.is_finite() { value } else { 0.0 };
        Self(value.round().clamp(0.0, 100.0) as u8)
    }

    pub(crate) fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageColorBand {
    Green,
    LightGreen,
    Yellow,
    Orange,
    Red,
}

impl UsageColorBand {
    pub(crate) fn for_percent(percent: u8) -> Self {
        match percent {
            0..=19 => Self::Green,
            20..=39 => Self::LightGreen,
            40..=59 => Self::Yellow,
            60..=79 => Self::Orange,
            _ => Self::Red,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanUsage {
    pub(crate) five_hour_percent: Option<UsagePercent>,
    pub(crate) five_hour_reset_at: Option<i64>,
    pub(crate) weekly_percent: Option<UsagePercent>,
    pub(crate) weekly_reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct UsageWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ClaudeUsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

fn parse_claude_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: ClaudeUsageResponse = serde_json::from_slice(body)?;
    let window_parts = |w: Option<UsageWindow>| -> (Option<UsagePercent>, Option<i64>) {
        let Some(w) = w else { return (None, None) };
        let percent = w.utilization.map(UsagePercent::new);
        let reset_at = w
            .resets_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.timestamp());
        (percent, reset_at)
    };
    let (five_hour_percent, five_hour_reset_at) = window_parts(response.five_hour);
    let (weekly_percent, weekly_reset_at) = window_parts(response.seven_day);
    Ok(PlanUsage {
        five_hour_percent,
        five_hour_reset_at,
        weekly_percent,
        weekly_reset_at,
    })
}

#[derive(Deserialize)]
struct CodexUsageWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<u64>,
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct CodexRateLimit {
    primary_window: Option<CodexUsageWindow>,
    secondary_window: Option<CodexUsageWindow>,
}

#[derive(Deserialize)]
struct CodexUsageResponse {
    rate_limit: Option<CodexRateLimit>,
}

fn parse_codex_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: CodexUsageResponse = serde_json::from_slice(body)?;
    let mut usage = PlanUsage::default();
    let Some(rate_limit) = response.rate_limit else {
        return Ok(usage);
    };
    for window in [rate_limit.primary_window, rate_limit.secondary_window]
        .into_iter()
        .flatten()
    {
        let Some(percent) = window.used_percent.map(UsagePercent::new) else {
            continue;
        };
        match window.limit_window_seconds {
            Some(18_000) => {
                usage.five_hour_percent = Some(percent);
                usage.five_hour_reset_at = window.reset_at;
            }
            Some(604_800) => {
                usage.weekly_percent = Some(percent);
                usage.weekly_reset_at = window.reset_at;
            }
            _ => {}
        }
    }
    Ok(usage)
}

#[derive(Deserialize)]
struct KimiQuota {
    limit: f64,
    remaining: f64,
}

#[derive(Deserialize)]
struct KimiLimit {
    detail: KimiQuota,
}

#[derive(Deserialize)]
struct KimiUsageResponse {
    #[serde(default)]
    limits: Vec<KimiLimit>,
    usage: Option<KimiQuota>,
}

fn used_percent(limit: f64, remaining: f64) -> UsagePercent {
    UsagePercent::new(if limit > 0.0 {
        (limit - remaining) / limit * 100.0
    } else {
        0.0
    })
}

fn parse_kimi_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: KimiUsageResponse = serde_json::from_slice(body)?;
    Ok(PlanUsage {
        five_hour_percent: response
            .limits
            .first()
            .map(|limit| used_percent(limit.detail.limit, limit.detail.remaining)),
        weekly_percent: response
            .usage
            .map(|usage| used_percent(usage.limit, usage.remaining)),
        ..PlanUsage::default()
    })
}

fn parse_zhipu_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: serde_json::Value = serde_json::from_slice(body)?;
    let limits = response
        .pointer("/data/limits")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    let mut usage = PlanUsage::default();
    let mut unclassified = Vec::new();
    for limit in limits {
        if !limit
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("TOKENS_LIMIT"))
        {
            continue;
        }
        let Some(percent) = limit
            .get("percentage")
            .and_then(serde_json::Value::as_f64)
            .map(UsagePercent::new)
        else {
            continue;
        };
        match limit.get("unit").and_then(serde_json::Value::as_u64) {
            Some(3) => usage.five_hour_percent = Some(percent),
            Some(6) => usage.weekly_percent = Some(percent),
            _ => unclassified.push((
                limit
                    .get("nextResetTime")
                    .and_then(serde_json::Value::as_i64),
                percent,
            )),
        }
    }
    unclassified.sort_by_key(|(reset, _)| (reset.is_some(), reset.unwrap_or(i64::MIN)));
    for (_, percent) in unclassified {
        if usage.five_hour_percent.is_none() {
            usage.five_hour_percent = Some(percent);
        } else if usage.weekly_percent.is_none() {
            usage.weekly_percent = Some(percent);
        }
    }
    Ok(usage)
}

fn parse_minimax_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: serde_json::Value = serde_json::from_slice(body)?;
    let general = response
        .get("model_remains")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("model_name").and_then(serde_json::Value::as_str) == Some("general")
            })
        });
    let Some(general) = general else {
        return Ok(PlanUsage::default());
    };
    let five_hour_percent = general
        .get("current_interval_remaining_percent")
        .and_then(serde_json::Value::as_f64)
        .map(|remaining| UsagePercent::new(100.0 - remaining));
    let weekly_percent = (general
        .get("current_weekly_status")
        .and_then(serde_json::Value::as_u64)
        == Some(1))
    .then(|| {
        general
            .get("current_weekly_remaining_percent")
            .and_then(serde_json::Value::as_f64)
            .map(|remaining| UsagePercent::new(100.0 - remaining))
    })
    .flatten();
    Ok(PlanUsage {
        five_hour_percent,
        weekly_percent,
        ..PlanUsage::default()
    })
}

fn parse_zenmux_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: serde_json::Value = serde_json::from_slice(body)?;
    let percent = |pointer: &str| {
        response
            .pointer(pointer)
            .and_then(serde_json::Value::as_f64)
            .map(|fraction| UsagePercent::new(fraction * 100.0))
    };
    Ok(PlanUsage {
        five_hour_percent: percent("/data/quota_5_hour/usage_percentage"),
        weekly_percent: percent("/data/quota_7_day/usage_percentage"),
        ..PlanUsage::default()
    })
}

fn parse_volcengine_agent_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: serde_json::Value = serde_json::from_slice(body)?;
    let result = response.get("Result").unwrap_or(&response);
    let percent = |window: &str| {
        let quota = result.pointer(&format!("/{window}/Quota"))?.as_f64()?;
        let used = result.pointer(&format!("/{window}/Used"))?.as_f64()?;
        (quota > 0.0).then(|| UsagePercent::new(used / quota * 100.0))
    };
    Ok(PlanUsage {
        five_hour_percent: percent("AFPFiveHour"),
        weekly_percent: percent("AFPWeekly"),
        ..PlanUsage::default()
    })
}

pub(crate) struct OAuthCredentials {
    access_token: String,
    account_id: Option<String>,
}

fn parse_claude_credentials(body: &[u8]) -> anyhow::Result<OAuthCredentials> {
    let document: serde_json::Value = serde_json::from_slice(body)?;
    let oauth = document
        .get("claudeAiOauth")
        .or_else(|| document.get("claude.ai_oauth"))
        .context("Claude OAuth credentials are missing")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .context("Claude OAuth access token is missing")?;
    Ok(OAuthCredentials {
        access_token: access_token.to_string(),
        account_id: None,
    })
}

fn parse_codex_credentials(body: &[u8]) -> anyhow::Result<OAuthCredentials> {
    let document: serde_json::Value = serde_json::from_slice(body)?;
    let tokens = document
        .get("tokens")
        .context("Codex OAuth credentials are missing")?;
    let access_token = tokens
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .context("Codex OAuth access token is missing")?;
    Ok(OAuthCredentials {
        access_token: access_token.to_string(),
        account_id: tokens
            .get("account_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

struct ProviderCredentials {
    base_url: String,
    api_key: String,
}

fn parse_codex_provider(
    body: &[u8],
    environment: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<ProviderCredentials> {
    let document: toml::Value = std::str::from_utf8(body)?.parse()?;
    let provider_name = document
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .context("Codex model_provider is missing")?;
    let provider = document
        .get("model_providers")
        .and_then(|providers| providers.get(provider_name))
        .context("Codex model provider configuration is missing")?;
    let base_url = provider
        .get("base_url")
        .and_then(toml::Value::as_str)
        .context("Codex provider base_url is missing")?;
    let environment_key = provider
        .get("env_key")
        .and_then(toml::Value::as_str)
        .unwrap_or("OPENAI_API_KEY");
    let api_key = environment(environment_key).context("Codex provider API key is missing")?;
    Ok(ProviderCredentials {
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
    })
}

pub(crate) async fn query_plan_usage(
    kind_id: &str,
    command: &AgentLaunchCommand,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<PlanUsage> {
    match kind_id {
        "claude" => query_claude(command, http_client).await,
        "codex" => query_codex(command, http_client).await,
        _ => anyhow::bail!("unsupported agent kind"),
    }
}

async fn query_claude(
    command: &AgentLaunchCommand,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<PlanUsage> {
    let home = agent_home(command, "CLAUDE_CONFIG_DIR", ".claude");
    let file_environment = read_json_environment(home.join("settings.json"));
    let environment = |key: &str| effective_environment(&command.env, &file_environment, key);
    let base_url = environment("ANTHROPIC_BASE_URL");
    match PlanProvider::from_base_url(base_url.as_deref()) {
        PlanProvider::Official => {
            let credentials = read_claude_oauth(&home).await?;
            let body = get_json(
                http_client,
                "https://api.anthropic.com/api/oauth/usage",
                &[
                    (
                        "Authorization",
                        format!("Bearer {}", credentials.access_token),
                    ),
                    ("anthropic-beta", "oauth-2025-04-20".to_string()),
                ],
            )
            .await?;
            parse_claude_usage(&body)
        }
        provider => {
            let api_key = environment("ANTHROPIC_AUTH_TOKEN")
                .or_else(|| environment("ANTHROPIC_API_KEY"))
                .context("provider API key is missing")?;
            query_third_party(
                provider,
                base_url.context("provider base URL is missing")?,
                api_key,
                command,
                http_client,
            )
            .await
        }
    }
}

async fn query_codex(
    command: &AgentLaunchCommand,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<PlanUsage> {
    let home = agent_home(command, "CODEX_HOME", ".codex");
    let auth = std::fs::read(home.join("auth.json")).ok();
    let config = std::fs::read(home.join("config.toml")).ok();
    if let Some(config) = config {
        let auth_api_key = auth.as_deref().and_then(codex_api_key);
        if let Ok(provider) = parse_codex_provider(&config, |key| {
            command
                .env
                .get(key)
                .cloned()
                .or_else(|| std::env::var(key).ok())
                .or_else(|| auth_api_key.clone())
        }) {
            let provider_kind = PlanProvider::from_base_url(Some(&provider.base_url));
            if provider_kind != PlanProvider::Unsupported {
                return query_third_party(
                    provider_kind,
                    provider.base_url,
                    provider.api_key,
                    command,
                    http_client,
                )
                .await;
            }
        }
    }
    let credentials = read_codex_oauth(&home, auth.as_deref()).await?;
    let mut headers = vec![
        (
            "Authorization",
            format!("Bearer {}", credentials.access_token),
        ),
        ("User-Agent", "codex-cli".to_string()),
    ];
    if let Some(account_id) = credentials.account_id {
        headers.push(("ChatGPT-Account-Id", account_id));
    }
    let body = get_json(
        http_client,
        "https://chatgpt.com/backend-api/wham/usage",
        &headers,
    )
    .await?;
    parse_codex_usage(&body)
}

async fn query_third_party(
    provider: PlanProvider,
    base_url: String,
    api_key: String,
    command: &AgentLaunchCommand,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<PlanUsage> {
    let (url, authorization) = match provider {
        PlanProvider::Kimi => (
            "https://api.kimi.com/coding/v1/usages".to_string(),
            format!("Bearer {api_key}"),
        ),
        PlanProvider::Zhipu => (
            format!(
                "{}/api/monitor/usage/quota/limit",
                if base_url.to_ascii_lowercase().contains("bigmodel.cn") {
                    "https://open.bigmodel.cn"
                } else {
                    "https://api.z.ai"
                }
            ),
            api_key,
        ),
        PlanProvider::MiniMax => (
            format!(
                "https://{}/v1/api/openplatform/coding_plan/remains",
                if base_url.to_ascii_lowercase().contains("minimaxi.com") {
                    "api.minimaxi.com"
                } else {
                    "api.minimax.io"
                }
            ),
            format!("Bearer {api_key}"),
        ),
        PlanProvider::ZenMux => (base_url, format!("Bearer {api_key}")),
        PlanProvider::Volcengine => {
            return query_volcengine(command, &base_url, http_client).await;
        }
        PlanProvider::Official | PlanProvider::Unsupported => {
            anyhow::bail!("unsupported third-party provider")
        }
    };
    let body = get_json(http_client, &url, &[("Authorization", authorization)]).await?;
    match provider {
        PlanProvider::Kimi => parse_kimi_usage(&body),
        PlanProvider::Zhipu => parse_zhipu_usage(&body),
        PlanProvider::MiniMax => parse_minimax_usage(&body),
        PlanProvider::ZenMux => parse_zenmux_usage(&body),
        _ => anyhow::bail!("unsupported third-party response"),
    }
}

async fn get_json(
    http_client: Arc<dyn HttpClient>,
    url: &str,
    headers: &[(&str, String)],
) -> anyhow::Result<Vec<u8>> {
    let mut request = Request::get(url).header("Accept", "application/json");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = http_client
        .send(request.body(AsyncBody::default())?)
        .await?;
    anyhow::ensure!(response.status().is_success(), "usage request failed");
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    Ok(body)
}

fn agent_home(command: &AgentLaunchCommand, environment_key: &str, directory: &str) -> PathBuf {
    command
        .env
        .get(environment_key)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(environment_key).map(PathBuf::from))
        .unwrap_or_else(|| paths::home_dir().join(directory))
}

fn effective_environment(
    command_environment: &HashMap<String, String>,
    file_environment: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    command_environment
        .get(key)
        .cloned()
        .or_else(|| file_environment.get(key).cloned())
        .or_else(|| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty())
}

fn read_json_environment(path: PathBuf) -> HashMap<String, String> {
    std::fs::read(path)
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|document| document.get("env").cloned())
        .and_then(|environment| serde_json::from_value(environment).ok())
        .unwrap_or_default()
}

fn codex_api_key(body: &[u8]) -> Option<String> {
    let document: serde_json::Value = serde_json::from_slice(body).ok()?;
    document
        .get("OPENAI_API_KEY")
        .or_else(|| document.pointer("/auth/OPENAI_API_KEY"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

async fn read_claude_oauth(home: &std::path::Path) -> anyhow::Result<OAuthCredentials> {
    #[cfg(target_os = "macos")]
    if let Some(body) = read_macos_generic_password("Claude Code-credentials").await {
        if let Ok(credentials) = parse_claude_credentials(&body) {
            return Ok(credentials);
        }
    }
    parse_claude_credentials(&std::fs::read(home.join(".credentials.json"))?)
}

async fn read_codex_oauth(
    home: &std::path::Path,
    file_body: Option<&[u8]>,
) -> anyhow::Result<OAuthCredentials> {
    #[cfg(target_os = "macos")]
    if let Some(body) = read_macos_generic_password("Codex Auth").await {
        if let Ok(credentials) = parse_codex_credentials(&body) {
            return Ok(credentials);
        }
    }
    match file_body {
        Some(body) => parse_codex_credentials(body),
        None => parse_codex_credentials(&std::fs::read(home.join("auth.json"))?),
    }
}

#[cfg(target_os = "macos")]
async fn read_macos_generic_password(service: &str) -> Option<Vec<u8>> {
    let output = smol::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .await
        .ok()?;
    output.status.success().then_some(output.stdout)
}

struct VolcengineSignature {
    authorization: String,
    date: String,
    content_sha256: String,
}

fn sign_volcengine_request(
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    canonical_query: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<VolcengineSignature> {
    let date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let content_sha256 = hex::encode(Sha256::digest([]));
    let canonical_headers = format!(
        "host:open.volcengineapi.com\nx-date:{date}\nx-content-sha256:{content_sha256}\ncontent-type:application/json; charset=utf-8\n"
    );
    let signed_headers = "host;x-date;x-content-sha256;content-type";
    let canonical_request = format!(
        "POST\n/\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{content_sha256}"
    );
    let scope = format!("{short_date}/{region}/ark/request");
    let string_to_sign = format!(
        "HMAC-SHA256\n{date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let date_key = hmac_sha256(secret_access_key.as_bytes(), short_date.as_bytes())?;
    let region_key = hmac_sha256(&date_key, region.as_bytes())?;
    let service_key = hmac_sha256(&region_key, b"ark")?;
    let signing_key = hmac_sha256(&service_key, b"request")?;
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
    Ok(VolcengineSignature {
        authorization: format!(
            "HMAC-SHA256 Credential={access_key_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
        date,
        content_sha256,
    })
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut hmac = Hmac::<Sha256>::new_from_slice(key)?;
    hmac.update(value);
    Ok(hmac.finalize().into_bytes().to_vec())
}

async fn query_volcengine(
    command: &AgentLaunchCommand,
    base_url: &str,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<PlanUsage> {
    let access_key_id = command
        .env
        .get("VOLCENGINE_ACCESS_KEY_ID")
        .cloned()
        .or_else(|| std::env::var("VOLCENGINE_ACCESS_KEY_ID").ok())
        .context("Volcengine AccessKey ID is missing")?;
    let secret_access_key = command
        .env
        .get("VOLCENGINE_SECRET_ACCESS_KEY")
        .cloned()
        .or_else(|| std::env::var("VOLCENGINE_SECRET_ACCESS_KEY").ok())
        .context("Volcengine secret AccessKey is missing")?;
    let region = volcengine_region(base_url);
    let agent_usage = request_volcengine_usage(
        "GetAFPUsage",
        &region,
        &access_key_id,
        &secret_access_key,
        http_client.clone(),
    )
    .await
    .and_then(|body| parse_volcengine_agent_usage(&body));
    if let Ok(usage) = agent_usage {
        if usage.five_hour_percent.is_some() || usage.weekly_percent.is_some() {
            return Ok(usage);
        }
    }
    let body = request_volcengine_usage(
        "GetCodingPlanUsage",
        &region,
        &access_key_id,
        &secret_access_key,
        http_client,
    )
    .await?;
    parse_volcengine_coding_usage(&body)
}

fn volcengine_region(base_url: &str) -> String {
    base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or_default()
        .split('.')
        .find(|part| part.starts_with("cn-") || part.starts_with("ap-"))
        .unwrap_or("cn-beijing")
        .to_string()
}

async fn request_volcengine_usage(
    action: &str,
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<Vec<u8>> {
    let canonical_query = format!("Action={action}&Region={region}&Version=2024-01-01");
    let signature = sign_volcengine_request(
        access_key_id,
        secret_access_key,
        region,
        &canonical_query,
        chrono::Utc::now(),
    )?;
    let url = format!("https://open.volcengineapi.com/?{canonical_query}");
    let request = Request::post(url)
        .header("Authorization", signature.authorization)
        .header("X-Date", signature.date)
        .header("X-Content-Sha256", signature.content_sha256)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(AsyncBody::default())?;
    let mut response = http_client.send(request).await?;
    anyhow::ensure!(response.status().is_success(), "usage request failed");
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    Ok(body)
}

fn parse_volcengine_coding_usage(body: &[u8]) -> anyhow::Result<PlanUsage> {
    let response: serde_json::Value = serde_json::from_slice(body)?;
    let result = response.get("Result").unwrap_or(&response);
    let windows = result
        .get("QuotaUsage")
        .or_else(|| result.get("Usages"))
        .or_else(|| result.get("Details"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten();
    let mut usage = PlanUsage::default();
    for window in windows {
        let label = window
            .get("Level")
            .or_else(|| window.get("Type"))
            .or_else(|| window.get("Period"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let Some(percent) = window
            .get("Percent")
            .or_else(|| window.get("UsedPercent"))
            .or_else(|| window.get("UsagePercent"))
            .and_then(serde_json::Value::as_f64)
            .map(UsagePercent::new)
        else {
            continue;
        };
        match label.as_str() {
            "session" | "5h" | "fivehour" | "five_hour" | "rolling_5h" => {
                usage.five_hour_percent = Some(percent)
            }
            "weekly" | "week" | "7d" => usage.weekly_percent = Some(percent),
            _ => {}
        }
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_official_and_known_plan_providers() {
        assert_eq!(PlanProvider::from_base_url(None), PlanProvider::Official);
        assert_eq!(
            PlanProvider::from_base_url(Some("https://api.kimi.com/coding/v1")),
            PlanProvider::Kimi
        );
        assert_eq!(
            PlanProvider::from_base_url(Some("https://open.bigmodel.cn/api/coding/paas/v4")),
            PlanProvider::Zhipu
        );
        assert_eq!(
            PlanProvider::from_base_url(Some("https://api.minimax.io/anthropic")),
            PlanProvider::MiniMax
        );
        assert_eq!(
            PlanProvider::from_base_url(Some("https://api.zenmux.ai/usage")),
            PlanProvider::ZenMux
        );
        assert_eq!(
            PlanProvider::from_base_url(Some("https://ark.cn-beijing.volces.com/api/coding/v3")),
            PlanProvider::Volcengine
        );
        assert_eq!(
            PlanProvider::from_base_url(Some("https://example.com/v1")),
            PlanProvider::Unsupported
        );
    }

    #[test]
    fn normalizes_usage_percentages() {
        assert_eq!(UsagePercent::new(-1.0).value(), 0);
        assert_eq!(UsagePercent::new(19.6).value(), 20);
        assert_eq!(UsagePercent::new(101.0).value(), 100);
    }

    #[test]
    fn parses_claude_usage_windows() {
        let usage = parse_claude_usage(
            br#"{"five_hour":{"utilization":12.4,"resets_at":"2026-06-29T10:49:59+00:00"},"seven_day":{"utilization":67.6,"resets_at":"2026-07-03T08:59:59+00:00"}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(12));
        assert_eq!(usage.five_hour_reset_at, Some(1782730199));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(68));
        assert_eq!(usage.weekly_reset_at, Some(1783069199));
    }

    #[test]
    fn parses_claude_usage_without_reset_times() {
        let usage = parse_claude_usage(
            br#"{"five_hour":{"utilization":12.4},"seven_day":{"utilization":67.6}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(12));
        assert_eq!(usage.five_hour_reset_at, None);
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(68));
        assert_eq!(usage.weekly_reset_at, None);
    }

    #[test]
    fn parses_codex_usage_windows_by_duration() {
        let usage = parse_codex_usage(
            br#"{"rate_limit":{"primary_window":{"used_percent":21,"limit_window_seconds":18000,"reset_at":1782731632},"secondary_window":{"used_percent":73,"limit_window_seconds":604800,"reset_at":1783300364}}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(21));
        assert_eq!(usage.five_hour_reset_at, Some(1782731632));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(73));
        assert_eq!(usage.weekly_reset_at, Some(1783300364));
    }

    #[test]
    fn parses_kimi_limit_and_usage_windows() {
        let usage = parse_kimi_usage(
            br#"{"limits":[{"detail":{"limit":100,"remaining":75}}],"usage":{"limit":1000,"remaining":410}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(25));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(59));
    }

    #[test]
    fn parses_zhipu_windows_by_unit() {
        let usage = parse_zhipu_usage(
            br#"{"data":{"limits":[{"type":"TOKENS_LIMIT","unit":6,"percentage":63},{"type":"TOKENS_LIMIT","unit":3,"percentage":17}]}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(17));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(63));
    }

    #[test]
    fn zhipu_falls_back_when_window_units_are_missing() {
        let usage = parse_zhipu_usage(
            br#"{"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":7},{"type":"TOKENS_LIMIT","percentage":44,"nextResetTime":2000}]}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(7));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(44));
    }

    #[test]
    fn parses_minimax_remaining_percentages() {
        let usage = parse_minimax_usage(
            br#"{"model_remains":[{"model_name":"general","current_interval_remaining_percent":81,"current_weekly_status":1,"current_weekly_remaining_percent":34}]}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(19));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(66));
    }

    #[test]
    fn parses_zenmux_fractional_usage() {
        let usage = parse_zenmux_usage(
            br#"{"data":{"quota_5_hour":{"usage_percentage":0.32},"quota_7_day":{"usage_percentage":0.87}}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(32));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(87));
    }

    #[test]
    fn parses_volcengine_agent_plan_windows() {
        let usage = parse_volcengine_agent_usage(
            br#"{"Result":{"AFPFiveHour":{"Quota":200,"Used":50},"AFPWeekly":{"Quota":500,"Used":300}}}"#,
        )
        .expect("fixture should parse");

        assert_eq!(usage.five_hour_percent.map(UsagePercent::value), Some(25));
        assert_eq!(usage.weekly_percent.map(UsagePercent::value), Some(60));
    }

    #[test]
    fn assigns_five_twenty_percent_color_bands() {
        assert_eq!(UsageColorBand::for_percent(19), UsageColorBand::Green);
        assert_eq!(UsageColorBand::for_percent(20), UsageColorBand::LightGreen);
        assert_eq!(UsageColorBand::for_percent(40), UsageColorBand::Yellow);
        assert_eq!(UsageColorBand::for_percent(60), UsageColorBand::Orange);
        assert_eq!(UsageColorBand::for_percent(80), UsageColorBand::Red);
    }

    #[test]
    fn parses_cli_oauth_credentials() {
        let claude =
            parse_claude_credentials(br#"{"claudeAiOauth":{"accessToken":"claude-token"}}"#)
                .expect("Claude fixture should parse");
        assert_eq!(claude.access_token, "claude-token");

        let codex = parse_codex_credentials(
            br#"{"tokens":{"access_token":"codex-token","account_id":"account"}}"#,
        )
        .expect("Codex fixture should parse");
        assert_eq!(codex.access_token, "codex-token");
        assert_eq!(codex.account_id.as_deref(), Some("account"));
    }

    #[test]
    fn parses_codex_provider_configuration() {
        let provider = parse_codex_provider(
            br#"model_provider = "glm"
[model_providers.glm]
base_url = "https://open.bigmodel.cn/api/coding/paas/v4"
env_key = "GLM_API_KEY"
"#,
            |key| (key == "GLM_API_KEY").then_some("secret".to_string()),
        )
        .expect("fixture should parse");

        assert_eq!(
            provider.base_url,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(provider.api_key, "secret");
    }

    #[test]
    fn signs_volcengine_usage_requests() {
        let now = chrono::DateTime::parse_from_rfc3339("2024-06-21T00:00:00Z")
            .expect("date should parse")
            .with_timezone(&chrono::Utc);
        let signature = sign_volcengine_request(
            "AKLTtest",
            "secretkey",
            "cn-beijing",
            "Action=GetAFPUsage&Region=cn-beijing&Version=2024-01-01",
            now,
        )
        .expect("request should sign");

        assert_eq!(signature.date, "20240621T000000Z");
        assert_eq!(
            signature.content_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(
            signature
                .authorization
                .starts_with("HMAC-SHA256 Credential=AKLTtest/20240621/cn-beijing/ark/request,")
        );
    }
}
