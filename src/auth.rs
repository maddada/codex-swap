use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    // Preserve legacy labels while rejecting owner matches when their saved hint is unusable.
    #[serde(skip)]
    pub(crate) legacy_hint_unusable: bool,
}

// Match Codex's externally tagged unit variants, including {"chatgpt": null}.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum AuthMode {
    #[serde(rename = "apikey")]
    ApiKey,
    Chatgpt,
    ChatgptAuthTokens,
    Headers,
    AgentIdentity,
    PersonalAccessToken,
    BedrockApiKey,
    BedrockAccessKeys,
}

// Match Codex's derived serde structs, including positional arrays and defaults.
#[derive(Deserialize)]
struct BedrockApiKeyAuth {
    #[serde(rename = "api_key")]
    _api_key: String,
    #[serde(rename = "region")]
    _region: String,
}

#[derive(Deserialize)]
struct BedrockAccessKeysAuth {
    #[serde(rename = "access_key_id")]
    _access_key_id: String,
    #[serde(rename = "secret_access_key")]
    _secret_access_key: String,
    #[serde(default, rename = "session_token")]
    _session_token: Option<String>,
}

impl Identity {
    pub fn has_owner(&self) -> bool {
        !self.legacy_hint_unusable
            && (usable(self.user_id.as_deref()).is_some()
                || usable(self.email.as_deref()).is_some())
    }

    /// Stable user claims identify workspace members; older tokens need a shared email.
    pub fn same_owner(&self, other: &Self) -> bool {
        if self.account_id != other.account_id || !self.has_owner() || !other.has_owner() {
            return false;
        }
        match (
            usable(self.user_id.as_deref()),
            usable(other.user_id.as_deref()),
        ) {
            (Some(first), Some(second)) => first == second,
            _ => match (
                usable(self.email.as_deref()),
                usable(other.email.as_deref()),
            ) {
                (Some(first), Some(second)) => first == second,
                _ => false,
            },
        }
    }
}

