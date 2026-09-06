use crate::{auth, cli::Cli, fsutil, sharing, store::Store};
#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, bail};
use fs2::FileExt;
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::Path,
    process::Command,
};

fn command(binary: &OsStr, home: &Path, file_auth: bool) -> Result<Command> {
    let mut cmd = crate::platform::codex_command(binary)?;
    cmd.env("CODEX_HOME", home);
    // Shell credentials must not silently outrank the explicitly selected account.
    for name in [
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CODEX_ACCESS_TOKEN",
        "OPENAI_ACCESS_TOKEN",
    ] {
        cmd.env_remove(name);
    }
    cmd.env_remove("CODEX_SQLITE_HOME");
    if file_auth {
        cmd.args(["-c", "cli_auth_credentials_store=\"file\""]);
    }
    Ok(cmd)
}

fn check_overrides(args: &[OsString], shared: bool) -> Result<()> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            break;
        }
        let arg = arg.to_string_lossy();
        let value = if arg == "-c" || arg == "--config" {
            args.get(i + 1).map(|a| a.to_string_lossy())
        } else {
            arg.strip_prefix("--config=")
                .or_else(|| arg.strip_prefix("-c"))
                .map(std::borrow::Cow::Borrowed)
        };
        if let Some(value) = value {
            let key = value
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if key == "cli_auth_credentials_store" || (shared && key == "sqlite_home") {
                bail!("xswap owns {key} for selected accounts; remove that Codex override");
            }
        }
    }
    Ok(())
}

