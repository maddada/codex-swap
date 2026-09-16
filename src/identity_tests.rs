use super::*;
use crate::cli::{Action, Output};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

struct Fixture {
    directory: tempfile::TempDir,
    cli: Cli,
}

impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let main = directory.path().join("main");
        fsutil::private_dir(&main)?;
        let cli = Cli {
            data_dir: Some(directory.path().join("registry")),
            codex_home: Some(main),
            codex_bin: Some("xswap-identity-test".into()),
            command: Action::Status(Output { json: false }),
        };
        Ok(Self { directory, cli })
    }

    fn home(&self, name: &str, document: &Value) -> Result<PathBuf> {
        let home = self.directory.path().join(name);
        fsutil::private_dir(&home)?;
        fsutil::atomic_json(&home.join("auth.json"), document)?;
        Ok(home)
    }
}

fn credentials(claims: Value) -> Value {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "account_id": "workspace-1",
            "access_token": "dummy-access-token",
            "refresh_token": "dummy-refresh-token",
            "id_token": format!("e30.{payload}.ZHVtbXktc2lnbmF0dXJl")
        }
    })
}

fn login(user: &str, email: &str) -> Value {
    credentials(json!({
        "https://api.openai.com/profile": { "email": email },
        "https://api.openai.com/auth": {
            "chatgpt_user_id": user,
            "chatgpt_plan_type": "team"
        }
    }))
}

fn saved(
    store: &mut Store,
    source: &Path,
    alias: Option<String>,
    slot: Option<u32>,
) -> Result<Account> {
    let mut transaction = Transaction::new();
    let account = capture(store, &mut transaction, source, alias, slot, false)?;
    transaction.finish(Ok(()))?;
    Ok(account)
}

#[test]
fn claims_follow_codex_precedence_and_keep_unknown_owners_unknown() -> Result<()> {
    let fixture = Fixture::new()?;
    let cases = [
        (
            json!({ "email": "top@example.test", "https://api.openai.com/profile": {"email": "profile@example.test"}, "https://api.openai.com/auth": {"chatgpt_user_id": "primary", "user_id": "fallback"} }),
            Some("top@example.test"),
            Some("primary"),
        ),
        (
            json!({ "email": null, "https://api.openai.com/profile": {"email": "profile@example.test"}, "https://api.openai.com/auth": {"chatgpt_user_id": null, "user_id": "fallback"} }),
            Some("profile@example.test"),
            Some("fallback"),
        ),
        (
            json!({ "email": " ", "https://api.openai.com/profile": {"email": "profile@example.test"}, "https://api.openai.com/auth": {"chatgpt_user_id": "primary"} }),
            None,
            Some("primary"),
        ),
        (
            json!({ "email": "top@example.test", "https://api.openai.com/auth": {"chatgpt_user_id": " ", "user_id": "fallback"} }),
            Some("top@example.test"),
            None,
        ),
    ];
    for (claims, email, user) in cases {
        let home = fixture.home("claims", &credentials(claims))?;
        let identity = auth::require(&home)?;
        assert_eq!(identity.email.as_deref(), email);
        assert_eq!(identity.user_id.as_deref(), user);
    }
    for claims in [
        json!({}),
        json!({"email": null}),
        json!({"email": " ", "https://api.openai.com/auth": {"chatgpt_user_id": "", "user_id": "fallback"}}),
    ] {
        let home = fixture.home("unknown", &credentials(claims))?;
        assert!(auth::credentials(&home).is_err());
    }
    Ok(())
}

#[test]
fn owner_comparison_uses_workspace_and_stable_user_before_labels() -> Result<()> {
    let fixture = Fixture::new()?;
    let home = fixture.home("first", &login("user-1", "same@example.test"))?;
    let first = auth::require(&home)?;
    let mut changed_labels = first.clone();
    changed_labels.email = Some("new@example.test".into());
    changed_labels.plan = Some("pro".into());
    assert!(first.same_owner(&changed_labels));
    let mut other = first.clone();
    other.user_id = Some("user-2".into());
    assert!(!first.same_owner(&other));
    other = first.clone();
    other.account_id = "workspace-2".into();
    assert!(!first.same_owner(&other));
    let legacy: auth::Identity = serde_json::from_value(
        json!({"accountId": "workspace-1", "email": "same@example.test", "plan": "team"}),
    )?;
    assert!(legacy.user_id.is_none());
    assert!(legacy.same_owner(&first));
    for email in [None, Some("".to_string()), Some(" ".to_string())] {
        let mut unknown = legacy.clone();
        unknown.email = email;
        assert!(!unknown.same_owner(&unknown));
        assert!(!unknown.same_owner(&first));
    }
    let home = fixture.home("no-email", &login("user-1", ""))?;
    let no_email = auth::require(&home)?;
    assert!(first.same_owner(&no_email));
    assert!(!legacy.same_owner(&no_email));
    let home = fixture.home("other-no-email", &login("user-2", ""))?;
    assert!(!no_email.same_owner(&auth::require(&home)?));
    Ok(())
}

