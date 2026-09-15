#![cfg(unix)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn ignored_assets_reach_child_without_bypassing_account_guards() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let main = root.join("main");
    let data = root.join("data");
    let account = data.join("account");
    for path in [&main, &data, &account] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let original = "model_instructions_file = '../outside.md'\n";
    fs::write(main.join("config.toml"), original).unwrap();
    let claim = URL_SAFE_NO_PAD.encode(br#"{"email":"fixture@example.test"}"#);
    let auth = json!({"auth_mode":"chatgpt", "tokens": {
        "account_id":"fixture-account", "id_token":format!("fixture.{claim}.fixture"),
        "access_token":"synthetic-not-valid", "refresh_token":"synthetic-not-valid"
    }})
    .to_string();
    fs::write(account.join("auth.json"), &auth).unwrap();
    fs::set_permissions(account.join("auth.json"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        data.join("accounts.json"),
        json!({
            "schemaVersion":1,"mainHome":main,"nextNumber":2,"default":1,
            "accounts":[{"number":1,"alias":"fixture","home":account,"managed":true,
            "shareHistory":false,"enabled":true,"identity":{"accountId":"fixture-account",
            "email":"fixture@example.test","plan":null}}]
        })
        .to_string(),
    )
    .unwrap();
    fs::set_permissions(
        data.join("accounts.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let fake = root.join("fake-codex");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\0' \"$@\"\nexit 23\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_xswap"))
            .arg("--data-dir")
            .arg(&data)
            .arg("--codex-home")
            .arg(&main)
            .arg("--codex-bin")
            .arg(&fake)
            .args(["run", "fixture", "--"])
            .args(args)
            .env("HOME", &root)
            .current_dir(&root)
            .output()
            .unwrap()
    };
    for args in [
        vec!["exec", "--ignore-user-config", "synthetic prompt"],
        vec!["e", "--ignore-user-config", "synthetic prompt"],
        vec!["x", "--ignore-user-config", "synthetic prompt"],
        vec!["--help"],
        vec!["--version"],
        vec!["exec", "--ignore-user-config", "--", "--help"],
        vec![
            "--image=fake.png",
            "exec",
            "--ignore-user-config",
            "synthetic prompt",
        ],
        vec![
            "-ifake.png",
            "exec",
            "--ignore-user-config",
            "synthetic prompt",
        ],
    ] {
        let result = invoke(&args);
        assert_eq!(
            result.status.code(),
            Some(23),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let forwarded = result
            .stdout
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            &forwarded[..2],
            [
                b"-c".as_slice(),
                b"cli_auth_credentials_store=\"file\"".as_slice()
            ]
        );
        assert_eq!(
            &forwarded[2..],
            args.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>()
        );
    }
    for args in [
        vec!["exec", "synthetic prompt"],
        vec!["exec", "--", "--help"],
        vec!["exec", "--", "--ignore-user-config"],
        vec!["--image=fake.png", "exec", "synthetic prompt"],
        vec!["-ifake.png", "exec", "synthetic prompt"],
        vec!["--image=fake.png", "exec", "--", "--ignore-user-config"],
        vec!["-ifake.png", "exec", "--", "--help"],
    ] {
        let result = invoke(&args);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("cannot preserve model_instructions_file")
        );
    }
    let result = invoke(&[
        "exec",
        "--ignore-user-config",
        "-c",
        "cli_auth_credentials_store='keyring'",
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    fs::write(
        account.join("auth.json"),
        auth.replace("fixture-account", "wrong-account"),
    )
    .unwrap();
    let result = invoke(&["exec", "--ignore-user-config"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(main.join("config.toml")).unwrap(),
        original
    );
    assert!(!data.join("outside.md").exists());
    assert!(fs::symlink_metadata(data.join("outside.md")).is_err());
}
