use crate::{
    auth,
    cli::{Add, Cli, Output},
    fsutil, launch, sharing,
    store::{Account, Store},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    number: u32,
    alias: Option<String>,
    email: Option<String>,
    account_id: Option<String>,
    plan: Option<String>,
    home: std::path::PathBuf,
    managed: bool,
    share_history: bool,
    is_default: bool,
    login_status: &'static str,
    enabled: bool,
}

fn view(store: &Store, account: &Account) -> AccountView {
    let (identity, login_status) = match auth::identity(&account.home) {
        Ok(Some(live)) => {
            if account
                .identity
                .as_ref()
                .is_some_and(|i| i.account_id != live.account_id || i.email != live.email)
            {
                (account.identity.clone(), "identity_changed")
            } else {
                (Some(live), "present")
            }
        }
        Ok(None) => (account.identity.clone(), "login_required"),
        Err(_) => (account.identity.clone(), "invalid_credentials"),
    };
    AccountView {
        number: account.number,
        alias: account.alias.clone(),
        email: identity.as_ref().and_then(|i| i.email.clone()),
        account_id: identity.as_ref().map(|i| i.account_id.clone()),
        plan: identity.as_ref().and_then(|i| i.plan.clone()),
        home: account.home.clone(),
        managed: account.managed,
        share_history: account.share_history || account.home == store.data.main_home,
        is_default: store.data.default == Some(account.number)
            || (store.data.default.is_none() && account.home == store.data.main_home),
        login_status,
        enabled: account.enabled,
    }
}

fn emit(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}

fn human(account: &AccountView) {
    let label = account
        .alias
        .as_deref()
        .or(account.email.as_deref())
        .unwrap_or("unnamed");
    println!(
        "{} {}  {}  {}  {}{}",
        if account.is_default { "*" } else { " " },
        account.number,
        label.escape_default(),
        account.email.as_deref().unwrap_or("").escape_default(),
        account.login_status,
        if account.enabled { "" } else { "  disabled" }
    );
}

pub fn list(cli: &Cli, output: &Output) -> Result<()> {
    let store = Store::open(cli)?;
    let accounts: Vec<_> = store
        .data
        .accounts
        .iter()
        .map(|a| view(&store, a))
        .collect();
    if output.json {
        emit(&json!({"schemaVersion": 1, "accounts": accounts}))?;
    } else if accounts.is_empty() {
        println!("No saved accounts. Use xswap add or xswap add --login.");
    } else {
        for account in accounts {
            human(&account);
        }
    }
    Ok(())
}

pub fn status(cli: &Cli, output: &Output) -> Result<()> {
    let store = Store::open(cli)?;
    let selected = match store.data.default {
        Some(number) => Some(store.resolve(&number.to_string())?),
        None => store.main_account(),
    };
    let active = selected.as_ref().map(|a| view(&store, a));
    if output.json {
        emit(
            &json!({"schemaVersion": 1, "active": active, "defaultHome": store.data.main_home,
            "usesOriginalDefault": store.data.default.is_none()}),
        )?;
    } else if let Some(active) = active {
        human(&active);
    } else {
        println!(
            "Default: original Codex home ({})",
            store.data.main_home.display()
        );
    }
    Ok(())
}

pub fn switch(cli: &Cli, identifier: &str, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let target = if identifier == "default" {
        if let Some(account) = store.main_account() {
            Store::require_enabled(&account)?;
        }
        None
    } else {
        let account = store.resolve(identifier)?;
        Store::require_enabled(&account)?;
        if account.identity.is_none() {
            bail!(
                "account setup is incomplete; run xswap login {}",
                account.number
            );
        }
        auth::verify(&account.home, &account.identity)?;
        Some(account.number)
    };
    store.data.default = target;
    store.save()?;
    drop(store);
    status(cli, output)
}

pub fn remove(cli: &Cli, identifier: &str, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let account = store.resolve(identifier)?;
    let _lease = store.lease(&account.home, true)?;
    store.data.accounts.retain(|a| a.number != account.number);
    store
        .data
        .directory_mappings
        .retain(|_, number| *number != account.number);
    if store.data.default == Some(account.number) {
        store.data.default = None;
    }
    store.save()?;
    if output.json {
        emit(
            &json!({"schemaVersion": 1, "removed": account.number, "retainedHome": account.home}),
        )?;
    } else {
        println!(
            "Removed slot {}. Credentials and history remain at {}",
            account.number,
            account.home.display()
        );
    }
    Ok(())
}