#[test]
fn verification_and_usage_reject_another_member_before_any_request() -> Result<()> {
    let fixture = Fixture::new()?;
    let home = fixture.home("first", &login("user-1", "same@example.test"))?;
    let expected = Some(auth::require(&home)?);
    fsutil::atomic_json(
        &home.join("auth.json"),
        &login("user-2", "same@example.test"),
    )?;
    assert!(auth::verify(&home, &expected).is_err());
    let client = crate::usage_client::client()?;
    let error = crate::usage_client::fetch(&client, &home, &expected)
        .err()
        .unwrap();
    assert!(error.to_string().contains("another account"));
    Ok(())
}

#[test]
fn verified_credentials_keep_document_and_owner_from_the_same_read() -> Result<()> {
    let fixture = Fixture::new()?;
    let original = login("user-1", "first@example.test");
    let home = fixture.home("first", &original)?;
    let expected = Some(auth::require(&home)?);
    assert!(auth::verified_credentials(&home, &None).is_ok());
    let (document, identity) = auth::verified_credentials(&home, &expected)?;
    fsutil::atomic_json(
        &home.join("auth.json"),
        &login("user-2", "second@example.test"),
    )?;
    assert_eq!(document, original);
    assert_eq!(document["tokens"]["account_id"], identity.account_id);
    assert_eq!(identity.user_id.as_deref(), Some("user-1"));
    assert!(auth::verified_credentials(&home, &expected).is_err());
    Ok(())
}

#[test]
fn saving_members_separately_and_refreshing_owner_preserves_metadata() -> Result<()> {
    let fixture = Fixture::new()?;
    let first_doc = login("user-1", "same@example.test");
    let first_home = fixture.home("first", &first_doc)?;
    let second_home = fixture.home("second", &login("user-2", "same@example.test"))?;
    let mut store = Store::open(&fixture.cli)?;
    let first = saved(&mut store, &first_home, Some("first".into()), Some(3))?;
    store.data.default = Some(first.number);
    store.data.original_account = Some(first.number);
    let mapping = fixture.directory.path().join("project");
    store
        .data
        .directory_mappings
        .insert(mapping.clone(), first.number);
    let second = saved(&mut store, &second_home, None, None)?;
    assert_ne!(first.number, second.number);
    assert_eq!(auth::credentials(&first.home)?.0, first_doc);
    let refreshed = credentials(
        json!({"email": "changed@example.test", "https://api.openai.com/auth": {"chatgpt_user_id": "user-1", "chatgpt_plan_type": "pro"}}),
    );
    fsutil::atomic_json(&first_home.join("auth.json"), &refreshed)?;
    let updated = saved(&mut store, &first_home, None, None)?;
    assert_eq!(updated.number, first.number);
    assert_eq!(updated.home, first.home);
    assert_eq!(updated.alias, first.alias);
    assert_eq!(store.data.default, Some(first.number));
    assert_eq!(store.data.original_account, Some(first.number));
    assert_eq!(store.data.directory_mappings[&mapping], first.number);
    assert_eq!(auth::credentials(&updated.home)?.0, refreshed);
    assert_eq!(store.data.accounts.len(), 2);
    store.ensure_unique_identity(second.identity.as_ref().unwrap(), second.number)?;
    assert!(
        store
            .ensure_unique_identity(updated.identity.as_ref().unwrap(), second.number)
            .is_err()
    );
    let main = store.data.main_home.clone();
    fsutil::atomic_json(
        &main.join("auth.json"),
        &login("user-2", "same@example.test"),
    )?;
    assert_eq!(store.live_account()?.unwrap().number, second.number);
    Ok(())
}

