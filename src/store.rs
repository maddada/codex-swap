use crate::{auth::Identity, cli::Cli, fsutil};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Account {
    pub number: u32,
    pub alias: Option<String>,
    pub home: PathBuf,
    pub managed: bool,
    pub share_history: bool,
    pub identity: Option<Identity>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub main_home: PathBuf,
    pub next_number: u32,
    pub default: Option<u32>,
    pub accounts: Vec<Account>,
}

pub struct Store {
    pub root: PathBuf,
    pub data: Registry,
    // Released before exec; account leases use separate lock files.
    _lock: File,
}

impl Store {
    pub fn open(cli: &Cli) -> Result<Self> {
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let root = cli.data_dir.clone().unwrap_or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| user_home.join(".local/share"))
                .join("codex-swap")
        });
        // Validate before canonicalizing so a planted store-root symlink is refused.
        fsutil::private_dir(&root)?;
        let root = root.canonicalize()?;
        let lock = fsutil::lock(&root.join("registry.lock"), true, true)?;
        let saved = fsutil::optional_bytes(&root.join("accounts.json"))?;
        let data = if let Some(bytes) = saved {
            let registry: Registry =
                serde_json::from_slice(&bytes).context("invalid xswap registry")?;
            if registry.schema_version != 1 {
                bail!("unsupported xswap registry version");
            }
            if let Some(home) = &cli.codex_home {
                if fsutil::absolute(home)? != registry.main_home {
                    bail!(
                        "this registry already uses a different main Codex home; use a separate --data-dir"
                    );
                }
            }
            registry
        } else {
            let home = cli
                .codex_home
                .clone()
                .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
                .unwrap_or_else(|| user_home.join(".codex"));
            Registry {
                schema_version: 1,
                main_home: fsutil::absolute(&home)?,
                next_number: 1,
                default: None,
                accounts: vec![],
            }
        };
        if !data.main_home.is_absolute() || data.next_number == 0 {
            bail!("invalid xswap registry paths or numbering");
        }
        let mut seen = std::collections::HashSet::new();
        for account in &data.accounts {
            if account.number == 0 || !seen.insert(account.number) || !account.home.is_absolute() {
                bail!("invalid or duplicate account in xswap registry");
            }
        }
        if data.default.is_some_and(|n| !seen.contains(&n)) {
            bail!("default account is missing from registry");
        }
        Ok(Self {
            root,
            data,
            _lock: lock,
        })
    }

    pub fn save(&self) -> Result<()> {
        fsutil::atomic_json(&self.root.join("accounts.json"), &self.data)
    }

    pub fn resolve(&self, identifier: &str) -> Result<Account> {
        let matches: Vec<_> = self
            .data
            .accounts
            .iter()
            .filter(|a| {
                a.number.to_string() == identifier
                    || a.alias
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(identifier))
                    || a.identity
                        .as_ref()
                        .and_then(|i| i.email.as_deref())
                        .is_some_and(|s| s.eq_ignore_ascii_case(identifier))
            })
            .collect();
        match matches.as_slice() {
            [a] => Ok((*a).clone()),
            [] => bail!("unknown account; run xswap list"),
            _ => bail!("ambiguous account identifier; select its slot number from xswap list"),
        }
    }

    pub fn selected(&self, identifier: Option<&str>) -> Result<Option<Account>> {
        match identifier {
            Some("default") => Ok(self.main_account()),
            Some(s) => self.resolve(s).map(Some),
            None => match self.data.default {
                Some(n) => self.resolve(&n.to_string()).map(Some),
                None => Ok(self.main_account()),
            },
        }
    }

    pub fn main_account(&self) -> Option<Account> {
        self.data
            .accounts
            .iter()
            .find(|a| a.home == self.data.main_home)
            .cloned()
    }

    pub fn validate_alias(&self, alias: &Option<String>) -> Result<()> {
        if let Some(alias) = alias {
            if alias.is_empty()
                || alias.len() > 64
                || alias.eq_ignore_ascii_case("default")
                || alias.parse::<u32>().is_ok()
                || !alias
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            {
                bail!(
                    "alias must be 1-64 letters, digits, dots, hyphens or underscores, and cannot be a number or 'default'"
                );
            }
            if self.data.accounts.iter().any(|a| {
                a.alias
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(alias))
            }) {
                bail!("alias is already in use");
            }
        }
        Ok(())
    }

    pub fn lease(&self, home: &Path, exclusive: bool) -> Result<File> {
        let dir = self.root.join("locks");
        fsutil::private_dir(&dir)?;
        let hash = format!("{:x}", Sha256::digest(home.as_os_str().as_encoded_bytes()));
        fsutil::lock(&dir.join(format!("{hash}.lock")), exclusive, false)
    }

    pub fn ensure_unique_identity(&self, identity: &Identity, except: u32) -> Result<()> {
        if self.data.accounts.iter().any(|a| {
            a.number != except
                && a.identity.as_ref().is_some_and(|i| {
                    i.account_id == identity.account_id && i.email == identity.email
                })
        }) {
            bail!(
                "this account is already registered; use its existing slot so refreshed credentials have one home"
            );
        }
        Ok(())
    }

    pub fn replace(&mut self, account: Account) -> Result<()> {
        let entry = self
            .data
            .accounts
            .iter_mut()
            .find(|a| a.number == account.number)
            .context("account was removed")?;
        *entry = account;
        self.save()
    }
}
