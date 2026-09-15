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
        || store
            .effective_account(&account, store.observe_live_account().as_ref())
            .home
            != destination.effective_home
    {
        bail!(
            "account selection changed during login; retry xswap login {}",
            account.number
        );
    }
    if account
        .identity
        .as_ref()
        .is_some_and(|expected| !expected.same_owner(&identity))
    {
        bail!(
            "Codex signed into a different account or its saved owner is unresolved; saved credentials were unchanged. Retry xswap login {} for a known owner. An unresolved legacy owner needs xswap add --login --email <owner> --slot <unused-slot>",
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
    let mut copied_identity = None;
    if let Some((document, live)) = auth::optional_credentials(&store.data.main_home)? {
        if legacy
            .identity
            .as_ref()
            .is_some_and(|saved| saved.same_owner(&live))
        {
            transaction.write(&home.join("auth.json"), &document)?;
            copied_identity = Some(live);
        } else {
            eprintln!(
                "Original slot {} needs sign-in again: its saved owner could not be matched to the current login. Use xswap login for a known owner; an unknown legacy owner needs xswap add --login --email <owner> --slot <unused-slot>.",
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
    if let Some(identity) = copied_identity {
        account.identity = Some(identity);
    }
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
    let existing = store.account_for_identity(&identity)?;
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
            store.validate_alias_except(&Some(alias.clone()), Some(account.number))?;
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

fn validate_snapshot_alias(store: &Store, source: &Path, alias: &Option<String>) -> Result<()> {
    let identity = auth::require(source)?;
    let existing = store.account_for_identity(&identity)?.map(|a| a.number);
    store.validate_alias_except(alias, existing)
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
    validate_snapshot_alias(&store, &source, &alias)?;
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
        let (document, identity) = auth::verified_credentials(&selected.home, &selected.identity)?;
        crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
        if fsutil::optional_bytes(&store.data.main_home.join("auth.json"))? != original_live {
            bail!("the current Codex login changed during switching; stop Codex and retry");
        }
        transaction.write(&store.data.main_home.join("auth.json"), &document)?;
        store
            .data
            .accounts
            .iter_mut()
            .find(|account| account.number == selected.number)
            .context("account was removed")?
            .identity = Some(identity);
        store.data.default = default;
        store.data.accounts.sort_by_key(|a| a.number);
        transaction.write(&store.root.join("accounts.json"), &store.data)
    })();
    transaction.finish(result)
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
mod alias_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::json;

    #[test]
    fn snapshot_alias_preflight_preserves_own_slot_refreshes() {
        let directory = tempfile::tempdir().unwrap();
        let cli = Cli {
            data_dir: Some(directory.path().join("data")),
            codex_home: Some(directory.path().join("main")),
            codex_bin: None,
            command: crate::cli::Action::List(crate::cli::Output { json: false }),
        };
        let mut store = Store::open(&cli).unwrap();
        let source = store.data.main_home.clone();
        let saved_home = store.root.join("saved-home");
        fsutil::private_dir(&source).unwrap();
        fsutil::private_dir(&saved_home).unwrap();
        let payload = URL_SAFE_NO_PAD.encode(
            r#"{"email":"user@example.invalid","https://api.openai.com/auth":{"chatgpt_user_id":"synthetic-user-1"}}"#,
        );
        let document = json!({"auth_mode": "chatgpt", "tokens": {
            "account_id": "synthetic-workspace", "access_token": "synthetic-refreshed",
            "refresh_token": "synthetic-refresh", "id_token": format!("e30.{payload}.synthetic")
        }});
        fsutil::atomic_json(&source.join("auth.json"), &document).unwrap();
        let mut previous = document.clone();
        previous["tokens"]["access_token"] = json!("synthetic-previous");
        fsutil::atomic_json(&saved_home.join("auth.json"), &previous).unwrap();
        store.data.accounts.push(Account {
            number: 1,
            alias: Some("work-team".into()),
            home: saved_home.clone(),
            managed: true,
            share_history: false,
            identity: Some(auth::require(&source).unwrap()),
            enabled: true,
        });
        store.data.accounts.push(Account {
            number: 2,
            alias: Some("personal".into()),
            home: store.root.join("other-home"),
            managed: true,
            share_history: false,
            identity: None,
            enabled: true,
        });
        store.data.next_number = 3;
        for alias in [None, Some("WORK-TEAM"), Some("work.team"), Some("_work")] {
            validate_snapshot_alias(&store, &source, &alias.map(String::from)).unwrap();
        }
        assert!(validate_snapshot_alias(&store, &source, &Some("-work".into())).is_err());
        assert!(validate_snapshot_alias(&store, &source, &Some("PERSONAL".into())).is_err());

        let mut transaction = Transaction::new();
        let account = capture(
            &mut store,
            &mut transaction,
            &source,
            Some("WORK-TEAM".into()),
            None,
            false,
        )
        .unwrap();
        transaction.finish(Ok(())).unwrap();
        assert_eq!(account.number, 1);
        assert_eq!(account.alias.as_deref(), Some("WORK-TEAM"));
        assert_eq!(account.home, saved_home);
        assert_eq!(store.data.accounts.len(), 2);
        assert_eq!(store.data.next_number, 3);
        assert_eq!(auth::credentials(&saved_home).unwrap().0, document);
        store.data.accounts[0].identity.as_mut().unwrap().user_id = Some("synthetic-user-2".into());
        assert!(
            validate_snapshot_alias(&store, &source, &Some("WORK-TEAM".into()))
                .unwrap_err()
                .to_string()
                .contains("alias is already in use")
        );
        validate_snapshot_alias(&store, &source, &Some("another-team".into())).unwrap();

        store.data.accounts[0].identity.as_mut().unwrap().user_id = None;
        store.data.accounts[1].identity = store.data.accounts[0].identity.clone();
        for alias in [None, Some("WORK-TEAM"), Some("another-team")] {
            assert!(
                validate_snapshot_alias(&store, &source, &alias.map(String::from))
                    .unwrap_err()
                    .to_string()
                    .contains("ambiguous saved account identity")
            );
        }
    }
}
