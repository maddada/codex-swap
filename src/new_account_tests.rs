//! Exercise the real import and add --login paths using isolated synthetic accounts.
use crate::{
    auth, backup,
    cli::{Action, Add, Cli, Output},
    commands, fsutil,
    fsutil::test_faults::{self, Point},
    store::{Account, Store},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn credentials(name: &str) -> Value {
    let claims =
        URL_SAFE_NO_PAD.encode(json!({"email": format!("{name}@example.invalid")}).to_string());
    json!({"auth_mode": "chatgpt", "tokens": {
        "account_id": name, "access_token": "synthetic_access",
        "refresh_token": "synthetic_refresh", "id_token": format!("header.{claims}.signature")
    }})
}

struct Fixture {
    _temporary: tempfile::TempDir,
    cli: Cli,
    prior: Option<Value>,
    existing_home: Option<PathBuf>,
    existing_auth: Option<Vec<u8>>,
}

impl Fixture {
    fn new(existing: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let mut cli = Cli {
            data_dir: Some(temporary.path().join("store")),
            codex_home: Some(temporary.path().join("main")),
            codex_bin: None,
            command: Action::List(Output { json: true }),
        };
        let mut store = Store::open(&cli).unwrap();
        cli.data_dir = Some(store.root.clone());
        fsutil::private_dir(&store.root.join("accounts")).unwrap();
        let existing_home = existing.then(|| {
            let home = store.root.join("accounts/7-existing");
            fsutil::private_dir(&home).unwrap();
            fsutil::atomic_json(&home.join("auth.json"), &credentials("existing")).unwrap();
            fs::write(home.join("history.marker"), b"synthetic existing history").unwrap();
            store.data.accounts.push(Account {
                number: 7,
                alias: Some("existing".into()),
                home: home.clone(),
                managed: true,
                share_history: false,
                identity: Some(auth::require(&home).unwrap()),
                enabled: false,
            });
            store.data.default = Some(7);
            store.data.original_account = Some(7);
            store.data.next_number = 42;
            store
                .data
                .directory_mappings
                .insert(temporary.path().join("mapped"), 7);
            store.data.preferences.codex_bin = Some("synthetic-codex".into());
            store.save().unwrap();
            home
        });
        let existing_auth = existing_home
            .as_ref()
            .map(|home| fs::read(home.join("auth.json")).unwrap());
        let prior = fsutil::optional_bytes(&store.root.join("accounts.json"))
            .unwrap()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap());
        drop(store);
        Self {
            _temporary: temporary,
            cli,
            prior,
            existing_home,
            existing_auth,
        }
    }

    fn registry(&self) -> PathBuf {
        self.cli.data_dir.as_ref().unwrap().join("accounts.json")
    }

    fn homes(&self) -> BTreeSet<PathBuf> {
        fs::read_dir(self.cli.data_dir.as_ref().unwrap().join("accounts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    fn assert_existing_unchanged(&self) {
        if let Some(home) = &self.existing_home {
            assert_eq!(
                fs::read(home.join("auth.json")).unwrap(),
                *self.existing_auth.as_ref().unwrap()
            );
            assert_eq!(
                fs::read(home.join("history.marker")).unwrap(),
                b"synthetic existing history"
            );
        }
        assert!(
            !fs::read_dir(self.cli.data_dir.as_ref().unwrap())
                .unwrap()
                .any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("new-login-")
                })
        );
    }

    fn assert_registry_restored(&self) {
        let current = fsutil::optional_bytes(&self.registry())
            .unwrap()
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap());
        assert_eq!(current, self.prior);
        self.assert_existing_unchanged();
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Import,
    AddLogin,
}