pub fn run(
    cli: &Cli,
    identifier: Option<&str>,
    share_history: bool,
    args: &[OsString],
) -> Result<()> {
    let mut store = Store::open(cli)?;
    let mut selected = store.selected_for_run(identifier)?;
    let effective = selected
        .as_ref()
        .map(|a| store.effective_account(a))
        .transpose()?;
    let home = effective
        .as_ref()
        .map(|a| a.home.clone())
        .unwrap_or_else(|| store.data.main_home.clone());
    let shared = share_history
        || selected.as_ref().is_some_and(|a| a.share_history)
        || home == store.data.main_home;
    check_overrides(args, shared)?;
    let enabling = selected
        .as_ref()
        .is_some_and(|a| shared && !a.share_history && home != store.data.main_home);
    let lease = store.lease(&home, enabling)?;
    let snapshot_lease = selected
        .as_ref()
        .filter(|a| a.home != home)
        .map(|a| store.lease(&a.home, false))
        .transpose()?;
    if let Some(account) = selected.as_mut() {
        if account.identity.is_none() {
            bail!(
                "account setup is incomplete; run xswap login {}",
                account.number
            );
        }
        auth::verify(&home, &account.identity)?;
        if account.managed && home != store.data.main_home {
            sharing::config(&store.data.main_home, &home)?;
        }
        if shared {
            sharing::history(&store.data.main_home, &home)?;
            if !account.share_history && home != store.data.main_home {
                account.share_history = true;
                store.replace(account.clone())?;
            }
        }
    }
    let mut cmd = command(&store.codex_bin(cli), &home, selected.is_some())?;
    if shared && home != store.data.main_home {
        let sqlite = sharing::sqlite_home(&store.data.main_home)?;
        let value = toml::Value::String(sqlite.to_string_lossy().into_owned()).to_string();
        cmd.args(["-c", &format!("sqlite_home={value}")]);
        cmd.env("CODEX_SQLITE_HOME", sqlite);
    }
    cmd.args(args);
    if enabling {
        #[cfg(windows)]
        FileExt::unlock(&lease)?;
        FileExt::lock_shared(&lease)?;
    }
    crate::platform::keep_lease_across_exec(&lease)?;
    if let Some(snapshot) = &snapshot_lease {
        crate::platform::keep_lease_across_exec(snapshot)?;
    }
    drop(store);
    crate::platform::execute(cmd, lease)
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Browser SSO can authenticate the wrong account; logging into a fresh home lets us check the registered identity before replacing credentials and keeps the same login command retryable.
pub fn login(cli: &Cli, identifier: &str, device_auth: bool) -> Result<()> {
    let store = Store::open(cli)?;
    let account = store.resolve(identifier)?;
    let effective = store.effective_account(&account)?;
    let lease = store.lease(&effective.home, true)?;
    let snapshot_lease = (effective.home != account.home)
        .then(|| store.lease(&account.home, true))
        .transpose()?;
    if effective.home == store.data.main_home {
        crate::platform::ensure_codex_stopped(&store.codex_bin(cli))?;
    }
    let mut paths = vec![effective.home.join("auth.json")];
    if effective.home != account.home {
        paths.push(account.home.join("auth.json"));
    }
    let destination = crate::account_state::LoginDestination {
        account: account.clone(),
        effective_home: effective.home.clone(),
        previous: paths
            .into_iter()
            .map(|path| {
                let bytes = fsutil::optional_bytes(&path)?;
                Ok((path, bytes))
            })
            .collect::<Result<_>>()?,
    };
    let staging = fsutil::private_tempdir(&store.root, "login-")?;
    // Copy configuration instead of linking it: login must not update real settings.
    let config = effective.home.join("config.toml");
    if config.exists() {
        let mut copy = tempfile::NamedTempFile::new_in(staging.path())?;
        copy.write_all(&std::fs::read(config)?)?;
        copy.persist(staging.path().join("config.toml"))?;
    }
    let mut cmd = command(&store.codex_bin(cli), staging.path(), true)?;
    let sqlite = toml::Value::String(staging.path().to_string_lossy().into_owned()).to_string();
    cmd.args(["-c", &format!("sqlite_home={sqlite}")]);
    if let Some(expected) = &account.identity {
        eprintln!(
            "Sign in to account {} ({:?}). Choose this account in the browser.",
            account.number,
            expected.email.as_deref().unwrap_or(&expected.account_id)
        );
    } else {
        eprintln!("Sign in to new account {}.", account.number);
    }
    cmd.arg("login");
    // Keep xswap add --json stdout machine-readable, including during native sign-in.
    cmd.stdout(std::io::stderr());
    if device_auth {
        cmd.arg("--device-auth");
    }
    // Other accounts stay available while this browser/terminal login is in progress.
    crate::platform::keep_lease_across_exec(&lease)?;
    if let Some(snapshot) = &snapshot_lease {
        crate::platform::keep_lease_across_exec(snapshot)?;
    }
    drop(store);
    #[cfg(unix)]
    let status = cmd.status().context("could not start Codex login")?;
    #[cfg(windows)]
    let status = crate::platform::status(&mut cmd)?;
    if !status.success() {
        bail!(
            "Codex login did not complete; retry xswap login {}",
            account.number
        );
    }
    let (document, live) = auth::credentials(staging.path())?;
    crate::account_state::commit_login(cli, &destination, &document, live)?;
    drop(lease);
    drop(snapshot_lease);
    eprintln!("Account login saved.");
    Ok(())
}

pub fn validate_file_store(home: &Path) -> Result<()> {
    let path = home.join("config.toml");
    if !path.exists() {
        return Ok(());
    }
    let value: toml::Value = toml::from_str(&std::fs::read_to_string(path)?)
        .map_err(|_| anyhow::anyhow!("invalid Codex config.toml (contents omitted)"))?;
    if value
        .get("cli_auth_credentials_store")
        .is_some_and(|v| v.as_str() != Some("file"))
    {
        bail!(
            "adopting keyring/auto storage is not supported; use xswap add --login for a dedicated file-based login"
        );
    }
    Ok(())
}

pub fn prepare_main(home: &Path) -> Result<()> {
    if !home.exists() {
        fsutil::private_dir(home)?;
    }
    if !home.is_dir() {
        bail!("main Codex home is not a directory");
    }
    Ok(())
}
