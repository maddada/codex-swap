//! Operating-system boundaries for account privacy and child execution.
mod process_guard;
pub use process_guard::ensure_codex_stopped;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
pub fn keep_lease_across_exec(file: &std::fs::File) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context("retain account lease during Codex execution");
    }
    Ok(())
}

#[cfg(unix)]
pub fn codex_command(binary: &std::ffi::OsStr) -> anyhow::Result<std::process::Command> {
    Ok(std::process::Command::new(binary))
}

#[cfg(unix)]
pub fn execute(mut cmd: std::process::Command, _lease: std::fs::File) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::unix::process::CommandExt;
    Err(cmd.exec()).context("could not execute Codex; install it or set XSWAP_CODEX_BIN")
}