impl Operation {
    fn run(self, fixture: &mut Fixture) -> anyhow::Result<()> {
        match self {
            Self::Import => {
                let file = fixture._temporary.path().join("backup.json");
                fsutil::atomic_json(
                    &file,
                    &json!({
                        "format": "codex-swap-account-backup", "schemaVersion": 1, "default": 3,
                        "accounts": [
                            {"number": 5, "alias": "five", "enabled": false, "shareHistory": false, "auth": credentials("five")},
                            {"number": 3, "alias": "three", "enabled": true, "shareHistory": false, "auth": credentials("three")}
                        ]
                    }),
                )?;
                backup::import(&fixture.cli, &file, false, &Output { json: true })
            }
            Self::AddLogin => {
                let file = fixture._temporary.path().join("login-auth.json");
                fsutil::atomic_json(&file, &credentials("new-login"))?;
                let mock = fixture._temporary.path().join("mock-codex");
                let quoted = file.display().to_string().replace('\'', "'\\''");
                fs::write(
                    &mock,
                    format!(
                        "#!/bin/sh\n/bin/cp '{quoted}' \"$CODEX_HOME/auth.json\"\n/bin/chmod 600 \"$CODEX_HOME/auth.json\"\n"
                    ),
                )?;
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&mock, fs::Permissions::from_mode(0o700))?;
                fixture.cli.codex_bin = Some(mock.into_os_string());
                commands::add(
                    &fixture.cli,
                    &Add {
                        alias: Some("new-login".into()),
                        slot: None,
                        home: None,
                        login: true,
                        email: Some("new-login@example.invalid".into()),
                        device_auth: false,
                        share_history: false,
                        output: Output { json: true },
                    },
                )
            }
        }
    }

    fn assert_committed(self, fixture: &Fixture, registry: &Value) -> Vec<PathBuf> {
        let accounts = registry["accounts"].as_array().unwrap();
        let homes: Vec<_> = accounts
            .iter()
            .filter(|account| account["alias"] != "existing")
            .map(|account| PathBuf::from(account["home"].as_str().unwrap()))
            .collect();
        match self {
            Self::Import => {
                assert_eq!(homes.len(), 2);
                let numbers: Vec<_> = accounts
                    .iter()
                    .map(|account| account["number"].as_u64().unwrap())
                    .collect();
                assert_eq!(
                    numbers,
                    if fixture.prior.is_some() {
                        vec![3, 5, 7]
                    } else {
                        vec![3, 5]
                    }
                );
                assert_eq!(
                    registry["default"],
                    if fixture.prior.is_some() {
                        json!(7)
                    } else {
                        json!(3)
                    }
                );
                assert_eq!(
                    registry["nextNumber"],
                    if fixture.prior.is_some() {
                        json!(42)
                    } else {
                        json!(6)
                    }
                );
            }
            Self::AddLogin => {
                assert_eq!(homes.len(), 1);
                assert_eq!(
                    accounts.last().unwrap()["number"],
                    if fixture.prior.is_some() {
                        json!(42)
                    } else {
                        json!(1)
                    }
                );
                assert_eq!(
                    registry["default"],
                    if fixture.prior.is_some() {
                        json!(7)
                    } else {
                        Value::Null
                    }
                );
                assert_eq!(
                    registry["nextNumber"],
                    if fixture.prior.is_some() {
                        json!(43)
                    } else {
                        json!(2)
                    }
                );
            }
        }
        if let Some(prior) = &fixture.prior {
            for key in [
                "mainHome",
                "originalAccount",
                "directoryMappings",
                "preferences",
            ] {
                assert_eq!(registry[key], prior[key]);
            }
            assert_eq!(
                accounts
                    .iter()
                    .find(|account| account["number"] == 7)
                    .unwrap(),
                &prior["accounts"][0]
            );
        }
        homes
    }
}