fn usable(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

/// Identity decoding follows swapdex's Codex adapter; see THIRD_PARTY_NOTICES.md.
/// Claims label the local account only. They are not an authentication verification.
pub fn identity(home: &Path) -> Result<Option<Identity>> {
    Ok(optional_credentials(home)?.map(|(_, identity)| identity))
}

pub fn optional_credentials(home: &Path) -> Result<Option<(Value, Identity)>> {
    let Some(value) = document(home)? else {
        return Ok(None);
    };
    let identity = identity_value(&value)?;
    Ok(Some((value, identity)))
}

fn document(home: &Path) -> Result<Option<Value>> {
    let Some(bytes) = crate::fsutil::optional_bytes(&home.join("auth.json"))? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid Codex auth.json (contents omitted)"))?;
    Ok(Some(value))
}

pub fn credentials(home: &Path) -> Result<(Value, Identity)> {
    optional_credentials(home)?
        .context("no file-based ChatGPT login here; sign in with Codex first")
}

fn identity_value(value: &Value) -> Result<Identity> {
    let mode = Option::<AuthMode>::deserialize(&value["auth_mode"])
        .map_err(|_| anyhow::anyhow!("invalid Codex auth_mode (contents omitted)"))?;
    let api_key = optional_string(value, "OPENAI_API_KEY")?;
    let personal_access_token = optional_string(value, "personal_access_token")?;
    let bedrock_api_key = Option::<BedrockApiKeyAuth>::deserialize(&value["bedrock_api_key"])
        .map_err(|_| anyhow::anyhow!("invalid Codex bedrock_api_key (contents omitted)"))?;
    let bedrock_access_keys =
        Option::<BedrockAccessKeysAuth>::deserialize(&value["bedrock_access_keys"])
            .map_err(|_| anyhow::anyhow!("invalid Codex bedrock_access_keys (contents omitted)"))?;
    // Match Codex's resolved_mode: an explicit mode wins, otherwise any stored
    // non-ChatGPT credential selects its mode, including an empty API-key string.
    if mode.is_some_and(|m| m != AuthMode::Chatgpt)
        || (mode.is_none()
            && (api_key.is_some()
                || personal_access_token.is_some()
                || bedrock_api_key.is_some()
                || bedrock_access_keys.is_some()))
    {
        bail!(
            "xswap currently manages ChatGPT logins; this home uses another authentication mode. Run xswap login <account> and sign in with ChatGPT"
        );
    }
    let tokens = &value["tokens"];
    for key in ["access_token", "refresh_token", "id_token"] {
        if tokens[key].as_str().is_none_or(str::is_empty) {
            bail!("incomplete Codex login; sign in again");
        }
    }
    identity_labels(value)
}

/// Read-only enrichment uses a registered separate home's saved hint, never the main home.
/// Token labels establish a legacy owner hint without authorizing its transport credentials.
pub fn enrich_legacy_identity(home: &Path, expected: &mut Option<Identity>) {
    let Some(saved) = expected.as_mut() else {
        return;
    };
    saved.legacy_hint_unusable = false;
    if usable(saved.user_id.as_deref()).is_some() || usable(saved.email.as_deref()).is_none() {
        return;
    }
    let hint = document(home)
        .ok()
        .flatten()
        .and_then(|value| identity_labels(&value).ok());
    match hint {
        Some(hint) if saved.same_owner(&hint) => {
            if usable(hint.user_id.as_deref()).is_some() {
                saved.user_id = hint.user_id;
            }
        }
        _ => saved.legacy_hint_unusable = true,
    }
}

fn identity_labels(value: &Value) -> Result<Identity> {
    let tokens = &value["tokens"];
    let account_id = tokens["account_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Codex login has no account ID; sign in with ChatGPT")?;
    let mut parts = tokens["id_token"]
        .as_str()
        .context("invalid Codex identity token")?
        .split('.');
    let payload = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            payload
        }
        _ => bail!("invalid Codex identity token"),
    };
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .map_err(|_| anyhow::anyhow!("invalid Codex identity token encoding"))?,
    )
    .map_err(|_| anyhow::anyhow!("invalid Codex identity token claims"))?;
    let auth = &claims["https://api.openai.com/auth"];
    // Preserve Codex's primary-claim precedence, then treat blank owner labels as unknown.
    let email = usable(
        claims["email"]
            .as_str()
            .or_else(|| claims["https://api.openai.com/profile"]["email"].as_str()),
    );
    let user_id = usable(
        auth["chatgpt_user_id"]
            .as_str()
            .or_else(|| auth["user_id"].as_str()),
    );
    let identity = Identity {
        account_id: account_id.to_owned(),
        user_id: user_id.map(str::to_owned),
        email: email.map(str::to_owned),
        plan: auth["chatgpt_plan_type"].as_str().map(str::to_owned),
        legacy_hint_unusable: false,
    };
    if !identity.has_owner() {
        bail!("Codex login has no usable user ID or email; sign in again");
    }
    Ok(identity)
}

fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => bail!("invalid Codex {key}; expected a string or null (contents omitted)"),
    }
}

pub fn require(home: &Path) -> Result<Identity> {
    identity(home)?.context(
        "no file-based ChatGPT login here; use xswap add --login to sign in to an isolated account",
    )
}

pub fn verify(home: &Path, expected: &Option<Identity>) -> Result<Identity> {
    Ok(verified_credentials(home, expected)?.1)
}

pub fn verified_credentials(home: &Path, expected: &Option<Identity>) -> Result<(Value, Identity)> {
    let (document, live) = credentials(home)?;
    if expected.as_ref().is_some_and(|id| !id.same_owner(&live)) {
        bail!(
            "this directory is now signed into another account or its saved owner is unknown; run xswap login with this account's slot or alias for a known owner. An unresolved legacy owner needs xswap add --login --email <owner> --slot <unused-slot>"
        );
    }
    Ok((document, live))
}

#[cfg(test)]
mod tests;