#[test]
fn legacy_resave_enriches_only_an_unambiguous_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    let source = fixture.home("source", &login("user-1", "first@example.test"))?;
    let mut store = Store::open(&fixture.cli)?;
    let first = saved(&mut store, &source, Some("legacy".into()), Some(7))?;
    store.data.accounts[0].identity.as_mut().unwrap().user_id = None;
    let updated = saved(&mut store, &source, None, None)?;
    assert_eq!(updated.number, first.number);
    assert_eq!(updated.alias, first.alias);
    assert_eq!(
        updated.identity.as_ref().unwrap().user_id.as_deref(),
        Some("user-1")
    );
    store.data.accounts[0].identity.as_mut().unwrap().user_id = None;
    let mut duplicate = store.data.accounts[0].clone();
    duplicate.number = 8;
    duplicate.alias = None;
    store.data.accounts.push(duplicate);
    let before = fsutil::optional_bytes(&first.home.join("auth.json"))?;
    assert!(saved(&mut store, &source, None, None).is_err());
    assert_eq!(
        fsutil::optional_bytes(&first.home.join("auth.json"))?,
        before
    );
    fsutil::atomic_json(
        &store.data.main_home.join("auth.json"),
        &login("user-1", "first@example.test"),
    )?;
    assert!(store.live_account().is_err());
    Ok(())
}

#[test]
fn original_home_migration_does_not_assign_a_login_without_owner_evidence() -> Result<()> {
    for evidence in [None, Some(""), Some("first@example.test")] {
        let fixture = Fixture::new()?;
        let document = login("user-1", "first@example.test");
        let mut store = Store::open(&fixture.cli)?;
        fsutil::atomic_json(&store.data.main_home.join("auth.json"), &document)?;
        let legacy = evidence.map(|email| auth::Identity {
            account_id: "workspace-1".into(),
            user_id: None,
            email: Some(email.into()),
            plan: Some("team".into()),
            legacy_hint_unusable: false,
        });
        store.data.accounts.push(Account {
            number: 4,
            alias: Some("original".into()),
            home: store.data.main_home.clone(),
            managed: false,
            share_history: false,
            identity: legacy,
            enabled: false,
        });
        store.data.default = Some(4);
        let mut transaction = Transaction::new();
        migrate_original(&mut store, &mut transaction)?;
        let account = &store.data.accounts[0];
        assert_ne!(account.home, store.data.main_home);
        assert_eq!(account.alias.as_deref(), Some("original"));
        assert!(!account.enabled);
        assert_eq!(store.data.default, Some(4));
        assert_eq!(store.data.original_account, Some(4));
        if evidence == Some("first@example.test") {
            assert_eq!(auth::credentials(&account.home)?.0, document);
            assert_eq!(
                account.identity.as_ref().unwrap().user_id.as_deref(),
                Some("user-1")
            );
        } else {
            assert!(fsutil::optional_bytes(&account.home.join("auth.json"))?.is_none());
        }
        transaction.finish(Ok(()))?;
    }
    Ok(())
}

#[test]
fn staged_login_rejects_another_member_and_refreshes_the_registered_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    let main = fixture.home("main", &credentials(json!({})))?;
    let main_before = fsutil::optional_bytes(&main.join("auth.json"))?;
    let home = fixture.home("source", &login("user-1", "same@example.test"))?;
    let mut store = Store::open(&fixture.cli)?;
    let account = saved(&mut store, &home, Some("first".into()), None)?;
    store.save()?;
    let path = account.home.join("auth.json");
    let previous = fsutil::optional_bytes(&path)?;
    let destination = LoginDestination {
        account: account.clone(),
        effective_home: account.home.clone(),
        previous: vec![(path.clone(), previous.clone())],
    };
    drop(store);
    let wrong_doc = login("user-2", "same@example.test");
    let wrong_home = fixture.home("wrong", &wrong_doc)?;
    assert!(
        commit_login(
            &fixture.cli,
            &destination,
            &wrong_doc,
            auth::require(&wrong_home)?
        )
        .is_err()
    );
    assert_eq!(fsutil::optional_bytes(&path)?, previous);
    let refreshed = login("user-1", "changed@example.test");
    let refreshed_home = fixture.home("refreshed", &refreshed)?;
    commit_login(
        &fixture.cli,
        &destination,
        &refreshed,
        auth::require(&refreshed_home)?,
    )?;
    assert_eq!(auth::credentials(&account.home)?.0, refreshed);
    // Labels changing in the registry while login is pending still invalidate selection.
    let error = commit_login(
        &fixture.cli,
        &destination,
        &refreshed,
        auth::require(&refreshed_home)?,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("selection changed"));
    assert_eq!(
        fsutil::optional_bytes(&main.join("auth.json"))?,
        main_before
    );
    Ok(())
}

