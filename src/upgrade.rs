use anyhow::{Context, Result, bail};
use std::process::Command;
#[cfg(unix)]
use std::{fs, io::ErrorKind, os::unix::process::CommandExt, path::Path};

#[cfg(unix)]
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
#[cfg(unix)]
const FORMULA: &str = "maddada/tap/codex-swap";

#[cfg(unix)]
fn install_command(executable: &Path) -> Result<Option<Command>> {
    let Some(bin) = executable.parent().filter(|path| path.ends_with("bin")) else {
        return Ok(None);
    };
    let Some(root) = bin.parent() else {
        return Ok(None);
    };

    if root
        .parent()
        .is_some_and(|path| path.ends_with("Cellar/codex-swap"))
        && root.join("INSTALL_RECEIPT.json").is_file()
    {
        let mut command = Command::new("brew");
        command.args(["upgrade", FORMULA]);
        return Ok(Some(command));
    }

    let manifest = root.join(".crates.toml");
    let contents = match fs::read_to_string(&manifest) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", manifest.display())),
    };
    let installed: toml::Value =
        toml::from_str(&contents).with_context(|| format!("parse {}", manifest.display()))?;
    let registered = installed
        .get("v1")
        .and_then(toml::Value::as_table)
        .is_some_and(|packages| {
            packages.iter().any(|(package, binaries)| {
                package.starts_with("codex-swap ")
                    && binaries.as_array().is_some_and(|binaries| {
                        binaries
                            .iter()
                            .any(|binary| binary.as_str() == Some("xswap"))
                    })
            })
        });
    if !registered {
        return Ok(None);
    }

    let mut command = Command::new("cargo");
    command
        .args([
            "install", "--git", REPOSITORY, "--locked", "--force", "--root",
        ])
        .arg(root);
    Ok(Some(command))
}

/// CDXC:AgentProviders 2026-09-08 DECISION:
/// User: keep both Cargo and Homebrew upgrade support, with cswap-compatible spellings and exit status handling.
/// This supersedes unconditional Homebrew upgrades on Unix; the native Windows installer remains supported.
#[cfg(unix)]
pub fn run() -> Result<()> {
    let executable = std::env::current_exe()
        .context("locate xswap executable")?
        .canonicalize()
        .context("resolve xswap executable")?;
    let Some(mut command) = install_command(&executable)? else {
        bail!(
            "Could not detect install method (looked for Cargo / Homebrew).\n\
             Executable: {}\n\
             To upgrade manually, run one of:\n  \
             cargo install --git {REPOSITORY} --locked --force\n  \
             brew upgrade {FORMULA}\n\
             For a source checkout, use `git pull` then `cargo install --path . --locked --force`.\n\
             Keep the original `--root` when using a custom Cargo install directory.",
            executable.display()
        );
    };

    let manager = command.get_program().to_string_lossy().into_owned();
    let error = command.exec();
    if error.kind() == ErrorKind::NotFound {
        bail!(
            "Detected {manager} install but `{manager}` is not on PATH. \
             Run the upgrade manually from a shell where it is available."
        );
    }
    Err(error).with_context(|| format!("could not execute {manager} upgrade"))
}

#[cfg(windows)]
pub fn run() -> Result<()> {
    let status = {
        let scratch = crate::fsutil::private_tempdir(&std::env::temp_dir(), "xswap-upgrade-")?;
        let script = scratch.path().join("install.ps1");
        std::fs::write(&script, include_str!("../scripts/install.ps1"))?;
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
