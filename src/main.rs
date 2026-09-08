mod account_state;
mod auth;
mod backup;
mod cli;
mod commands;
mod fsutil;
mod launch;
mod maintenance;
mod mappings;
mod platform;
mod preferences;
mod sharing;
mod store;
mod upgrade;
mod usage;
mod usage_client;
mod usage_model;

use clap::Parser;
use cli::{Action, Cli};

fn execute(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Action::Add(args) => commands::add(cli, args),
        Action::List(output) => commands::list(cli, output),
        Action::Usage {
            account,
            all,
            output,
        } => usage::show(cli, account.as_deref(), *all, output),
        Action::Rename {
            account,
            alias,
            output,
            ..
        } => commands::rename(cli, account, alias.clone(), output),
        Action::Move {
            account,
            slot,
            output,
        } => commands::move_slot(cli, account, *slot, output),
        Action::Swap {
            account,
            other,
            output,
        } => commands::swap(cli, account, other, output),
        Action::Enable { account, output } => commands::set_enabled(cli, account, true, output),
        Action::Disable { account, output } => commands::set_enabled(cli, account, false, output),
        Action::Export {
            file,
            account,
            output,
        } => backup::export(cli, file, account.as_deref(), output),
        Action::Import {
            file,
            remap_slots,
            output,
        } => backup::import(cli, file, *remap_slots, output),
        Action::Map {
            account,
            directory,
            output,
        } => mappings::map(cli, account.as_deref(), directory.as_deref(), output),
        Action::Unmap { directory, output } => mappings::unmap(cli, directory.as_deref(), output),
        Action::Config { action, output } => preferences::configure(cli, action.as_ref(), output),
        Action::Upgrade => upgrade::run(),
        Action::Purge { yes, output } => maintenance::purge(cli, *yes, output),
        Action::Status(output) => commands::status(cli, output),
        Action::Switch { account, output } => commands::switch(cli, account.as_deref(), output),
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
