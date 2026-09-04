use crate::{auth, cli::Cli, fsutil, sharing, store::Store};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    ffi::OsString,
    fs::File,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::Command,
};

fn command(cli: &Cli, home: &Path, file_auth: bool) -> Command {
    let mut cmd = Command::new(&cli.codex_bin);
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
    cmd
}

fn keep_lease_across_exec(file: &File) -> Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("retain account lease during Codex execution");
    }
    Ok(())
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
    let mut selected = store.selected(identifier)?;
    let home = selected
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
    if let Some(account) = selected.as_mut() {
        if account.identity.is_none() {
            bail!(
                "account setup is incomplete; run xswap login {}",
                account.number
            );
        }
        auth::verify(&home, &account.identity)?;
        if account.managed {
            sharing::config(&store.data.main_home, &home)?;
        }
        if shared {
            sharing::history(&store.data.main_home, &home)?;
            if !account.share_history {
                account.share_history = true;
                store.replace(account.clone())?;
            }
        }
    }
    let mut cmd = command(cli, &home, selected.is_some());
    if shared && home != store.data.main_home {
        let sqlite = sharing::sqlite_home(&store.data.main_home)?;
        let value = toml::Value::String(sqlite.to_string_lossy().into_owned()).to_string();
        cmd.args(["-c", &format!("sqlite_home={value}")]);
        cmd.env("CODEX_SQLITE_HOME", sqlite);
    }
    cmd.args(args);
    if enabling {
        FileExt::lock_shared(&lease)?;
    }
    keep_lease_across_exec(&lease)?;
    drop(store);
    // exec preserves the terminal, PID, signals and exact Codex exit status.
    Err(cmd.exec()).context("could not execute Codex; install it or set XSWAP_CODEX_BIN")
}

pub fn login(cli: &Cli, identifier: &str, device_auth: bool) -> Result<()> {
    let store = Store::open(cli)?;
    let mut account = store.resolve(identifier)?;
    let lease = store.lease(&account.home, true)?;
    // Refuse a known wrong login before asking Codex to mutate its directory.
    if auth::identity(&account.home)?.is_some() {
        auth::verify(&account.home, &account.identity)?;
    }
    let mut cmd = command(cli, &account.home, true);
    cmd.arg("login");
    // Keep xswap add --json stdout machine-readable, including during native sign-in.
    cmd.stdout(std::io::stderr());
    if device_auth {
        cmd.arg("--device-auth");
    }
    // Other accounts stay available while this browser/terminal login is in progress.
    keep_lease_across_exec(&lease)?;
    drop(store);
    let status = cmd.status().context("could not start Codex login")?;
    if !status.success() {
        bail!(
            "Codex login did not complete; retry xswap login {}",
            account.number
        );
    }
    let live = auth::verify(&account.home, &account.identity)?;
    let mut store = Store::open(cli)?;
    store.ensure_unique_identity(&live, account.number)?;
    account.identity = Some(live);
    store.replace(account)?;
    drop(lease);
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
