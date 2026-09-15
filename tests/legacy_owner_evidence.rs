use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const EMAIL: &str = "owner@example.test";

fn login(user: &str, email: &str) -> Value {
    let claims = json!({
        "https://api.openai.com/profile": { "email": email },
        "https://api.openai.com/auth": { "chatgpt_user_id": user, "chatgpt_plan_type": "team" }
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    json!({ "auth_mode": "chatgpt", "tokens": {
        "account_id": "workspace-1",
        "access_token": format!("synthetic-access-{user}-{email}"),
        "refresh_token": "synthetic-refresh",
        "id_token": format!("e30.{payload}.dummy")
    }})
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    main: PathBuf,
    saved: PathBuf,
    data: PathBuf,
    registry: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let physical = root.path().canonicalize().unwrap();
        let main = physical.join("main");
        let data = physical.join("data");
        let project = physical.join("project");
        for directory in [&main, &data, &project] {
            private_dir(directory);
        }
        let accounts = data.join("accounts");
        private_dir(&accounts);
        let saved = accounts.join("1-legacy");
        private_dir(&saved);
        let registry = data.join("accounts.json");
        write_json(&saved.join("auth.json"), &login("user-A", EMAIL));
        write_json(&main.join("auth.json"), &login("user-B", EMAIL));
        write_json(
            &registry,
            &json!({
                "schemaVersion": 1, "mainHome": main, "nextNumber": 2, "default": 1,
                "originalAccount": 1, "directoryMappings": {project.to_str().unwrap(): 1},
                "accounts": [{"number": 1, "alias": "owner-a", "home": saved,
                    "managed": true, "shareHistory": false, "enabled": true,
                    "identity": {"accountId": "workspace-1", "email": EMAIL, "plan": "team"}}]
            }),
        );
        Self {
            _root: root,
            main,
            saved,
            data,
            registry,
            project,
        }
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xswap"));
        for variable in [
            "CODEX_HOME",
            "XSWAP_HOME",
            "XSWAP_CODEX_HOME",
            "XSWAP_CODEX_BIN",
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "OPENAI_ACCESS_TOKEN",
        ] {
            command.env_remove(variable);
        }
        command
            .args(["--data-dir"])
            .arg(&self.data)
            .args(["--codex-home"])
            .arg(&self.main)
            .args(arguments);
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command(arguments).output().unwrap()
    }

    fn json(&self, arguments: &[&str]) -> Value {
        let output = self.run(arguments);
        success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn export(&self, name: &str) -> (Output, PathBuf) {
        let file = self.data.join(name);
        let output = self
            .command(&["export"])
            .arg(&file)
            .args(["--account", "1", "--json"])
            .output()
            .unwrap();
        (output, file)
    }

    fn import(&self, document: Value) -> Output {
        let file = self.data.join("import.json");
        write_json(
            &file,
            &json!({"format": "codex-swap-account-backup", "schemaVersion": 1,
            "default": 2, "accounts": [{"number": 2, "alias": "new-owner", "enabled": true,
                "shareHistory": false, "auth": document}]}),
        );
        self.command(&["import"])
            .arg(file)
            .args(["--json"])
            .output()
            .unwrap()
    }

    fn snapshot(&self) -> Vec<Vec<u8>> {
        [
            &self.registry,
            &self.saved.join("auth.json"),
            &self.main.join("auth.json"),
        ]
        .into_iter()
        .map(|path| fs::read(path).unwrap())
        .collect()
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn failure(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn same_email_member_cannot_claim_or_export_a_legacy_saved_owner() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    assert!(fixture.json(&["status", "--json"])["active"].is_null());
    let listed = fixture.json(&["list", "--json"]);
    assert_eq!(listed["accounts"][0]["userId"], "user-A");
    assert_eq!(listed["accounts"][0]["home"], json!(fixture.saved));
    assert_eq!(listed["accounts"][0]["isDefault"], false);
    let (output, file) = fixture.export("owner-a.json");
    success(&output);
    let backup: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(backup["accounts"][0]["auth"], login("user-A", EMAIL));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn changed_email_keeps_a_legacy_owner_active_and_exports_its_current_login() {
    let fixture = Fixture::new();
    let current = login("user-A", "changed@example.test");
    write_json(&fixture.main.join("auth.json"), &current);
    let before = fixture.snapshot();
    let status = fixture.json(&["status", "--json"]);
    assert_eq!(status["active"]["number"], 1);
    assert_eq!(status["active"]["userId"], "user-A");
    assert_eq!(status["active"]["email"], "changed@example.test");
    assert_eq!(status["active"]["home"], json!(fixture.main));
    let (output, file) = fixture.export("current-owner-a.json");
    success(&output);
    let backup: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(backup["accounts"][0]["auth"], current);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn imports_reject_a_changed_email_duplicate_and_accept_a_distinct_same_email_member() {
    let fixture = Fixture::new();
    write_json(
        &fixture.main.join("auth.json"),
        &login("user-A", "changed@example.test"),
    );
    let before = fixture.snapshot();
    failure(
        &fixture.import(login("user-A", "changed@example.test")),
        "already registered",
    );
    assert_eq!(fixture.snapshot(), before);
    success(&fixture.import(login("user-B", EMAIL)));
    let saved: Value = serde_json::from_slice(&fs::read(&fixture.registry).unwrap()).unwrap();
    assert_eq!(saved["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(saved["accounts"][0]["identity"]["userId"], "user-A");
    assert_eq!(saved["accounts"][1]["identity"]["userId"], "user-B");
    assert_eq!(saved["accounts"][0]["number"], 1);
    assert_eq!(saved["accounts"][0]["alias"], "owner-a");
    assert_eq!(saved["accounts"][0]["home"], json!(fixture.saved));
    assert_eq!(saved["accounts"][0]["identity"]["email"], EMAIL);
    assert_eq!(saved["accounts"][0]["identity"]["plan"], "team");
    assert_eq!(saved["default"], 1);
    assert_eq!(saved["originalAccount"], 1);
    assert_eq!(
        saved["directoryMappings"][fixture.project.to_str().unwrap()],
        1
    );
    assert_eq!(
        fs::read(fixture.saved.join("auth.json")).unwrap(),
        before[1]
    );
    assert_eq!(fs::read(fixture.main.join("auth.json")).unwrap(), before[2]);
}

#[test]
fn unusable_transport_hints_isolate_the_owner_without_authorizing_credentials() {
    let fixture = Fixture::new();
    let mut unusable = login("user-A", EMAIL);
    unusable["auth_mode"] = json!("api_key");
    unusable["tokens"]["access_token"] = json!("synthetic-secret-that-must-not-appear");
    write_json(&fixture.saved.join("auth.json"), &unusable);
    let before = fixture.snapshot();
    assert!(fixture.json(&["status", "--json"])["active"].is_null());
    let output = fixture.run(&["list", "--json"]);
    success(&output);
    let listed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["accounts"][0]["userId"], "user-A");
    assert_eq!(listed["accounts"][0]["loginStatus"], "invalid_credentials");
    let (export, file) = fixture.export("bad-mode.json");
    failure(&export, "another authentication mode");
    assert!(!file.exists());
    for output in [&output, &export] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-secret"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-secret"));
    }
    failure(
        &fixture.import(login("user-A", "changed@example.test")),
        "already registered",
    );
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn mismatched_or_unreadable_snapshot_hints_cannot_adopt_or_export_main() {
    for kind in [
        "email",
        "workspace",
        "unknown-email",
        "malformed",
        "truncated",
        "empty-signature",
        "missing",
    ]
    .into_iter()
    .chain(cfg!(unix).then_some("unreadable"))
    {
        let fixture = Fixture::new();
        let mut hint = login("user-A", EMAIL);
        match kind {
            "email" => hint = login("user-A", "other@example.test"),
            "workspace" => hint["tokens"]["account_id"] = json!("workspace-2"),
            "unknown-email" => hint = login("user-A", ""),
            "malformed" => {
                hint["tokens"]["id_token"] = json!("dummy.synthetic-secret-not-valid-base64.dummy")
            }
            "truncated" => {
                let token = hint["tokens"]["id_token"].as_str().unwrap();
                hint["tokens"]["id_token"] = json!(token.rsplit_once('.').unwrap().0);
            }
            "empty-signature" => {
                let token = hint["tokens"]["id_token"].as_str().unwrap();
                hint["tokens"]["id_token"] =
                    json!(format!("{}.", token.rsplit_once('.').unwrap().0));
            }
            "missing" | "unreadable" => (),
            _ => unreachable!(),
        }
        write_json(&fixture.saved.join("auth.json"), &hint);
        if kind == "missing" {
            fs::remove_file(fixture.saved.join("auth.json")).unwrap();
        }
        #[cfg(unix)]
        if kind == "unreadable" {
            fs::set_permissions(
                fixture.saved.join("auth.json"),
                fs::Permissions::from_mode(0o000),
            )
            .unwrap();
        }
        let registry_before = fs::read(&fixture.registry).unwrap();
        let main_before = fs::read(fixture.main.join("auth.json")).unwrap();
        let saved_before = fs::read(fixture.saved.join("auth.json")).ok();
        assert!(
            fixture.json(&["status", "--json"])["active"].is_null(),
            "{kind}"
        );
        let listed = fixture.json(&["list", "--json"]);
        assert!(listed["accounts"][0]["userId"].is_null(), "{kind}");
        assert!(listed["accounts"][0]["accountId"].is_null(), "{kind}");
        assert_eq!(listed["accounts"][0]["loginStatus"], "login_required");
        assert_eq!(listed["accounts"][0]["email"], EMAIL);
        let (output, file) = fixture.export("unresolved.json");
        assert!(!output.status.success(), "{kind}");
        assert!(!file.exists());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-secret"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-secret"));
        assert_eq!(fs::read(&fixture.registry).unwrap(), registry_before);
        assert_eq!(
            fs::read(fixture.main.join("auth.json")).unwrap(),
            main_before
        );
        assert_eq!(fs::read(fixture.saved.join("auth.json")).ok(), saved_before);
        // A new explicit registration cannot assign its user ID to the unresolved slot.
        success(&fixture.import(login("user-B", EMAIL)));
        let registry: Value =
            serde_json::from_slice(&fs::read(&fixture.registry).unwrap()).unwrap();
        let previous: Value = serde_json::from_slice(&registry_before).unwrap();
        assert_eq!(registry["accounts"][0], previous["accounts"][0]);
        assert_eq!(registry["accounts"][1]["identity"]["userId"], "user-B");
    }
}

#[test]
fn workspace_only_registry_owners_are_not_enriched_from_saved_or_main_credentials() {
    for (email, user) in [
        (Value::Null, None),
        (json!(" "), None),
        (Value::Null, Some(json!(" "))),
    ] {
        let fixture = Fixture::new();
        let mut registry: Value =
            serde_json::from_slice(&fs::read(&fixture.registry).unwrap()).unwrap();
        registry["accounts"][0]["identity"]["email"] = email;
        if let Some(user) = user {
            registry["accounts"][0]["identity"]["userId"] = user;
        }
        write_json(&fixture.registry, &registry);
        let before = fixture.snapshot();
        assert!(fixture.json(&["status", "--json"])["active"].is_null());
        let listed = fixture.json(&["list", "--json"]);
        assert!(listed["accounts"][0]["userId"].is_null());
        assert!(listed["accounts"][0]["accountId"].is_null());
        assert_eq!(listed["accounts"][0]["loginStatus"], "login_required");
        let (output, file) = fixture.export("unknown-owner.json");
        assert!(!output.status.success());
        assert!(!file.exists());
        assert_eq!(fixture.snapshot(), before);
        success(&fixture.import(login("user-A", "changed@example.test")));
        let imported: Value =
            serde_json::from_slice(&fs::read(&fixture.registry).unwrap()).unwrap();
        assert_eq!(imported["accounts"][0], registry["accounts"][0]);
        assert_eq!(imported["accounts"][1]["identity"]["userId"], "user-A");
    }
}

#[test]
fn matching_email_only_snapshot_hints_preserve_legacy_token_support() {
    let fixture = Fixture::new();
    write_json(&fixture.saved.join("auth.json"), &login("", EMAIL));
    write_json(&fixture.main.join("auth.json"), &login("user-A", EMAIL));
    let before = fixture.snapshot();
    let status = fixture.json(&["status", "--json"]);
    assert_eq!(status["active"]["number"], 1);
    let (output, file) = fixture.export("legacy-current.json");
    success(&output);
    let backup: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(backup["accounts"][0]["auth"], login("user-A", EMAIL));
    assert_eq!(fixture.snapshot(), before);
}
