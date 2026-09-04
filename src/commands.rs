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
        "{} {}  {}  {}  {}",
        if account.is_default { "*" } else { " " },
        account.number,
        label.escape_default(),
        account.email.as_deref().unwrap_or("").escape_default(),
        account.login_status
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
    let selected = store.selected(None)?;
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
        None
    } else {
        let account = store.resolve(identifier)?;
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
        let dir = tempfile::Builder::new()
            .prefix(&format!("{number}-"))
            .tempdir_in(profiles)?;
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