fn assert_credentials(home: &Path) {
    let (document, identity) = auth::credentials(home).unwrap();
    assert_eq!(document, credentials(&identity.account_id));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(home.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn postcommit_failure_rolls_back_both_new_account_paths_and_allows_retry() {
    for operation in [Operation::Import, Operation::AddLogin] {
        for existing in [false, true] {
            let mut fixture = Fixture::new(existing);
            let before = fixture.homes();
            let faults = test_faults::inject(&fixture.registry(), &[Point::AfterCommit]);
            let error = operation.run(&mut fixture).unwrap_err();
            assert!(error.to_string().contains("injected AfterCommit failure"));
            let commits = faults.commits();
            assert_eq!(commits.len(), if existing { 2 } else { 1 });
            let registry = serde_json::from_slice(&commits[0]).unwrap();
            let staged = operation.assert_committed(&fixture, &registry);
            assert!(staged.iter().all(|home| !home.exists()));
            fixture.assert_registry_restored();
            assert_eq!(fixture.homes(), before);
            drop(faults);
            operation.run(&mut fixture).unwrap();
            let registry = serde_json::from_slice(&fs::read(fixture.registry()).unwrap()).unwrap();
            for home in operation.assert_committed(&fixture, &registry) {
                assert_credentials(&home);
            }
            fixture.assert_existing_unchanged();
        }
    }
}

#[test]
fn rollback_failure_retains_every_new_home_and_reports_recovery_paths() {
    for operation in [Operation::Import, Operation::AddLogin] {
        for existing in [false, true] {
            for rollback in if existing {
                vec![Point::BeforeCommit, Point::AfterCommit]
            } else {
                vec![Point::RollbackRemove, Point::RollbackSync]
            } {
                let mut fixture = Fixture::new(existing);
                let faults =
                    test_faults::inject(&fixture.registry(), &[Point::AfterCommit, rollback]);
                let error = format!("{:#}", operation.run(&mut fixture).unwrap_err());
                assert!(error.contains("account rollback also failed"));
                let commits = faults.commits();
                if rollback == Point::RollbackSync {
                    assert!(
                        !fixture.registry().exists(),
                        "registry was actually unlinked"
                    );
                    assert_eq!(commits.len(), 1);
                    assert!(error.contains("injected RollbackSync failure"));
                }
                let registry = serde_json::from_slice(&commits[0]).unwrap();
                for home in operation.assert_committed(&fixture, &registry) {
                    assert_credentials(&home);
                    assert!(
                        error.contains(&home.display().to_string()),
                        "missing recovery path: {error}"
                    );
                }
                let store = Store::open(&fixture.cli).unwrap();
                for account in &store.data.accounts {
                    assert_credentials(&account.home);
                }
                fixture.assert_existing_unchanged();
            }
        }
    }
}

#[test]
fn precommit_failure_cleans_both_new_account_paths() {
    for operation in [Operation::Import, Operation::AddLogin] {
        for existing in [false, true] {
            let mut fixture = Fixture::new(existing);
            let before = fixture.homes();
            let faults = test_faults::inject(&fixture.registry(), &[Point::BeforeCommit]);
            assert!(
                operation
                    .run(&mut fixture)
                    .unwrap_err()
                    .to_string()
                    .contains("injected BeforeCommit failure")
            );
            assert_eq!(faults.commits().len(), usize::from(existing));
            fixture.assert_registry_restored();
            assert_eq!(fixture.homes(), before);
        }
    }
}

#[test]
fn validation_failure_cleans_every_staged_import_and_login_home() {
    let fixture = Fixture::new(false);
    let backup = fixture._temporary.path().join("backup.json");
    fsutil::atomic_json(&backup, &json!({
        "format": "codex-swap-account-backup", "schemaVersion": 1, "default": 3,
        "accounts": [
            {"number": 5, "alias": "same", "enabled": true, "shareHistory": false, "auth": credentials("five")},
            {"number": 3, "alias": "same", "enabled": true, "shareHistory": false, "auth": credentials("three")}
        ]
    })).unwrap();
    assert!(backup::import(&fixture.cli, &backup, false, &Output { json: true }).is_err());
    fixture.assert_registry_restored();
    assert!(fixture.homes().is_empty());
    // A duplicate identity is detected after login, while the login stage is still owned.
    let mut fixture = Fixture::new(true);
    let mut store = Store::open(&fixture.cli).unwrap();
    let home = &store.data.accounts[0].home;
    fsutil::atomic_json(&home.join("auth.json"), &credentials("new-login")).unwrap();
    store.data.accounts[0].identity = Some(auth::require(home).unwrap());
    store.save().unwrap();
    fixture.prior = Some(serde_json::to_value(&store.data).unwrap());
    fixture.existing_auth = Some(fs::read(store.data.accounts[0].home.join("auth.json")).unwrap());
    drop(store);
    let before = fixture.homes();
    assert!(
        Operation::AddLogin
            .run(&mut fixture)
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    fixture.assert_registry_restored();
    assert_eq!(fixture.homes(), before);
}
