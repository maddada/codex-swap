use crate::{
    auth,
    cli::Cli,
    fsutil, launch, sharing,
    store::{Account, Store},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};

struct Transaction {
    previous: Vec<(PathBuf, Option<Value>)>,
    directories: Vec<tempfile::TempDir>,
}

impl Transaction {
    fn new() -> Self {
        Self {
            previous: Vec::new(),
            directories: Vec::new(),
        }
    }

    fn write(&mut self, path: &Path, value: &impl serde::Serialize) -> Result<()> {
        if !self.previous.iter().any(|(saved, _)| saved == path) {
            let old = fsutil::optional_bytes(path)?
                .map(|bytes| serde_json::from_slice(&bytes))
                .transpose()
                .context("invalid existing account data; refusing replacement")?;
            self.previous.push((path.to_owned(), old));
        }
        fsutil::atomic_json(path, value)
    }

    fn finish(mut self, result: Result<()>) -> Result<()> {
        match result {
            Ok(()) => {
                for directory in self.directories {
                    let _ = directory.keep();
                }
                Ok(())
            }
            Err(error) => {
                let mut failures = Vec::new();
                for (path, previous) in self.previous.drain(..).rev() {
                    if self
                        .directories
                        .iter()
                        .any(|directory| path.starts_with(directory.path()))
                    {
                        // TempDir removes these only after every existing file was restored.
                        continue;
                    }
                    let restored = match previous {
                        Some(value) => fsutil::atomic_json(&path, &value),
                        None => match std::fs::remove_file(&path) {
                            Ok(()) => Ok(()),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                            Err(error) => Err(error.into()),
                        },
                    };
                    if let Err(restore) = restored {
                        failures.push(format!("{}: {restore:#}", path.display()));
                    }
                }
                if failures.is_empty() {
                    Err(error)
                } else {
                    // A failed registry restore may still refer to newly created homes.
                    // Retain them for recovery rather than deleting their credentials.
                    for directory in self.directories {
                        let _ = directory.keep();
                    }
                    Err(error.context(format!(
                        "account rollback also failed; new account homes retained for recovery: {}",
                        failures.join("; ")
                    )))
                }
            }
        }
    }
}

fn profile(
    store: &Store,
    transaction: &mut Transaction,
    number: u32,
    shared: bool,
) -> Result<PathBuf> {
    launch::prepare_main(&store.data.main_home)?;
    let profiles = store.root.join("accounts");
    fsutil::private_dir(&profiles)?;
    let directory = fsutil::private_tempdir(&profiles, &format!("{number}-"))?;
    sharing::config(&store.data.main_home, directory.path())?;
    if shared {
        sharing::history(&store.data.main_home, directory.path())?;
    }
    let path = directory.path().to_owned();
    transaction.directories.push(directory);
    Ok(path)
}

fn same_identity(a: &auth::Identity, b: &auth::Identity) -> bool {
    a.account_id == b.account_id && a.email == b.email
}

pub(crate) struct LoginDestination {
    pub account: Account,
    pub effective_home: PathBuf,
    pub previous: Vec<(PathBuf, Option<Vec<u8>>)>,
}

