#![cfg(unix)]

use std::{
    ffi::CString,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn unused_fifo_profile_cannot_block_new_login() {
    let root = tempfile::tempdir().unwrap();
    let main = root.path().join("main");
    fs::create_dir(&main).unwrap();
    fs::write(
        main.join("config.toml"),
        "model_instructions_file = 'instructions.md'\n",
    )
    .unwrap();
    fs::write(main.join("instructions.md"), "synthetic instructions").unwrap();
    let fifo = main.join("unused-fifo");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    symlink(&fifo, main.join("unused.config.toml")).unwrap();
    let binary = root.path().join("fake-codex");
    fs::write(
        &binary,
        "#!/bin/sh\ncp \"$CODEX_HOME/config.toml\" \"$0.config-copy\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_xswap"))
        .arg("--data-dir")
        .arg(root.path().join("data"))
        .arg("--codex-home")
        .arg(&main)
        .arg("--codex-bin")
        .arg(&binary)
        .args(["add", "--login", "--email", "fixture@example.test"])
        .env("HOME", root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                !status.success(),
                "the fake native login is deliberately cancelled"
            );
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("unused FIFO profile blocked login configuration staging");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let copied = fs::read_to_string(binary.with_extension("config-copy")).unwrap();
    let config: toml::Value = toml::from_str(&copied).unwrap();
    assert_eq!(
        std::path::Path::new(config["model_instructions_file"].as_str().unwrap()),
        main.canonicalize().unwrap().join("instructions.md")
    );
    assert_eq!(
        fs::read_to_string(main.join("config.toml")).unwrap(),
        "model_instructions_file = 'instructions.md'\n"
    );
}
