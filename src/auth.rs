use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub account_id: String,
    pub email: Option<String>,
    pub plan: Option<String>,
}

/// Identity decoding follows swapdex's Codex adapter; see THIRD_PARTY_NOTICES.md.
/// Claims label the local account only. They are not an authentication verification.
pub fn identity(home: &Path) -> Result<Option<Identity>> {
    let Some(bytes) = crate::fsutil::optional_bytes(&home.join("auth.json"))? else {
        return Ok(None);
    };
    let value: Value =
        serde_json::from_slice(&bytes).context("invalid Codex auth.json (contents omitted)")?;
    identity_value(&value).map(Some)
}

pub fn credentials(home: &Path) -> Result<(Value, Identity)> {
    let bytes = crate::fsutil::optional_bytes(&home.join("auth.json"))?
        .context("no file-based ChatGPT login here; sign in with Codex first")?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid Codex auth.json (contents omitted)"))?;
    let identity = identity_value(&value)?;
    Ok((value, identity))
}

fn identity_value(value: &Value) -> Result<Identity> {
    if value
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|m| m != "chatgpt")
    {
        bail!("xswap currently manages ChatGPT logins; this home uses another authentication mode");
    }
    let tokens = &value["tokens"];
    let account_id = tokens["account_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Codex login has no account ID; sign in with ChatGPT")?;
    for key in ["access_token", "refresh_token", "id_token"] {
        if tokens[key].as_str().is_none_or(str::is_empty) {
            bail!("incomplete Codex login; sign in again");
        }
    }
    let payload = tokens["id_token"]
        .as_str()
        .and_then(|s| s.split('.').nth(1))
        .context("invalid Codex identity token")?;
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|_| anyhow::anyhow!("invalid Codex identity token encoding"))?,
    )
    .map_err(|_| anyhow::anyhow!("invalid Codex identity token claims"))?;
    Ok(Identity {
        account_id: account_id.to_owned(),
        email: claims["email"].as_str().map(str::to_owned),
        plan: claims["https://api.openai.com/auth"]["chatgpt_plan_type"]
            .as_str()
            .map(str::to_owned),
    })
}

pub fn require(home: &Path) -> Result<Identity> {
    identity(home)?.context(
        "no file-based ChatGPT login here; use xswap add --login to sign in to an isolated account",
    )
}

pub fn verify(home: &Path, expected: &Option<Identity>) -> Result<Identity> {
    let live = require(home)?;
    if expected
        .as_ref()
        .is_some_and(|id| id.account_id != live.account_id || id.email != live.email)
    {
        bail!(
            "this directory is now signed into another account; run xswap login with this account's slot or alias and choose its registered identity"
        );
    }
    Ok(live)
}
