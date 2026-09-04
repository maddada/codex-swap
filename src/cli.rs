use clap::{Args, Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};

/// CDXC:AgentProviders 2026-09-05 DECISION:
/// The executable is xswap; implement the user's cswap account-launch and shared-history workflow, with usage polling and auto-swap policy owned by gxserver.
#[derive(Parser)]
#[command(
    name = "xswap",
    version,
    about = "Run Codex with separate accounts and shared conversations"
)]
pub struct Cli {
    /// Account registry directory (never put this inside a source repository).
    #[arg(long, env = "XSWAP_HOME", global = true)]
    pub data_dir: Option<PathBuf>,
    /// Main Codex home, remembered when the account registry is created.
    #[arg(long, env = "XSWAP_CODEX_HOME", global = true)]
    pub codex_home: Option<PathBuf>,
    /// Official Codex executable, resolved on PATH by default.
    #[arg(long, env = "XSWAP_CODEX_BIN", default_value = "codex", global = true)]
    pub codex_bin: OsString,
    #[command(subcommand)]
    pub command: Action,
}

#[derive(Subcommand)]
pub enum Action {
    /// Register the current login in place, or sign into a new isolated account.
    Add(Add),
    /// List accounts, identities, login state and launch directories (no quota requests).
    List(Output),
    /// Show the account selected for future xswap run launches.
    Status(Output),
    /// Select the default for future xswap launches; existing sessions keep their account.
    Switch {
        /// Slot number, alias, email, or 'default' for the original Codex home.
        account: String,
        #[command(flatten)]
        output: Output,
    },
    /// Forget an account without deleting credentials or conversation files.
    Remove {
        account: String,
        #[command(flatten)]
        output: Output,
    },
    /// Sign in again using Codex's own interactive login.
    Login {
        account: String,
        #[arg(long)]
        device_auth: bool,
    },
    /// Launch the selected account; everything after -- goes directly to Codex.
    Run {
        account: Option<String>,
        /// Share transcripts, archives, history, session index and SQLite state.
        #[arg(long)]
        share_history: bool,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

#[derive(Args)]
pub struct Add {
    #[arg(long)]
    pub alias: Option<String>,
    /// Assign an unused positive slot number.
    #[arg(long)]
    pub slot: Option<u32>,
    /// Adopt a logged-in Codex home without copying its credentials.
    #[arg(long, conflicts_with = "login")]
    pub home: Option<PathBuf>,
    /// Create a permanent account home and open Codex's sign-in flow.
    #[arg(long)]
    pub login: bool,
    #[arg(long, requires = "login")]
    pub device_auth: bool,
    #[arg(long)]
    pub share_history: bool,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct Output {
    #[arg(long)]
    pub json: bool,
}

/// Accept the exact status/switch spellings used by the user's cs shell wrapper.
pub fn arguments() -> Vec<OsString> {
    let mut args: Vec<_> = std::env::args_os().collect();
    for arg in args.iter_mut().skip(1) {
        if arg == "--" {
            break;
        }
        if arg == "--status" {
            *arg = "status".into();
        } else if arg == "--switch-to" {
            *arg = "switch".into();
        }
    }
    args
}