#[test]
fn imports_keep_members_separate_and_exports_reject_a_replaced_owner() -> Result<()> {
    let fixture = Fixture::new()?;
    let file = fixture.directory.path().join("backup.json");
    fsutil::create_json(
        &file,
        &json!({"format": "codex-swap-account-backup", "schemaVersion": 1, "default": 1, "accounts": [
            {"number": 1, "alias": "first", "enabled": true, "shareHistory": false, "auth": login("user-1", "same@example.test")},
            {"number": 2, "alias": "second", "enabled": true, "shareHistory": false, "auth": login("user-2", "same@example.test")}
        ]}),
    )?;
    crate::backup::import(&fixture.cli, &file, false, &Output { json: false })?;
    let store = Store::open(&fixture.cli)?;
    assert_eq!(store.data.accounts.len(), 2);
    let first = store.resolve("1")?;
    assert_eq!(
        first.identity.as_ref().unwrap().user_id.as_deref(),
        Some("user-1")
    );
    assert_eq!(
        store
            .resolve("2")?
            .identity
            .as_ref()
            .unwrap()
            .user_id
            .as_deref(),
        Some("user-2")
    );
    fsutil::atomic_json(
        &first.home.join("auth.json"),
        &login("user-2", "same@example.test"),
    )?;
    drop(store);
    let export = fixture.directory.path().join("wrong-export.json");
    assert!(
        crate::backup::export(&fixture.cli, &export, Some("1"), &Output { json: false }).is_err()
    );
    assert!(!export.exists());
    Ok(())
}

#[test]
fn import_rejects_duplicate_owners_and_unknown_owners_without_saving() -> Result<()> {
    for second in [
        login("user-1", "changed@example.test"),
        credentials(json!({})),
    ] {
        let fixture = Fixture::new()?;
        let file = fixture.directory.path().join("backup.json");
        fsutil::create_json(
            &file,
            &json!({"format": "codex-swap-account-backup", "schemaVersion": 1, "default": 1, "accounts": [
                {"number": 1, "alias": "first", "enabled": true, "shareHistory": false, "auth": login("user-1", "first@example.test")},
                {"number": 2, "alias": "second", "enabled": true, "shareHistory": false, "auth": second}
            ]}),
        )?;
        assert!(
            crate::backup::import(&fixture.cli, &file, false, &Output { json: false }).is_err()
        );
        let store = Store::open(&fixture.cli)?;
        assert!(store.data.accounts.is_empty());
        assert!(!store.root.join("accounts.json").exists());
        assert_eq!(std::fs::read_dir(store.root.join("accounts"))?.count(), 0);
    }
    Ok(())
}

#[test]
fn unknown_saved_owner_cannot_export_or_request_usage_from_a_mutable_home() -> Result<()> {
    let fixture = Fixture::new()?;
    let document = login("arbitrary-user", "current@example.test");
    let mut store = Store::open(&fixture.cli)?;
    fsutil::atomic_json(&store.data.main_home.join("auth.json"), &document)?;
    store.data.accounts.push(Account {
        number: 1,
        alias: Some("unknown".into()),
        home: store.data.main_home.clone(),
        managed: false,
        share_history: false,
        identity: None,
        enabled: true,
    });
    store.data.next_number = 2;
    store.data.default = Some(1);
    store.save()?;
    let before = fsutil::optional_bytes(&store.root.join("accounts.json"))?;
    let registry = store.root.clone();
    assert!(
        crate::store::require_registered_identity(1, &store.data.accounts[0].identity).is_err()
    );
    drop(store);
    let export = fixture.directory.path().join("unknown-export.json");
    assert!(
        crate::backup::export(&fixture.cli, &export, Some("1"), &Output { json: false }).is_err()
    );
    let error = crate::usage::show(&fixture.cli, Some("1"), false, &Output { json: false })
        .err()
        .unwrap();
    assert!(error.to_string().contains("usage unavailable"));
    assert!(!export.exists());
    assert_eq!(
        fsutil::optional_bytes(&registry.join("accounts.json"))?,
        before
    );
    assert_eq!(
        auth::credentials(fixture.cli.codex_home.as_ref().unwrap())?.0,
        document
    );
    Ok(())
}
