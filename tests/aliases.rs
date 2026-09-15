use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    main: PathBuf,
    homes: [PathBuf; 2],
    child: PathBuf,
}

impl Fixture {
    fn new(alias: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let data = root.join("data");
        let main = root.join("main");
        let homes = [root.join("account-1"), root.join("account-2")];
        for path in [&data, &main, &homes[0], &homes[1]] {
            fs::create_dir(path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        write_json(&main.join("auth.json"), &auth(9));
        let accounts: Vec<_> = homes
            .iter()
            .enumerate()
            .map(|(index, home)| {
                let number = index + 1;
                write_json(&home.join("auth.json"), &auth(number));
                json!({
                    "number": number, "alias": if number == 1 { alias } else { "personal" },
                    "home": home, "managed": false, "shareHistory": false, "enabled": true,
                    "identity": {"accountId": format!("workspace-{number}"),
                        "email": format!("user{number}@example.invalid"), "plan": null}
                })
            })
            .collect();
        write_json(
            &data.join("accounts.json"),
            &json!({
                "schemaVersion": 1, "mainHome": main, "nextNumber": 3, "default": 2,
                "originalAccount": 1, "accounts": accounts,
                "directoryMappings": {}, "preferences": {}
            }),
        );
        let child = root.join(if cfg!(windows) {
            "mock-agent.exe"
        } else {
            "mock-agent"
        });
        Self {
            _directory: directory,
            root,
            data,
            main,
            homes,
            child,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xswap"));
        command
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--codex-home")
            .arg(&self.main)
            .arg("--codex-bin")
            .arg(&self.child)
            .current_dir(&self.root);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn saved_files(&self) -> Vec<Vec<u8>> {
        [
            self.data.join("accounts.json"),
            self.main.join("auth.json"),
            self.homes[0].join("auth.json"),
            self.homes[1].join("auth.json"),
        ]
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect()
    }

    fn compile_child(&self) {
        // A native child keeps these argument assertions independent of shell quoting on Windows.
        let source = self.root.join("mock_agent.rs");
        fs::write(
            &source,
            r#"
fn main() {
    println!("{}", std::env::var("CODEX_HOME").unwrap());
    for argument in std::env::args().skip(1) {
        println!("{argument}");
    }
}
"#,
        )
        .unwrap();
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--crate-name", "mock_agent", "--edition=2024"])
            .arg(source)
            .arg("-o")
            .arg(&self.child)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn auth(number: usize) -> Value {
    let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"email":"user{number}@example.invalid"}}"#));
    json!({"auth_mode": "chatgpt", "tokens": {
        "account_id": format!("workspace-{number}"), "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh", "id_token": format!("e30.{payload}.synthetic")
    }})
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn leading_hyphen_aliases_fail_before_saved_files_change() {
    let fixture = Fixture::new("work-team");
    let before = fixture.saved_files();
    for alias in ["-work", "--work", "-"] {
        let add_alias = format!("--alias={alias}");
        let backup = fixture.root.join("backup.json");
        write_json(
            &backup,
            &json!({
                "format": "codex-swap-account-backup", "schemaVersion": 1, "default": 3,
                "accounts": [
                    {"number": 3, "alias": "imported-team", "enabled": true,
                        "shareHistory": false, "auth": auth(3)},
                    {"number": 4, "alias": alias, "enabled": true,
                        "shareHistory": false, "auth": auth(4)}
                ]
            }),
        );
        for args in [
            vec![
                "add",
                "--login",
                "--email",
                "user3@example.invalid",
                &add_alias,
            ],
            vec!["rename", "1", "--json", "--", alias],
            vec!["import", backup.to_str().unwrap()],
        ] {
            let output = fixture.run(&args);
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("cannot start with a hyphen"));
            assert_eq!(fixture.saved_files(), before);
        }
        assert!(
            fs::read_dir(fixture.data.join("accounts"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[test]
fn snapshot_rejects_aliases_before_migrating_the_original_home() {
    let fixture = Fixture::new("work-team");
    let path = fixture.data.join("accounts.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    registry["accounts"][0]["home"] = json!(fixture.main);
    registry["accounts"][0]["identity"] = json!({
        "accountId": "workspace-9", "email": "user9@example.invalid", "plan": null
    });
    registry["originalAccount"] = Value::Null;
    write_json(&path, &registry);
    let before = fixture.saved_files();

    for alias in ["-work", "--work", "-", "work team", "PERSONAL"] {
        let argument = format!("--alias={alias}");
        let output = fixture.run(&["add", &argument]);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(if alias == "PERSONAL" {
            "alias is already in use"
        } else {
            "alias must be"
        }));
        assert_eq!(fixture.saved_files(), before);
        assert!(!fixture.data.join("accounts").exists());
        assert!(
            fs::read_dir(&fixture.main)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "auth.json")
        );
    }
}

#[test]
fn legacy_aliases_remain_readable_and_repairable_by_slot() {
    for alias in ["-work", "--work", "-"] {
        let fixture = Fixture::new(alias);
        let auth_before = fixture.saved_files()[1..].to_vec();
        let listed: Value =
            serde_json::from_str(&success(fixture.run(&["list", "--json"]))).unwrap();
        assert_eq!(listed["accounts"][0]["alias"], alias);
        success(fixture.run(&["rename", "1", "work-team", "--json"]));
        let registry: Value =
            serde_json::from_slice(&fs::read(fixture.data.join("accounts.json")).unwrap()).unwrap();
        assert_eq!(registry["accounts"][0]["alias"], "work-team");
        assert_eq!(fixture.saved_files()[1..], auth_before);
        let before = fixture.saved_files();
        let duplicate = fixture.run(&["rename", "2", "WORK-TEAM"]);
        assert_eq!(duplicate.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&duplicate.stderr).contains("alias is already in use"));
        assert_eq!(fixture.saved_files(), before);
    }
}

#[test]
fn explicit_accounts_and_default_child_arguments_stay_distinct() {
    let fixture = Fixture::new("-work");
    fixture.compile_child();
    let forwarded = [
        "-work",
        "--work",
        "-",
        "--status",
        "--switch-to",
        "--upgrade",
        "prompt with spaces",
        "",
    ];
    let check = |selection: Option<&str>, number: usize| {
        let mut args = vec!["run"];
        args.extend(selection);
        args.push("--");
        args.extend(forwarded);
        let output = success(fixture.run(&args));
        let mut expected = vec![
            fixture.homes[number - 1].to_str().unwrap(),
            "-c",
            "cli_auth_credentials_store=\"file\"",
        ];
        expected.extend(forwarded);
        assert_eq!(output.lines().collect::<Vec<_>>(), expected);
    };
    check(Some("1"), 1);
    for alias in ["work-team", "_work", "work.team"] {
        success(fixture.run(&["rename", "1", alias]));
        check(Some(&alias.to_ascii_uppercase()), 1);
    }
    check(None, 2);
    check(Some("personal"), 2);
}