pub fn add(cli: &Cli, args: &Add) -> Result<()> {
    let mut store = Store::open(cli)?;
    store.validate_alias(&args.alias)?;
    let number = args.slot.unwrap_or(store.data.next_number);
    if number == 0 || store.data.accounts.iter().any(|a| a.number == number) {
        bail!("slot must be a positive, unused number");
    }
    let next = number.checked_add(1).context("slot number is too large")?;
    launch::prepare_main(&store.data.main_home)?;
    let (home, managed, identity) = if args.login {
        let profiles = store.root.join("accounts");
        fsutil::private_dir(&profiles)?;
        let dir = fsutil::private_tempdir(&profiles, &format!("{number}-"))?;
        sharing::config(&store.data.main_home, dir.path())?;
        if args.share_history {
            sharing::history(&store.data.main_home, dir.path())?;
        }
        (dir.keep(), true, None)
    } else {
        let home = fsutil::absolute(args.home.as_deref().unwrap_or(&store.data.main_home))?;
        if store.data.accounts.iter().any(|a| a.home == home) {
            bail!("this Codex home is already registered");
        }
        launch::validate_file_store(&home)?;
        let identity = auth::require(&home)?;
        store.ensure_unique_identity(&identity, number)?;
        if args.share_history {
            sharing::history(&store.data.main_home, &home)?;
        }
        (home, false, Some(identity))
    };
    let account = Account {
        number,
        alias: args.alias.clone(),
        share_history: args.share_history || home == store.data.main_home,
        home,
        managed,
        identity,
        enabled: true,
    };
    store.data.next_number = store.data.next_number.max(next);
    store.data.accounts.push(account.clone());
    store.save()?;
    drop(store);
    if args.login {
        eprintln!(
            "Created account slot {number}. If sign-in is interrupted, retry xswap login {number}."
        );
        launch::login(cli, &number.to_string(), args.device_auth)?;
    }
    let store = Store::open(cli)?;
    let saved = store.resolve(&number.to_string())?;
    if args.output.json {
        emit(&json!({"schemaVersion": 1, "account": view(&store, &saved)}))?;
    } else {
        human(&view(&store, &saved));
    }
    Ok(())
}

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// The user requested alias edits, moving or swapping slots, and enabling or disabling accounts.
/// Disabled accounts remain accessible through an explicit account selection; implicit launches require an enabled account.
pub fn rename(cli: &Cli, identifier: &str, alias: Option<String>, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let mut account = store.resolve(identifier)?;
    let _lease = store.lease(&account.home, true)?;
    // Ignore this account when checking whether its replacement alias is occupied.
    store
        .data
        .accounts
        .iter_mut()
        .find(|a| a.number == account.number)
        .unwrap()
        .alias = None;
    store.validate_alias(&alias)?;
    account.alias = alias;
    store.replace(account.clone())?;
    account_result(&store, &account, output)
}

pub fn set_enabled(cli: &Cli, identifier: &str, enabled: bool, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let mut account = store.resolve(identifier)?;
    let _lease = store.lease(&account.home, true)?;
    account.enabled = enabled;
    store.replace(account.clone())?;
    account_result(&store, &account, output)
}

fn account_result(store: &Store, account: &Account, output: &Output) -> Result<()> {
    let account = view(store, account);
    if output.json {
        emit(&json!({"schemaVersion": 1, "account": account}))
    } else {
        human(&account);
        Ok(())
    }
}

pub fn move_slot(cli: &Cli, identifier: &str, slot: u32, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let account = store.resolve(identifier)?;
    renumber(&mut store, account.number, slot, output)
}

pub fn swap(cli: &Cli, identifier: &str, other: &str, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let account = store.resolve(identifier)?;
    let other = store.resolve(other)?;
    renumber(&mut store, account.number, other.number, output)
}

fn renumber(store: &mut Store, from: u32, to: u32, output: &Output) -> Result<()> {
    if to == 0 || to == u32::MAX {
        bail!("slot must be positive and less than {}", u32::MAX);
    }
    // An interactive login commits using its slot after releasing the registry lock.
    // Keep slot identities stable until every affected account lease is free.
    let _leases: Vec<_> = store
        .data
        .accounts
        .iter()
        .filter(|a| a.number == from || a.number == to)
        .map(|a| store.lease(&a.home, true))
        .collect::<Result<_>>()?;
    for account in &mut store.data.accounts {
        if account.number == from {
            account.number = to;
        } else if account.number == to {
            account.number = from;
        }
    }
    store.data.default = store
        .data
        .default
        .map(|number| remap_number(number, from, to));
    for number in store.data.directory_mappings.values_mut() {
        *number = remap_number(*number, from, to);
    }
    store.data.next_number = store.data.next_number.max(to + 1);
    store.data.accounts.sort_by_key(|a| a.number);
    store.save()?;
    let account = store.resolve(&to.to_string())?;
    account_result(store, &account, output)
}

pub fn remap_number(number: u32, from: u32, to: u32) -> u32 {
    if number == from {
        to
    } else if number == to {
        from
    } else {
        number
    }
}
