use crate::{
    cli::{Cli, ConfigAction, Output},
    store::Store,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};

fn value(store: &Store, key: &str) -> Result<Value> {
    match key {
        "codex-bin" => Ok(json!(
            store
                .data
                .preferences
                .codex_bin
                .as_deref()
                .unwrap_or("codex")
        )),
        "default-account" => Ok(store
            .data
            .default
            .map_or_else(|| json!("default"), |n| json!(n.to_string()))),
        _ => bail!("unknown preference {key:?}; supported keys: codex-bin, default-account"),
    }
}

fn emit(value: Value, output: &Output) -> Result<()> {
    if output.json {
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &json!({"schemaVersion": 1, "config": value}),
        )?;
        println!();
    } else if let Some(object) = value.as_object() {
        for (key, value) in object {
            println!(
                "{key} = {}",
                value.as_str().unwrap_or_default().escape_default()
            );
        }
    } else {
        println!("{}", value.as_str().unwrap_or_default().escape_default());
    }
    Ok(())
}

pub fn configure(cli: &Cli, action: Option<&ConfigAction>, output: &Output) -> Result<()> {
    let mut store = Store::open(cli)?;
    match action.unwrap_or(&ConfigAction::List) {
        ConfigAction::List => emit(
            json!({"codex-bin": value(&store, "codex-bin")?, "default-account": value(&store, "default-account")?}),
            output,
        ),
        ConfigAction::Path => emit(json!(store.root.join("accounts.json")), output),
        ConfigAction::Get { key } => emit(value(&store, key)?, output),
        ConfigAction::Set { key, value: new } => {
            match key.as_str() {
                "codex-bin" => {
                    if new.trim().is_empty() || new.contains('\0') {
                        bail!("codex-bin must be an executable name or path");
                    }
                    let path = std::path::Path::new(new);
                    let binary = if path.components().count() > 1 && !path.is_absolute() {
                        crate::fsutil::absolute(path)?
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        new.clone()
                    };
                    store.data.preferences.codex_bin = Some(binary);
                    store.save()?;
                }
                "default-account" => {
                    let account = store.selected(Some(new))?;
                    if let Some(account) = &account {
                        Store::require_enabled(account)?;
                        if new != "default" {
                            if account.identity.is_none() {
                                bail!(
                                    "account setup is incomplete; run xswap login {}",
                                    account.number
                                );
                            }
                            crate::auth::verify(&account.home, &account.identity)?;
                        }
                    }
                    store.data.default = if new == "default" {
                        None
                    } else {
                        account.map(|a| a.number)
                    };
                    store.save()?;
                }
                _ => {
                    value(&store, key)?;
                    unreachable!()
                }
            }
            emit(value(&store, key)?, output)
        }
        ConfigAction::Unset { key } => {
            match key.as_str() {
                "codex-bin" => store.data.preferences.codex_bin = None,
                "default-account" => store.data.default = None,
                _ => {
                    value(&store, key)?;
                    unreachable!()
                }
            }
            store.save()?;
            emit(value(&store, key)?, output)
        }
    }
}
