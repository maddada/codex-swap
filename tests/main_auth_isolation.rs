use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _root: tempfile::TempDir,
    main: PathBuf,
    saved: PathBuf,
    data: PathBuf,
    fake: PathBuf,
    marker: PathBuf,
    next_auth: PathBuf,
}

fn credentials(account: &str, access: &str) -> Value {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "email": "synthetic@example.test",
            "https://api.openai.com/auth": {"chatgpt_user_id": "synthetic-user"}
        }))
        .unwrap(),
    );
    json!({"auth_mode": "chatgpt", "tokens": {
        "account_id": account, "id_token": format!("synthetic.{payload}.sig"),
        "access_token": access, "refresh_token": "synthetic-refresh"
    }})
}

fn invalid_main_kinds() -> impl Iterator<Item = &'static str> {
    [
        "malformed",
        "api-key",
        "incomplete",
        "directory",
        "hybrid-missing-mode",
        "hybrid-null-mode",
    ]
    .into_iter()
    .chain(cfg!(unix).then_some("unreadable"))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let fixture = Self {
            main: root_path.join("main"),
            saved: root_path.join("saved"),
            data: root_path.join("data"),
            fake: root_path.join("synthetic-child"),
            marker: root_path.join("child-home"),
            next_auth: root_path.join("next-auth.json"),
            _root: root,
        };
        for directory in [&fixture.main, &fixture.saved, &fixture.data] {
            fs::create_dir(directory).unwrap();
            #[cfg(unix)]
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        write(
            &fixture.saved.join("auth.json"),
            serde_json::to_vec(&credentials("synthetic-account", "synthetic-old")).unwrap(),
        );
        write(
            &fixture.next_auth,
            serde_json::to_vec(&credentials("synthetic-account", "synthetic-new")).unwrap(),
        );
        write(
            &fixture.data.join("accounts.json"),
            serde_json::to_vec(&json!({"schemaVersion": 1, "mainHome": fixture.main,
                "nextNumber": 2, "default": 1, "directoryMappings": {}, "accounts": [{
                    "number": 1, "alias": "synthetic", "home": fixture.saved,
                    "managed": false, "shareHistory": false, "enabled": true,
                    "identity": {"accountId": "synthetic-account", "userId": "synthetic-user", "email": "synthetic@example.test", "plan": null}
                }]})).unwrap(),
        );
        #[cfg(unix)]
        {
            write(&fixture.fake, b"#!/bin/sh\nset -eu\nprintf '%s' \"$CODEX_HOME\" > \"$SYNTHETIC_MARKER\"\nif [ \"${SYNTHETIC_HOLD:-}\" = 1 ]; then sleep 5; exit 0; fi\ncase \" $* \" in\n  *' login '*)\n    cp \"$SYNTHETIC_NEXT_AUTH\" \"$CODEX_HOME/auth.json\"\n    if [ -n \"${SYNTHETIC_CHANGED_PATH:-}\" ]; then\n      cp \"$SYNTHETIC_NEXT_AUTH\" \"$SYNTHETIC_CHANGED_PATH\"\n    fi\n    if [ -n \"${SYNTHETIC_CHANGED_HOME:-}\" ]; then\n      rm \"$SYNTHETIC_CHANGED_HOME\"\n      ln -s \"$SYNTHETIC_HOME_TARGET\" \"$SYNTHETIC_CHANGED_HOME\"\n    fi\n    ;;\nesac\n");
            fs::set_permissions(&fixture.fake, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fixture
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
            .args(["--codex-bin"])
            .arg(&self.fake)
            .env("SYNTHETIC_MARKER", &self.marker)
            .env("SYNTHETIC_NEXT_AUTH", &self.next_auth)
            .args(arguments);
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command(arguments).output().unwrap()
    }

    fn main_source(&self, kind: &str) {
        let path = self.main.join("auth.json");
        match kind {
            "malformed" => write(&path, b"{\"synthetic-secret-that-must-not-appear\":"),
            "api-key" => write(&path, br#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-secret-that-must-not-appear"}"#),
            "incomplete" => write(&path, br#"{"tokens":{"account_id":"synthetic","access_token":"synthetic-secret-that-must-not-appear"}}"#),
            "hybrid-missing-mode" | "hybrid-null-mode" => {
                let mut document = credentials("synthetic-account", "synthetic-main");
                document.as_object_mut().unwrap().remove("auth_mode");
                if kind == "hybrid-null-mode" {
                    document["auth_mode"] = Value::Null;
                }
                document["OPENAI_API_KEY"] = json!("synthetic-secret-that-must-not-appear");
                write(&path, serde_json::to_vec(&document).unwrap());
            }
            "directory" => fs::create_dir(path).unwrap(),
            #[cfg(unix)]
            "unreadable" => {
                write(&path, b"synthetic-secret-that-must-not-appear");
                fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
            }
            "missing" => (),
            "matching" => write(&path, fs::read(&self.next_auth).unwrap()),
            "unmatched" => write(&path, serde_json::to_vec(&credentials("other-account", "synthetic-other")).unwrap()),
            _ => panic!("unknown synthetic main source"),
        }
    }

    fn listed(&self) -> (Value, Output) {
        let result = self.run(&["list", "--json"]);
        assert_success(&result);
        (serde_json::from_slice(&result.stdout).unwrap(), result)
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output, message: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn invalid_main_preserves_independent_list_and_export() {
    for kind in invalid_main_kinds() {
        let fixture = Fixture::new();
        fixture.main_source(kind);
        let registry_before = fs::read(fixture.data.join("accounts.json")).unwrap();
        let saved_before = fs::read(fixture.saved.join("auth.json")).unwrap();
        let main_before = fs::read(fixture.main.join("auth.json")).ok();
        let (listed, output) = fixture.listed();
        let account = &listed["accounts"][0];
        assert_eq!(account["loginStatus"], "present");
        assert_eq!(account["home"], json!(fixture.saved));
        assert_eq!(account["savedHome"], json!(fixture.saved));
        assert_eq!(account["isDefault"], false);
        assert_eq!(account["accountId"], "synthetic-account");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert_eq!(diagnostic.matches("main Codex login").count(), 1);
        assert!(!diagnostic.contains("synthetic-secret-that-must-not-appear"));
        let backup = fixture.data.join("backup.json");
        let result = fixture
            .command(&["export"])
            .arg(&backup)
            .args(["--account", "1", "--json"])
            .output()
            .unwrap();
        assert_success(&result);
        let exported: Value = serde_json::from_slice(&fs::read(backup).unwrap()).unwrap();
        assert_eq!(
            exported["accounts"][0]["auth"],
            serde_json::from_slice::<Value>(&saved_before).unwrap()
        );
        assert_eq!(
            fs::read(fixture.data.join("accounts.json")).unwrap(),
            registry_before
        );
        assert_eq!(
            fs::read(fixture.saved.join("auth.json")).unwrap(),
            saved_before
        );
        assert_eq!(fs::read(fixture.main.join("auth.json")).ok(), main_before);
        assert_eq!(fixture.main.join("auth.json").is_dir(), kind == "directory");
        assert_failure(&fixture.run(&["status", "--json"]), "xswap:");
        assert_failure(&fixture.run(&["switch", "1"]), "xswap:");
        assert_eq!(
            fs::read(fixture.data.join("accounts.json")).unwrap(),
            registry_before
        );
        assert_eq!(fs::read(fixture.main.join("auth.json")).ok(), main_before);
    }
}

#[test]
fn missing_or_unmatched_main_keeps_independent_account() {
    for kind in ["missing", "unmatched"] {
        let fixture = Fixture::new();
        fixture.main_source(kind);
        let (listed, output) = fixture.listed();
        assert_eq!(listed["accounts"][0]["home"], json!(fixture.saved));
        assert_eq!(listed["accounts"][0]["loginStatus"], "present");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn missing_home_directories_remain_read_only_and_require_login() {
    let fixture = Fixture::new();
    let registry_before = fs::read(fixture.data.join("accounts.json")).unwrap();
    fs::remove_file(fixture.saved.join("auth.json")).unwrap();
    fs::remove_dir(&fixture.saved).unwrap();
    fs::remove_dir(&fixture.main).unwrap();
    let (listed, _) = fixture.listed();
    assert_eq!(listed["accounts"][0]["home"], json!(fixture.saved));
    assert_eq!(listed["accounts"][0]["loginStatus"], "login_required");
    assert!(!fixture.saved.exists());
    assert!(!fixture.main.exists());
    assert_eq!(
        fs::read(fixture.data.join("accounts.json")).unwrap(),
        registry_before
    );
}

#[test]
fn selected_source_errors_remain_independent() {
    for (document, status, error) in [
        (
            b"{".to_vec(),
            "invalid_credentials",
            "invalid Codex auth.json",
        ),
        (
            serde_json::to_vec(&credentials("wrong-account", "synthetic-wrong")).unwrap(),
            "identity_changed",
            "another account",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.main_source("malformed");
        write(&fixture.saved.join("auth.json"), &document);
        let (listed, _) = fixture.listed();
        assert_eq!(listed["accounts"][0]["loginStatus"], status);
        #[cfg(unix)]
        {
            assert_failure(&fixture.run(&["run", "1", "--", "--version"]), error);
            assert!(!fixture.marker.exists());
        }
        let backup = fixture.data.join("backup.json");
        assert_failure(
            &fixture
                .command(&["export"])
                .arg(&backup)
                .args(["--account", "1"])
                .output()
                .unwrap(),
            if status == "identity_changed" {
                "identity"
            } else {
                error
            },
        );
        assert!(!backup.exists());
        // Invalid selected credentials fail before usage can make an HTTP request.
        let usage = fixture.run(&["usage", "1", "--json"]);
        let report: Value = serde_json::from_slice(&usage.stdout).unwrap();
        assert!(!usage.status.success());
        assert!(
            report["accounts"][0]["error"]
                .as_str()
                .unwrap()
                .contains(error)
        );
        assert_eq!(fs::read(fixture.saved.join("auth.json")).unwrap(), document);
    }
}

#[test]
fn legacy_main_errors_do_not_hide_separate_accounts() {
    let fixture = Fixture::new();
    fixture.main_source("api-key");
    let registry_path = fixture.data.join("accounts.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    registry["accounts"].as_array_mut().unwrap().push(json!({
        "number": 2, "alias": "legacy", "home": fixture.main, "managed": false,
        "shareHistory": true, "identity": {"accountId": "legacy-account"}
    }));
    registry["nextNumber"] = json!(3);
    write(&registry_path, serde_json::to_vec(&registry).unwrap());
    let (listed, _) = fixture.listed();
    assert_eq!(listed["accounts"][0]["loginStatus"], "present");
    assert_eq!(listed["accounts"][1]["loginStatus"], "login_required");
    assert!(listed["accounts"][1]["accountId"].is_null());
    assert!(listed["accounts"][1]["userId"].is_null());
    #[cfg(unix)]
    {
        assert_failure(
            &fixture.run(&["run", "2", "--", "--version"]),
            "another authentication mode",
        );
        assert_failure(&fixture.run(&["login", "2"]), "another authentication mode");
        assert!(!fixture.marker.exists());
    }
    assert_eq!(
        fs::read(&registry_path).unwrap(),
        serde_json::to_vec(&registry).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn invalid_main_allows_explicit_run_and_staged_login() {
    for kind in invalid_main_kinds().chain(["missing"]) {
        let fixture = Fixture::new();
        fixture.main_source(kind);
        let main_before = fs::read(fixture.main.join("auth.json")).ok();
        let registry_before: Value =
            serde_json::from_slice(&fs::read(fixture.data.join("accounts.json")).unwrap()).unwrap();
        assert_success(&fixture.run(&["run", "1", "--", "--version"]));
        assert_eq!(
            fs::read_to_string(&fixture.marker).unwrap(),
            fixture.saved.to_str().unwrap()
        );
        assert_success(&fixture.run(&["login", "1"]));
        // Missing selected credentials can also be repaired without changing main.
        fs::remove_file(fixture.saved.join("auth.json")).unwrap();
        assert_success(&fixture.run(&["login", "1"]));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(fixture.saved.join("auth.json")).unwrap())
                .unwrap(),
            serde_json::from_slice::<Value>(&fs::read(&fixture.next_auth).unwrap()).unwrap()
        );
        let registry_after: Value =
            serde_json::from_slice(&fs::read(fixture.data.join("accounts.json")).unwrap()).unwrap();
        for field in ["mainHome", "nextNumber", "default", "directoryMappings"] {
            assert_eq!(registry_after[field], registry_before[field]);
        }
        for field in [
            "number",
            "alias",
            "home",
            "managed",
            "shareHistory",
            "enabled",
            "identity",
        ] {
            assert_eq!(
                registry_after["accounts"][0][field],
                registry_before["accounts"][0][field]
            );
        }
        assert_eq!(fs::read(fixture.main.join("auth.json")).ok(), main_before);
    }
}

#[cfg(unix)]
#[test]
fn hybrid_main_allows_verified_repair_of_incompatible_saved_credentials() {
    for kind in ["hybrid-missing-mode", "hybrid-null-mode"] {
        let fixture = Fixture::new();
        fixture.main_source(kind);
        let main_before = fs::read(fixture.main.join("auth.json")).unwrap();
        write(&fixture.saved.join("auth.json"), &main_before);
        let registry_path = fixture.data.join("accounts.json");
        let registry_before = fs::read(&registry_path).unwrap();
        let (listed, _) = fixture.listed();
        assert_eq!(listed["accounts"][0]["home"], json!(fixture.saved));
        assert_eq!(listed["accounts"][0]["loginStatus"], "invalid_credentials");

        write(
            &fixture.next_auth,
            serde_json::to_vec(&credentials("wrong-account", "synthetic-wrong")).unwrap(),
        );
        assert_failure(&fixture.run(&["login", "1"]), "different account");
        assert_eq!(
            fs::read(fixture.saved.join("auth.json")).unwrap(),
            main_before
        );
        assert_eq!(fs::read(&registry_path).unwrap(), registry_before);
        assert_eq!(
            fs::read(fixture.main.join("auth.json")).unwrap(),
            main_before
        );

        let repaired = credentials("synthetic-account", "synthetic-repaired");
        write(&fixture.next_auth, serde_json::to_vec(&repaired).unwrap());
        assert_success(&fixture.run(&["login", "1"]));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(fixture.saved.join("auth.json")).unwrap())
                .unwrap(),
            repaired
        );
        let registry_after: Value =
            serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
        let registry_before: Value = serde_json::from_slice(&registry_before).unwrap();
        for (field, before) in registry_before.as_object().unwrap() {
            assert_eq!(&registry_after[field], before);
        }
        assert_eq!(
            fs::read(fixture.main.join("auth.json")).unwrap(),
            main_before
        );
        let (listed, _) = fixture.listed();
        assert_eq!(listed["accounts"][0]["home"], json!(fixture.saved));
        assert_eq!(listed["accounts"][0]["loginStatus"], "present");
    }
}

#[cfg(unix)]
#[test]
fn staged_login_rechecks_destination_and_original_bytes() {
    for changed_main in [true, false] {
        let fixture = Fixture::new();
        fixture.main_source("malformed");
        let saved_before = fs::read(fixture.saved.join("auth.json")).unwrap();
        let registry_before = fs::read(fixture.data.join("accounts.json")).unwrap();
        let changed_path = if changed_main {
            fixture.main.join("auth.json")
        } else {
            fixture.saved.join("auth.json")
        };
        let result = fixture
            .command(&["login", "1"])
            .env("SYNTHETIC_CHANGED_PATH", &changed_path)
            .output()
            .unwrap();
        assert_failure(
            &result,
            if changed_main {
                "selection changed during login"
            } else {
                "credentials changed during login"
            },
        );
        assert_eq!(
            fs::read(&changed_path).unwrap(),
            fs::read(&fixture.next_auth).unwrap()
        );
        if changed_main {
            assert_eq!(
                fs::read(fixture.saved.join("auth.json")).unwrap(),
                saved_before
            );
        }
        assert_eq!(
            fs::read(fixture.data.join("accounts.json")).unwrap(),
            registry_before
        );
    }
}

#[cfg(unix)]
fn lease(fixture: &Fixture, home: &Path, exclusive: bool) -> fs::File {
    use fs2::FileExt;
    use sha2::{Digest, Sha256};
    let locks = fixture.data.join("locks");
    fs::create_dir_all(&locks).unwrap();
    fs::set_permissions(&locks, fs::Permissions::from_mode(0o700)).unwrap();
    let hash = format!("{:x}", Sha256::digest(home.as_os_str().as_encoded_bytes()));
    let path = locks.join(format!("{hash}.lock"));
    write(&path, []);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    if exclusive {
        file.lock_exclusive().unwrap();
    } else {
        FileExt::lock_shared(&file).unwrap();
    }
    file
}

#[cfg(unix)]
#[test]
fn explicit_chatgpt_variants_use_live_credentials_and_preserve_launch_leases() {
    for mode in [json!("chatgpt"), json!({"chatgpt": null})] {
        let fixture = Fixture::new();
        let mut document = credentials("synthetic-account", "synthetic-new");
        document["auth_mode"] = mode;
        document["OPENAI_API_KEY"] = json!("synthetic-stale-api-key");
        document["personal_access_token"] = json!("synthetic-stale-personal-token");
        document["bedrock_api_key"] =
            json!({"api_key": "synthetic-stale-bedrock", "region": "synthetic-region"});
        document["bedrock_access_keys"] = json!({"access_key_id": "synthetic-stale-id", "secret_access_key": "synthetic-stale-secret"});
        let original = serde_json::to_vec(&document).unwrap();
        write(&fixture.main.join("auth.json"), &original);
        write(&fixture.saved.join("auth.json"), b"{");
        let (listed, _) = fixture.listed();
        assert_eq!(listed["accounts"][0]["home"], json!(fixture.main));
        assert_eq!(listed["accounts"][0]["isDefault"], true);
        assert_eq!(listed["accounts"][0]["loginStatus"], "present");
        for home in [&fixture.main, &fixture.saved] {
            let held = lease(&fixture, home, true);
            assert_failure(&fixture.run(&["run", "1", "--", "--version"]), "busy");
            assert!(!fixture.marker.exists());
            drop(held);
        }
        assert_success(&fixture.run(&["run", "1", "--", "--version"]));
        assert_eq!(
            fs::read_to_string(&fixture.marker).unwrap(),
            fixture.main.to_str().unwrap()
        );
        let backup = fixture.data.join("backup.json");
        assert_success(
            &fixture
                .command(&["export"])
                .arg(&backup)
                .args(["--account", "1"])
                .output()
                .unwrap(),
        );
        let exported: Value = serde_json::from_slice(&fs::read(backup).unwrap()).unwrap();
        assert_eq!(exported["accounts"][0]["auth"], document);
        assert_eq!(fs::read(fixture.main.join("auth.json")).unwrap(), original);
    }
}

#[cfg(unix)]
#[test]
fn busy_independent_account_refuses_login_and_export() {
    let fixture = Fixture::new();
    fixture.main_source("malformed");
    let saved_before = fs::read(fixture.saved.join("auth.json")).unwrap();
    let _held = lease(&fixture, &fixture.saved, false);
    assert_failure(&fixture.run(&["login", "1"]), "busy");
    assert!(!fixture.marker.exists());
    let backup = fixture.data.join("backup.json");
    assert_failure(
        &fixture
            .command(&["export"])
            .arg(&backup)
            .args(["--account", "1"])
            .output()
            .unwrap(),
        "busy",
    );
    assert!(!backup.exists());
    assert_eq!(
        fs::read(fixture.saved.join("auth.json")).unwrap(),
        saved_before
    );
}

#[cfg(unix)]
#[test]
fn main_home_alias_keeps_strict_source_process_and_lease_guards() {
    use std::os::unix::fs::symlink;
    for alias_main in [false, true] {
        let fixture = Fixture::new();
        fixture.main_source("api-key");
        let account_alias = fixture.data.join("account-home-alias");
        let main_alias = fixture.data.join("main-home-alias");
        symlink(&fixture.main, &account_alias).unwrap();
        symlink(&fixture.main, &main_alias).unwrap();
        let registry_path = fixture.data.join("accounts.json");
        let mut registry: Value =
            serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
        if alias_main {
            registry["mainHome"] = json!(main_alias);
        }
        registry["accounts"].as_array_mut().unwrap().push(json!({
            "number": 2, "alias": "main-alias", "home": account_alias,
            "managed": false, "shareHistory": false,
            "identity": {"accountId": "alias-account", "email": "alias@example.test"}
        }));
        registry["nextNumber"] = json!(3);
        write(&registry_path, serde_json::to_vec(&registry).unwrap());
        let registry_before = fs::read(&registry_path).unwrap();
        let main_before = fs::read(fixture.main.join("auth.json")).unwrap();
        let (listed, _) = fixture.listed();
        assert_eq!(listed["accounts"][0]["loginStatus"], "present");
        assert_eq!(listed["accounts"][1]["loginStatus"], "invalid_credentials");
        assert_eq!(listed["accounts"][1]["home"], json!(fixture.main));
        assert_failure(&fixture.run(&["login", "2"]), "another authentication mode");
        assert!(!fixture.marker.exists());
        let held = lease(&fixture, &fixture.main, false);
        assert_failure(&fixture.run(&["login", "2"]), "busy");
        drop(held);
        assert_eq!(fs::read(&registry_path).unwrap(), registry_before);
        assert_eq!(
            fs::read(fixture.main.join("auth.json")).unwrap(),
            main_before
        );

        write(
            &fixture.main.join("auth.json"),
            fs::read(&fixture.next_auth).unwrap(),
        );
        let running_marker = fixture.data.join("running-child");
        let mut child = Command::new("/bin/sh")
            .arg(&fixture.fake)
            .env("CODEX_HOME", &fixture.main)
            .env("SYNTHETIC_MARKER", &running_marker)
            .env("SYNTHETIC_HOLD", "1")
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !running_marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let result = fixture.run(&["login", "2"]);
        child.wait().unwrap();
        assert!(running_marker.exists());
        assert!(!result.status.success());
        let diagnostic = String::from_utf8_lossy(&result.stderr);
        assert!(
            ["Codex is still running", "cannot enumerate processes"]
                .iter()
                .any(|message| diagnostic.contains(message)),
            "{diagnostic}"
        );
        assert!(!fixture.marker.exists());
        assert_eq!(fs::read(&registry_path).unwrap(), registry_before);
        assert_eq!(
            fs::read(fixture.main.join("auth.json")).unwrap(),
            fs::read(&fixture.next_auth).unwrap()
        );
    }
}

#[cfg(unix)]
#[test]
fn staged_login_rechecks_retargeted_home_alias() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.main_source("api-key");
    let account_alias = fixture.data.join("saved-home-alias");
    symlink(&fixture.saved, &account_alias).unwrap();
    let registry_path = fixture.data.join("accounts.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&registry_path).unwrap()).unwrap();
    registry["accounts"][0]["home"] = json!(account_alias);
    write(&registry_path, serde_json::to_vec(&registry).unwrap());
    let main_before = fs::read(fixture.main.join("auth.json")).unwrap();
    let saved_before = fs::read(fixture.saved.join("auth.json")).unwrap();
    let registry_before = fs::read(&registry_path).unwrap();
    let result = fixture
        .command(&["login", "1"])
        .env("SYNTHETIC_CHANGED_HOME", &account_alias)
        .env("SYNTHETIC_HOME_TARGET", &fixture.main)
        .output()
        .unwrap();
    assert_failure(&result, "selection changed during login");
    assert_eq!(fs::read(&registry_path).unwrap(), registry_before);
    assert_eq!(
        fs::read(fixture.main.join("auth.json")).unwrap(),
        main_before
    );
    assert_eq!(
        fs::read(fixture.saved.join("auth.json")).unwrap(),
        saved_before
    );
}
