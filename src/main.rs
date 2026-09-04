#[cfg(not(unix))]
compile_error!("xswap currently supports macOS and Linux (including WSL)");

mod auth;
mod cli;
mod commands;
mod fsutil;
mod launch;
mod sharing;
mod store;

use clap::Parser;
use cli::{Action, Cli};

fn execute(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Action::Add(args) => commands::add(cli, args),
        Action::List(output) => commands::list(cli, output),
        Action::Status(output) => commands::status(cli, output),
        Action::Switch { account, output } => commands::switch(cli, account, output),
        Action::Remove { account, output } => commands::remove(cli, account, output),
        Action::Login {
            account,
            device_auth,
        } => launch::login(cli, account, *device_auth),
        Action::Run {
            account,
            share_history,
            args,
        } => launch::run(cli, account.as_deref(), *share_history, args),
    }
}

fn main() {
    let cli = Cli::parse_from(cli::arguments());
    if let Err(error) = execute(&cli) {
        eprintln!("xswap: {error:#}");
        std::process::exit(1);
    }
}