pub(crate) fn commit_login(
    cli: &Cli,
    destination: &LoginDestination,
    document: &Value,
    identity: auth::Identity,
) -> Result<()> {
    let mut store = Store::open(cli)?;
    let mut account = store.resolve(&destination.account.number.to_string())?;
    if account.home != destination.account.home
        || account.identity != destination.account.identity
        || store.effective_account(&account)?.home != destination.effective_home
    {
        bail!(
            "account selection changed during login; retry xswap login {}",
            account.number
        );
    }
    if account
        .identity
        .as_ref()
        .is_some_and(|expected| !same_identity(expected, &identity))
    {
        bail!(
            "Codex signed into a different account; saved credentials were unchanged. Retry xswap login {} and choose the registered account",
            account.number
        );
    }
    store.ensure_unique_identity(&identity, account.number)?;
    if destination.effective_home == store.data.main_home {
        crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
    }
    for (path, previous) in &destination.previous {
        if fsutil::optional_bytes(path)? != *previous {
            bail!(
                "account credentials changed during login; saved credentials were unchanged. Retry xswap login {}",
                account.number
            );
        }
    }
    let mut transaction = Transaction::new();
    let result = (|| {
        for (path, _) in &destination.previous {
            transaction.write(path, document)?;
        }
        account.identity = Some(identity);
        let entry = store
            .data
            .accounts
            .iter_mut()
            .find(|a| a.number == account.number)
            .context("account was removed")?;
        *entry = account;
        transaction.write(&store.root.join("accounts.json"), &store.data)
    })();
    transaction.finish(result)
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Older registries registered the mutable original home in place, so its login must be preserved before global activation replaces that file.
/// A legacy identity already overwritten outside xswap cannot be recovered; retain its slot as requiring login instead of assigning another account's credentials.
fn migrate_original(store: &mut Store, transaction: &mut Transaction) -> Result<()> {
    let Some(legacy) = store
        .data
        .accounts
        .iter()
        .find(|a| a.home == store.data.main_home)
        .cloned()
    else {
        return Ok(());
    };
    let home = profile(store, transaction, legacy.number, true)?;
    if let Some(live) = auth::identity(&store.data.main_home)? {
        if legacy
            .identity
            .as_ref()
            .is_none_or(|saved| same_identity(saved, &live))
        {
            let (document, _) = auth::credentials(&store.data.main_home)?;
            transaction.write(&home.join("auth.json"), &document)?;
        } else {
            eprintln!(
                "Original slot {} needs sign-in again: its old login was replaced before xswap could snapshot it.",
                legacy.number
            );
        }
    }
    let account = store
        .data
        .accounts
        .iter_mut()
        .find(|a| a.number == legacy.number)
        .unwrap();
    account.home = home;
    account.managed = true;
    account.share_history = true;
    store.data.original_account.get_or_insert(legacy.number);
    Ok(())
}

fn remap(store: &mut Store, from: u32, to: u32) {
    for number in [&mut store.data.default, &mut store.data.original_account]
        .into_iter()
        .flatten()
    {
        if *number == from {
            *number = to;
        }
    }
    for number in store.data.directory_mappings.values_mut() {
        if *number == from {
            *number = to;
        }
    }
}

fn capture(
    store: &mut Store,
    transaction: &mut Transaction,
    source: &Path,
    alias: Option<String>,
    slot: Option<u32>,
    shared: bool,
) -> Result<Account> {
    let (document, identity) = auth::credentials(source)?;
    let existing = store
        .data
        .accounts
        .iter()
        .find(|a| {
            a.identity
                .as_ref()
                .is_some_and(|id| same_identity(id, &identity))
        })
        .cloned();
    let number = slot
        .or_else(|| existing.as_ref().map(|a| a.number))
        .unwrap_or(store.data.next_number);
    if number == 0 || number == u32::MAX {
        bail!("slot must be positive and less than {}", u32::MAX);
    }
    if store
        .data
        .accounts
        .iter()
        .any(|a| a.number == number && existing.as_ref().is_none_or(|old| old.number != a.number))
    {
        bail!(
            "slot {number} belongs to another account; use an empty slot or move accounts explicitly"
        );
    }
    let account = if let Some(mut account) = existing {
        if let Some(alias) = alias {
            store
                .data
                .accounts
                .iter_mut()
                .find(|a| a.number == account.number)
                .unwrap()
                .alias = None;
            store.validate_alias(&Some(alias.clone()))?;
            account.alias = Some(alias);
        }
        if !account.managed {
            account.home = profile(store, transaction, number, account.share_history)?;
            account.managed = true;
        }
        if account.home != source {
            transaction.write(&account.home.join("auth.json"), &document)?;
        }
        let old_number = account.number;
        account.number = number;
        account.identity = Some(identity);
        if shared && !account.share_history {
            sharing::history(&store.data.main_home, &account.home)?;
            account.share_history = true;
        }
        *store
            .data
            .accounts
            .iter_mut()
            .find(|a| a.number == old_number)
            .unwrap() = account.clone();
        remap(store, old_number, number);
        account
    } else {
        store.validate_alias(&alias)?;
        let shared = shared || source == store.data.main_home;
        let home = profile(store, transaction, number, shared)?;
        transaction.write(&home.join("auth.json"), &document)?;
        let account = Account {
            number,
            alias,
            home,
            managed: true,
            share_history: shared,
            identity: Some(identity),
            enabled: true,
        };
        store.data.accounts.push(account.clone());
        account
    };
    store.data.next_number = store.data.next_number.max(number + 1);
    if source == store.data.main_home && store.data.original_account.is_none() {
        store.data.original_account = Some(number);
    }
    Ok(account)
}

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// The user requested claude-swap registration and global switching: add snapshots the current login, and switch activates credentials for bare Codex launches.
/// Re-registering the same identity refreshes its existing slot and preserves its alias unless an alias is supplied.
pub fn snapshot(
    cli: &Cli,
    source: Option<&Path>,
    alias: Option<String>,
    slot: Option<u32>,
    shared: bool,
) -> Result<u32> {
    let mut store = Store::open(cli)?;
    let source = fsutil::absolute(source.unwrap_or(&store.data.main_home))?;
    launch::validate_file_store(&source)?;
    crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
    let mut homes: std::collections::BTreeSet<_> =
        store.data.accounts.iter().map(|a| a.home.clone()).collect();
    homes.insert(store.data.main_home.clone());
    homes.insert(source.clone());
    let _leases: Vec<_> = homes
        .iter()
        .map(|home| store.lease(home, true))
        .collect::<Result<_>>()?;
    let source_before = fsutil::optional_bytes(&source.join("auth.json"))?;
    let main_before = fsutil::optional_bytes(&store.data.main_home.join("auth.json"))?;
    let mut transaction = Transaction::new();
    let mut number = 0;
    let result = (|| {
        migrate_original(&mut store, &mut transaction)?;
        number = capture(&mut store, &mut transaction, &source, alias, slot, shared)?.number;
        if source == store.data.main_home {
            store.data.default = Some(number);
        }
        crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
        if fsutil::optional_bytes(&source.join("auth.json"))? != source_before
            || fsutil::optional_bytes(&store.data.main_home.join("auth.json"))? != main_before
        {
            bail!("Codex credentials changed while saving the account; stop Codex and retry");
        }
        store.data.accounts.sort_by_key(|a| a.number);
        transaction.write(&store.root.join("accounts.json"), &store.data)
    })();
    transaction.finish(result)?;
    Ok(number)
}

pub fn select_global(cli: &Cli, identifier: Option<&str>) -> Result<()> {
    let mut store = Store::open(cli)?;
    launch::validate_file_store(&store.data.main_home)?;
    let live = store.live_account()?;
    let selected = match identifier {
        Some("default") => store
            .main_account()
            .context("no original account snapshot; save the current login with xswap add first")?,
        Some(value) => store.resolve(value)?,
        _ => {
            let mut eligible: Vec<_> = store
                .data
                .accounts
                .iter()
                .filter(|a| a.enabled && a.identity.is_some())
                .filter(|a| {
                    auth::verify(&a.home, &a.identity).is_ok()
                        || live
                            .as_ref()
                            .is_some_and(|active| active.number == a.number)
                })
                .cloned()
                .collect();
            eligible.sort_by_key(|a| a.number);
            let current = live.as_ref().map(|a| a.number);
            eligible
                .iter()
                .find(|a| match current {
                    Some(number) => a.number > number,
                    None => store.data.default == Some(a.number),
                })
                .or_else(|| eligible.first())
                .cloned()
                .context("no enabled, logged-in accounts to switch to")?
        }
    };
    let default = if identifier == Some("default") {
        None
    } else {
        Some(selected.number)
    };
    if live
        .as_ref()
        .is_some_and(|account| account.number == selected.number)
    {
        store.data.default = default;
        return store.save();
    }
    if selected.identity.is_none() {
        bail!(
            "account setup is incomplete; run xswap login {}",
            selected.number
        );
    }
    crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
    let mut homes: std::collections::BTreeSet<_> =
        store.data.accounts.iter().map(|a| a.home.clone()).collect();
    homes.insert(store.data.main_home.clone());
    let _leases: Vec<_> = homes
        .iter()
        .map(|home| store.lease(home, true))
        .collect::<Result<_>>()?;
    let original_live = fsutil::optional_bytes(&store.data.main_home.join("auth.json"))?;
    let mut transaction = Transaction::new();
    let result = (|| {
        migrate_original(&mut store, &mut transaction)?;
        if auth::identity(&store.data.main_home)?.is_some() {
            let main = store.data.main_home.clone();
            capture(&mut store, &mut transaction, &main, None, None, false)?;
        }
        let selected = store.resolve(&selected.number.to_string())?;
        auth::verify(&selected.home, &selected.identity)?;
        let (document, _) = auth::credentials(&selected.home)?;
        crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
        if fsutil::optional_bytes(&store.data.main_home.join("auth.json"))? != original_live {
            bail!("the current Codex login changed during switching; stop Codex and retry");
        }
        transaction.write(&store.data.main_home.join("auth.json"), &document)?;
        store.data.default = default;
        store.data.accounts.sort_by_key(|a| a.number);
        transaction.write(&store.root.join("accounts.json"), &store.data)
    })();
    transaction.finish(result)
}
