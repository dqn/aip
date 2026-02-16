use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::tool::Tool;

#[derive(Debug, Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthData>,
}

#[derive(Debug, Deserialize)]
struct OAuthData {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "organizationName")]
    organization_name: Option<String>,
    #[serde(rename = "planType")]
    plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UsageResponse {
    pub five_hour: UsageWindow,
    pub seven_day: UsageWindow,
}

#[derive(Debug, Deserialize)]
pub struct UsageWindow {
    pub utilization: f64,
    pub resets_at: DateTime<Utc>,
}

pub struct ProfileInfo {
    #[allow(dead_code)] // Field is populated but may be used in future
    pub organization_name: Option<String>,
    pub plan_type: Option<String>,
}

fn read_credentials() -> Result<(String, ProfileInfo)> {
    let current = Tool::Claude
        .current_profile()?
        .ok_or_else(|| anyhow!("no current profile set for Claude Code"))?;

    let creds_path = Tool::Claude.profile_dir(&current)?.join("credentials.json");

    if !creds_path.exists() {
        return Err(anyhow!(
            "credentials.json not found for profile '{}'",
            current
        ));
    }

    let content = std::fs::read_to_string(&creds_path)?;
    let creds: Credentials = serde_json::from_str(&content)?;
    let oauth = creds
        .claude_ai_oauth
        .ok_or_else(|| anyhow!("no OAuth data in credentials"))?;

    let info = ProfileInfo {
        organization_name: oauth.organization_name,
        plan_type: oauth.plan_type,
    };

    Ok((oauth.access_token, info))
}

pub async fn fetch_usage() -> Result<(UsageResponse, ProfileInfo)> {
    let (token, info) = read_credentials()?;

    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "usage API returned status {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    let usage: UsageResponse = resp.json().await?;
    Ok((usage, info))
}
