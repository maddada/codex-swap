//! Codex processes that stand in the way of a command, and ending them once the user agrees.
use crate::cli::StopCodex;
use anyhow::{Result, bail};
use std::{
    io::{IsTerminal, Write},
    path::Path,
    time::{Duration, Instant},
};

/// CDXC:AgentProviders 2026-09-28 SEE-ALSO:
/// Ghostex's account login (server/src/accounts/setup.rs) reads "Codex is running (PID …)" from this refusal to offer sleeping the sessions that own those processes; keep the wording stable.
pub fn clear(
    shown: &[u32],
    targets: &[u32],
    stop: StopCodex,
    why: &str,
    rescan: impl Fn() -> Result<Vec<u32>>,
) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }
    let list = join(if shown.is_empty() { targets } else { shown });
    let agreed = match stop {
        StopCodex::Always => true,
        StopCodex::Never => false,
        StopCodex::Ask => {
            std::io::stdin().is_terminal()
                && std::io::stderr().is_terminal()
                && ask(&format!(
                    "Codex is running (PID {list}) and {why}.\nEnd these Codex processes and continue? [y/N] "
                ))?
        }
    };
    if !agreed {
        bail!(
            "Codex is running (PID {list}) and {why}. Close those Codex sessions, or rerun with --stop-codex to end them"
        );
    }
    for pid in targets {
        terminate(*pid);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = rescan()?;
        if remaining.is_empty() {
            eprintln!("Ended the Codex processes that were in the way.");
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "Codex is running (PID {}) and did not exit when asked to stop. Close it and try again",
                join(&remaining)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn join(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn ask(question: &str) -> Result<bool> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(unix)]
fn terminate(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(windows)]
fn terminate(pid: u32) {
    // Console Codex ignores WM_CLOSE, so a polite taskkill would never end it.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

/// Processes holding an account lease; Codex launched by xswap inherits it for its whole run.
#[cfg(target_os = "macos")]
pub fn lease_holders(path: &Path) -> Vec<u32> {
    let Ok(output) = std::process::Command::new("/usr/sbin/lsof")
        .args(["-t", "--"])
        .arg(path)
        .env("LC_ALL", "C")
        .output()
    else {
        return Vec::new();
    };
    pids_except_self(String::from_utf8_lossy(&output.stdout).lines())
}

#[cfg(target_os = "linux")]
pub fn lease_holders(path: &Path) -> Vec<u32> {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for process in processes.flatten() {
        let name = process.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        if descriptors
            .flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| target == path))
        {
            holders.push(pid);
        }
    }
    holders.retain(|pid| *pid != std::process::id());
    holders
}

/// Without a holder query the caller keeps its plain "busy" refusal.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn lease_holders(_path: &Path) -> Vec<u32> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn pids_except_self<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<u32> {
    let mut pids: Vec<u32> = lines
        .filter_map(|line| line.trim().parse().ok())
        .filter(|pid| *pid != std::process::id())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}
