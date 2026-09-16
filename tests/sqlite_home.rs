#![cfg(unix)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _root: tempfile::TempDir,
    main: PathBuf,
    account: PathBuf,
    data: PathBuf,
    user: PathBuf,
    fake: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let main = root_path.join("main");
        let account = root_path.join("account");
        let data = root_path.join("data");
        let user = root_path.join("user");
        for path in [&main, &account, &data, &user] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let claim = URL_SAFE_NO_PAD.encode(br#"{"email":"fixture@example.test"}"#);
        let credentials = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "account_id": "fixture-account",
                "id_token": format!("fixture.{claim}.fixture"),
                "access_token": "synthetic-not-valid",
                "refresh_token": "synthetic-not-valid"
            }
        });
        write_private(&account.join("auth.json"), &credentials.to_string());
        write_private(&data.join("accounts.json"), &json!({
            "schemaVersion": 1, "mainHome": main, "nextNumber": 2, "default": 1,
            "accounts": [{
                "number": 1, "alias": "fixture", "home": account,
                "managed": false, "shareHistory": false, "enabled": true,
                "identity": {"accountId": "fixture-account", "email": "fixture@example.test", "plan": null}
            }]
        }).to_string());
        let fake = root_path.join("fake-codex");
        fs::write(&fake, format!(
            "#!/bin/sh\nprintf '%s\\n' \"$CODEX_SQLITE_HOME\" \"$CODEX_HOME\"\nprintf '%s\\n' \"$@\"\nfor arg do\n  if [ \"$arg\" = login ]; then\n    cat > \"$CODEX_HOME/auth.json\" <<'FIXTURE_AUTH'\n{credentials}\nFIXTURE_AUTH\n  fi\ndone\n"
        )).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _root: root,
            main,
            account,
            data,
            user,
            fake,
        }
    }

    fn configure(&self, value: &str) {
        fs::write(
            self.main.join("config.toml"),
            format!("sqlite_home = {}\n", toml::Value::String(value.into())),
        )
        .unwrap();
    }

    fn invoke(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xswap"))
            .args([
                "--data-dir",
                self.data.to_str().unwrap(),
                "--codex-home",
                self.main.to_str().unwrap(),
                "--codex-bin",
                self.fake.to_str().unwrap(),
            ])
            .args(args)
            .env("HOME", &self.user)
            .env("CODEX_SQLITE_HOME", self.user.join("inherited-conflict"))
            .current_dir(&self.user)
            .output()
            .unwrap()
    }
}

fn write_private(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn shared_child_receives_the_main_config_anchor_in_both_override_and_environment() {
    let fixture = Fixture::new();
    let existing = fixture.user.join("existing state");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("state.sqlite"), b"existing fixture database").unwrap();
    let absolute = fixture.user.join("absolute/missing/../数据库 with spaces");
    for (value, expected) in [
        ("~/codex-state".to_owned(), fixture.user.join("codex-state")),
        ("~".to_owned(), fixture.user.clone()),
        (
            "~///missing/../数据库 with spaces".to_owned(),
            fixture.user.join("数据库 with spaces"),
        ),
        (
            "state/./missing/../数据库 with spaces".to_owned(),
            fixture.main.join("state/数据库 with spaces"),
        ),
        (
            "~someone/state".to_owned(),
            fixture.main.join("~someone/state"),
        ),
        (
            absolute.to_str().unwrap().to_owned(),
            fixture.user.join("absolute/数据库 with spaces"),
        ),
        ("~/existing state".to_owned(), existing.clone()),
    ] {
        fixture.configure(&value);
        let existed = expected.exists();
        let output = fixture.invoke(&["run", "fixture", "--share-history"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(Path::new(lines[0]), expected);
        assert_eq!(Path::new(lines[1]), fixture.account);
        let override_value = lines
            .iter()
            .find_map(|line| line.strip_prefix("sqlite_home="))
            .unwrap();
        let config: toml::Value = toml::from_str(&format!("sqlite_home={override_value}")).unwrap();
        assert_eq!(
            config["sqlite_home"].as_str(),
            Some(expected.to_str().unwrap())
        );
        assert_eq!(
            expected.exists(),
            existed,
            "resolution created a SQLite destination"
        );
    }
    assert_eq!(
        fs::read(existing.join("state.sqlite")).unwrap(),
        b"existing fixture database"
    );
    assert!(!fixture.user.join("inherited-conflict").exists());
}

#[test]
fn sharing_rejects_sqlite_overrides_and_login_keeps_its_isolated_anchor() {
    let fixture = Fixture::new();
    fixture.configure("~/shared-state");
    let rejected = fixture.invoke(&[
        "run",
        "fixture",
        "--share-history",
        "--",
        "-c",
        "sqlite_home=\"override\"",
    ]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("xswap owns sqlite_home"));
    let login = fixture.invoke(&["login", "fixture"]);
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    let stderr = String::from_utf8(login.stderr).unwrap();
    let staging = stderr
        .lines()
        .find_map(|line| line.strip_prefix("sqlite_home="))
        .unwrap();
    let config: toml::Value = toml::from_str(&format!("sqlite_home={staging}")).unwrap();
    let staging = Path::new(config["sqlite_home"].as_str().unwrap());
    assert_eq!(staging.parent(), Some(fixture.data.as_path()));
    assert!(
        staging
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("login-")
    );
    assert!(!staging.exists());
    assert!(!fixture.user.join("shared-state").exists());
}
