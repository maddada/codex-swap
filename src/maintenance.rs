use crate::{
    cli::{Cli, Output},
    fsutil,
    store::Store,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    io::{IsTerminal, Write},
    path::Path,
    process::Command,
};

/// CDXC:AgentProviders 2026-09-06 DECISION:
/// The user requested built-in upgrade using Homebrew and a native Windows installer, without requiring Cargo.
pub fn upgrade() -> Result<()> {
    #[cfg(unix)]
    let status = Command::new("brew")
        .args(["upgrade", "maddada/tap/codex-swap"])
        .status()
        .context("could not start Homebrew; install Homebrew and the maddada/tap/codex-swap formula first")?;
    #[cfg(windows)]
    let status = {
        let scratch = fsutil::private_tempdir(&std::env::temp_dir(), "xswap-upgrade-")?;
        let script = scratch.path().join("install.ps1");
        fs::write(&script, include_str!("../scripts/install.ps1"))?;
        crate::platform::own_new(&script)?;
        crate::platform::private_permissions(&script, false)?;
        let executable = std::env::current_exe().context("resolve current xswap executable")?;
        let install_dir = executable
            .parent()
            .context("xswap executable has no parent directory")?;
        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg("-InstallDir")
            .arg(install_dir)
            .status()
            .context("could not start the Windows xswap installer")?
    };
    if !status.success() {
        bail!("upgrade failed ({status})");
    }
    Ok(())
}

fn real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    };
    #[cfg(unix)]
    let link = metadata.file_type().is_symlink();
    if link || !metadata.is_dir() {
        bail!(
            "purge requires a real managed directory: {}",
            path.display()
        );
    }
    Ok(())
}

/// CDXC:AgentProviders 2026-09-06 WHY:
/// Removing the registry lock file would allow a concurrent process to lock a new inode while purge still holds the old one.
/// Purge retains lock files and removes only the registry and managed account tree; shared links are unlinked without following their targets.
pub fn purge(cli: &Cli, yes: bool, output: &Output) -> Result<()> {
    let store = Store::open(cli)?;
    let profiles = store.root.join("accounts");
    let protected: Vec<_> = std::iter::once(&store.data.main_home)
        .chain(
            store
                .data
                .accounts
                .iter()
                .filter(|a| !a.managed)
                .map(|a| &a.home),
        )
        .collect();
    for home in &protected {
        let home = fsutil::absolute(home)?;
        if home.starts_with(&profiles) || profiles.starts_with(&home) {
            bail!(
                "purge refused: managed account tree overlaps an original or adopted Codex home ({})",
                home.display()
            );
        }
    }
    let mut homes = BTreeSet::from([store.data.main_home.clone()]);
    let mut managed_homes = Vec::new();
    for account in &store.data.accounts {
        homes.insert(account.home.clone());
        if account.managed && account.home.parent() != Some(profiles.as_path()) {
            bail!("purge refused: managed home is outside the owned account directory");
        }
    }
    match fs::symlink_metadata(&profiles) {
        Ok(_) => {
            real_directory(&profiles)?;
            for entry in fs::read_dir(&profiles)? {
                let home = entry?.path();
                real_directory(&home)?;
                if home.canonicalize()? != home {
                    bail!("purge refused: managed account path is not canonical");
                }
                homes.insert(home.clone());
                managed_homes.push(home);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    // Lock forgotten managed homes too, because remove intentionally retains their data.
    let _leases: Vec<_> = homes
        .iter()
        .map(|home| store.lease(home, true))
        .collect::<Result<_>>()?;
    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!(
                "purge requires interactive confirmation; use --yes to confirm deletion of xswap-managed credentials and history"
            );
        }
        eprintln!(
            "Delete the xswap registry and {} managed account home(s) under {}? Original/adopted homes and shared data will remain.",
            managed_homes.len(),
            store.root.display()
        );
        eprint!("Type purge to confirm: ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != "purge" {
            bail!("purge cancelled");
        }
    }
    // std::fs::remove_dir_all does not follow symbolic links, including shared history/config.
    for home in &managed_homes {
        fs::remove_dir_all(home)
            .with_context(|| format!("remove managed home {}", home.display()))?;
    }
    if profiles.exists() {
        fs::remove_dir(&profiles)?;
    }
    let registry = store.root.join("accounts.json");
    if fs::symlink_metadata(&registry).is_ok() {
        fsutil::regular(&registry)?;
        fs::remove_file(registry)?;
    }
    if output.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &json!({"schemaVersion": 1, "purged": true, "managedHomesRemoved": managed_homes.len(), "retainedHomes": protected, "retainedLockDirectory": store.root}),
        )?;
        println!();
    } else {
        println!(
            "Purged xswap registry and {} managed account home(s). Original/adopted homes and lock files retained.",
            managed_homes.len()
        );
    }
    Ok(())
}
