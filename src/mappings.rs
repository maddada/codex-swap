use crate::{
    cli::{Cli, Output},
    store::Store,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};

fn directory(path: Option<&Path>, must_exist: bool) -> Result<PathBuf> {
    let current = std::env::current_dir()?;
    let path = path.unwrap_or(&current);
    let resolved = crate::fsutil::absolute(path)?;
    if must_exist && !resolved.is_dir() {
        bail!("mapping target must be an existing directory");
    }
    Ok(resolved)
}

fn emit(store: &Store, output: &Output) -> Result<()> {
    let mappings: Vec<_> = store.data.directory_mappings.iter().map(|(path, number)| {
        let account = store.data.accounts.iter().find(|a| a.number == *number).unwrap();
        json!({"directory": path, "number": number, "alias": account.alias, "enabled": account.enabled})
    }).collect();
    if output.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &json!({"schemaVersion": 1, "mappings": mappings}),
        )?;
        println!();
    } else if mappings.is_empty() {
        println!("No directory mappings. Use xswap map ACCOUNT [DIRECTORY].");
    } else {
        for (path, number) in &store.data.directory_mappings {
            println!(
                "{}  {}",
                number,
                path.display().to_string().escape_default()
            );
        }
    }
    Ok(())
}

pub fn map(
    cli: &Cli,
    identifier: Option<&str>,
    path: Option<&Path>,
    output: &Output,
) -> Result<()> {
    let mut store = Store::open(cli)?;
    if let Some(identifier) = identifier {
        let account = if identifier == "default" {
            store
                .main_account()
                .context("register the original Codex home with xswap add before mapping it")?
        } else {
            store.resolve(identifier)?
        };
        Store::require_enabled(&account)?;
        let directory = directory(path, true)?;
        store
            .data
            .directory_mappings
            .insert(directory, account.number);
        store.save()?;
    }
    emit(&store, output)
}

pub fn unmap(cli: &Cli, path: Option<&Path>, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    let directory = directory(path, false)?;
    if store.data.directory_mappings.remove(&directory).is_none() {
        bail!(
            "no mapping on this exact directory; inherited mappings belong to their parent directory"
        );
    }
    store.save()?;
    emit(&store, output)
}
