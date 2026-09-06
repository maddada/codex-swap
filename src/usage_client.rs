use crate::{auth, fsutil};
use anyhow::{Context, Result, bail};
use reqwest::{blocking::Client, header::HeaderMap, redirect::Policy};
use serde_json::Value;
use std::{io::Read, path::Path, time::Duration};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub struct Response {
    pub body: Value,
    pub headers: HeaderMap,
}

pub fn client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::none())
        .user_agent(concat!("codex-swap/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("create usage HTTP client")
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Codex owns refresh-token rotation in each account home; usage reads its current access token because competing refreshes can invalidate a running Codex login.
/// The endpoint and account header follow OpenUsage's CodexUsageClient; see THIRD_PARTY_NOTICES.md.
pub fn fetch(
    client: &Client,
    home: &Path,
    expected: &Option<auth::Identity>,
) -> Result<(auth::Identity, Response)> {
    let identity = auth::verify(home, expected)?;
    let bytes = fsutil::optional_bytes(&home.join("auth.json"))?
        .context("no Codex login; sign into this account with xswap login")?;
    let auth: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid Codex auth.json (contents omitted)"))?;
    let account_id = auth["tokens"]["account_id"]
        .as_str()
        .context("Codex login has no account ID")?;
    if account_id != identity.account_id {
        bail!("Codex login changed while reading usage; retry the command");
    }
    let token = auth["tokens"]["access_token"]
        .as_str()
        .filter(|value| !value.is_empty())
        .context("Codex login has no access token; sign in with xswap login")?;
    let response = client
        .get(USAGE_URL)
        .bearer_auth(token)
        .header("ChatGPT-Account-Id", account_id)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                anyhow::anyhow!("Codex usage request timed out after 10 seconds")
            } else {
                anyhow::anyhow!("could not connect to Codex usage API")
            }
        })?;
    let status = response.status();
    if status.as_u16() == 401 {
        bail!(
            "Codex access token expired or was rejected; run Codex for this account to refresh its login, or use xswap login <account>"
        );
    }
    if status.as_u16() == 429 {
        bail!("Codex usage API is rate limited (HTTP 429); retry later");
    }
    if !status.is_success() {
        bail!("Codex usage request failed (HTTP {})", status.as_u16());
    }
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read Codex usage response")?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        bail!("Codex usage response is too large");
    }
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Codex usage API returned invalid JSON (contents omitted)"))?;
    if !body.is_object() {
        bail!("Codex usage API returned an invalid response");
    }
    Ok((identity, Response { body, headers }))
}
