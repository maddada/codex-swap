#[test]
fn new_account_transaction_handles_commit_failures_on_every_platform() {
    use crate::{
        account_state,
        cli::{Action, Cli, Output},
        fsutil::test_faults::{Point, inject},
        store::{Account, Store},
    };
    for existing in [false, true] {
        for rollback_failure in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let cli = Cli {
                data_dir: Some(temporary.path().join("store")),
                codex_home: Some(temporary.path().join("main")),
                codex_bin: None,
                command: Action::List(Output { json: true }),
            };
            let mut store = Store::open(&cli).unwrap();
            let registry = store.root.join("accounts.json");
            if existing {
                store.data.next_number = 42;
                store.save().unwrap();
            }
            let previous = crate::fsutil::optional_bytes(&registry)
                .unwrap()
                .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
            let directory = crate::fsutil::private_tempdir(&store.root, "synthetic-").unwrap();
            let home = directory.path().to_owned();
            crate::fsutil::atomic_json(
                &home.join("auth.json"),
                &serde_json::json!({"synthetic": true}),
            )
            .unwrap();
            store.data.accounts.push(Account {
                number: 1,
                alias: None,
                home: home.clone(),
                managed: true,
                share_history: false,
                identity: None,
                enabled: true,
            });
            let points = if rollback_failure {
                vec![
                    Point::AfterCommit,
                    if existing {
                        Point::BeforeCommit
                    } else {
                        Point::RollbackRemove
                    },
                ]
            } else {
                vec![Point::AfterCommit]
            };
            let faults = inject(&registry, &points);
            let error = format!(
                "{:#}",
                account_state::commit_new_accounts(&store, vec![directory]).unwrap_err()
            );
            assert!(!faults.commits().is_empty());
            assert_eq!(home.join("auth.json").exists(), rollback_failure);
            if rollback_failure {
                assert!(error.contains(&home.display().to_string()));
            } else {
                let current = crate::fsutil::optional_bytes(&registry)
                    .unwrap()
                    .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
                assert_eq!(current, previous);
            }
        }
    }
}
