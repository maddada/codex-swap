use anyhow::{Context, Result, bail};
use std::process::Command;
#[cfg(unix)]
use std::{
    fs,
    io::ErrorKind,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
};

#[cfg(unix)]
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
#[cfg(unix)]
const FORMULA: &str = "maddada/tap/codex-swap";
/// Written beside the executable by scripts/install.sh.
#[cfg(unix)]
const RECEIPT: &str = ".xswap-install-receipt.json";
#[cfg(unix)]
const INSTALLER: &str = include_str!("../scripts/install.sh");

/// CDXC:Release 2026-09-29 DECISION:
/// User: macOS and Linux get a one-command installer like Windows' install.ps1 (`curl -fsSL …/install.sh | sh`), and `xswap upgrade` must recognise it.
/// The receipt counts only when its recorded SHA-256 matches the running executable, which proves this exact file came from the script. That makes it safe to check first: a Cargo root such as `~/.local` can hold both a `.crates.toml` entry and a later script install of the same binary name, and only the receipt knows which one wrote the file.
#[cfg(unix)]
fn script_install_dir(executable: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let directory = executable.parent()?;
    let receipt: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(directory.join(RECEIPT)).ok()?).ok()?;
    if receipt.get("method")?.as_str()? != "script" {
        return None;
    }
    let recorded = Path::new(receipt.get("installDir")?.as_str()?)
        .canonicalize()
        .ok()?;
    if recorded != directory {
        return None;
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut fs::File::open(executable).ok()?, &mut hasher).ok()?;
    let actual = format!("{:x}", hasher.finalize());
    actual
        .eq_ignore_ascii_case(receipt.get("sha256")?.as_str()?)
        .then(|| directory.to_path_buf())
}

/// Reruns the bundled installer for the directory holding the current executable, as the Windows
/// upgrade reruns install.ps1.
#[cfg(unix)]
fn run_installer(directory: &Path) -> Result<()> {
    let scratch = crate::fsutil::private_tempdir(&std::env::temp_dir(), "xswap-upgrade-")?;
    let script = scratch.path().join("install.sh");
    fs::write(&script, INSTALLER)?;
    let status = Command::new("/bin/sh")
        .arg(&script)
        .env("XSWAP_INSTALL_DIR", directory)
        .stdin(Stdio::null())
        .status()
        .context("could not start the xswap installer")?;
    if !status.success() {
        bail!("upgrade failed ({status})");
    }
    Ok(())
}

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
    if let Some(directory) = script_install_dir(&executable) {
        return run_installer(&directory);
    }
    let Some(mut command) = install_command(&executable)? else {
        bail!(
            "Could not detect install method (looked for the install script / Cargo / Homebrew).\n\
             Executable: {}\n\
             To upgrade manually, run one of:\n  \
             curl -fsSL {REPOSITORY}/releases/latest/download/install.sh | sh\n  \
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
