use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn private_directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn write_private(path: &Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn link_directory(source: &Path, destination: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, destination).unwrap();
    #[cfg(windows)]
    {
        // Directory junctions exercise reparse points without a symlink privilege.
        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(destination)
            .arg(source)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    main: PathBuf,
    fake: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let data = root.join("data");
        let main = root.join("main");
        private_directory(&data);
        private_directory(&main);
        #[cfg(unix)]
        let fake = {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("fake-login");
            let executable = std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .replace('\'', "'\\''");
            fs::write(
                &path,
                format!("#!/bin/sh\nexec '{executable}' --exact fake_login_child --nocapture\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        #[cfg(windows)]
        let fake = {
            let path = root.join("fake-login.cmd");
            fs::write(
                &path,
                format!(
                    "@\"{}\" --exact fake_login_child --nocapture\r\n",
                    std::env::current_exe().unwrap().display()
                ),
            )
            .unwrap();
            path
        };
        Self {
            _temporary: temporary,
            root,
            data,
            main,
            fake,
        }
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xswap"));
        command
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--codex-home")
            .arg(&self.main)
            .arg("--codex-bin")
            .arg(&self.fake)
            .args(arguments);
        command
    }

    fn purge(&self) -> Output {
        self.command(&["purge", "--yes", "--json"])
            .output()
            .unwrap()
    }

    fn registry(&self, accounts: Value) {
        write_private(
            &self.data.join("accounts.json"),
            &serde_json::to_vec(&json!({
                "schemaVersion": 1, "mainHome": self.main, "nextNumber": 2,
                "default": null, "accounts": accounts,
            }))
            .unwrap(),
        );
    }

    fn start(&self, arguments: &[&str], prefix: &str) -> SignIn {
        let mut command = self.command(arguments);
        command
            .env("XSWAP_FAKE_LOGIN_CONTROL", &self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut sign_in = SignIn {
            process: command.spawn().unwrap(),
            staging: PathBuf::new(),
            stopped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(staging) = fs::read_dir(&self.data).unwrap().find_map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name().to_string_lossy().starts_with(prefix)
                    && fs::read_to_string(entry.path().join("ready"))
                        .is_ok_and(|contents| contents.parse::<u32>().is_ok()))
                .then(|| entry.path())
            }) {
                sign_in.staging = staging;
                return sign_in;
            }
            assert!(
                sign_in.process.try_wait().unwrap().is_none(),
                "fake sign-in exited before writing credentials"
            );
            assert!(Instant::now() < deadline, "fake sign-in did not start");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn release(&self) {
        fs::write(self.root.join("release"), b"").unwrap();
    }
}

struct SignIn {
    process: Child,
    staging: PathBuf,
    stopped: bool,
}

impl SignIn {
    fn crash(&mut self) {
        #[cfg(unix)]
        unsafe {
            // Kill only the process group created for this synthetic sign-in.
            libc::kill(-(self.process.id() as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        self.process.kill().unwrap();
        self.process.wait().unwrap();
        self.stopped = true;
    }

    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                self.stopped = true;
                assert!(status.success(), "{status:?}");
                return;
            }
            assert!(Instant::now() < deadline, "fake sign-in did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        if !self.stopped {
            self.crash();
        }
    }
}

#[test]
fn fake_login_child() {
    let Some(control) = std::env::var_os("XSWAP_FAKE_LOGIN_CONTROL") else {
        return;
    };
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let claims = URL_SAFE_NO_PAD.encode(br#"{"email":"user@example.invalid"}"#);
    write_private(
        &home.join("auth.json"),
        &serde_json::to_vec(&json!({"tokens": {
            "account_id": "synthetic-account", "access_token": "synthetic-access",
            "refresh_token": "synthetic-refresh", "id_token": format!("fake.{claims}.fake"),
        }}))
        .unwrap(),
    );
    fs::write(home.join("ready"), std::process::id().to_string()).unwrap();
    let release = PathBuf::from(control).join("release");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release.exists() {
        assert!(Instant::now() < deadline, "fake sign-in was not released");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn purge_removes_legacy_staging_and_forgotten_homes_but_retains_shared_data_and_locks() {
    let fixture = Fixture::new();
    let adopted = fixture.root.join("adopted");
    private_directory(&adopted);
    write_private(&adopted.join("auth.json"), b"synthetic-adopted");
    write_private(&fixture.main.join("auth.json"), b"synthetic-main");
    let sessions = fixture.main.join("sessions");
    private_directory(&sessions);
    fs::write(sessions.join("synthetic-history"), b"retained").unwrap();
    let forgotten = fixture.data.join("accounts/forgotten");
    private_directory(&fixture.data.join("accounts"));
    private_directory(&forgotten);
    write_private(&forgotten.join("auth.json"), b"synthetic-forgotten");
    link_directory(&sessions, &forgotten.join("sessions"));
    let unrelated = fixture.data.join("unrelated");
    private_directory(&unrelated);
    write_private(&unrelated.join("auth.json"), b"retained");
    private_directory(&fixture.data.join("locks"));
    let persistent_lease = fixture.data.join("locks/persistent.lock");
    write_private(&persistent_lease, b"retained-lock");
    for prefix in ["login-", "new-login-"] {
        let staging = fixture.data.join(format!("{prefix}abandoned"));
        private_directory(&staging);
        write_private(&staging.join("auth.json"), b"synthetic-staging");
        link_directory(&sessions, &staging.join("sessions"));
    }
    fixture.registry(json!([{
        "number": 1, "alias": null, "home": adopted,
        "managed": false, "shareHistory": false, "identity": null,
    }]));
    assert!(
        fixture
            .command(&["config", "path", "--json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let registry_lock = fixture.data.join("registry.lock");
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&registry_lock).unwrap().ino()
    };
    let output = fixture.purge();
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["managedHomesRemoved"], 1);
    assert_eq!(result["loginStagingHomesRemoved"], 2);
    assert!(!fixture.data.join("accounts").exists());
    assert!(!fixture.data.join("accounts.json").exists());
    assert!(!fixture.data.join("login-abandoned").exists());
    assert!(!fixture.data.join("new-login-abandoned").exists());
    assert!(adopted.join("auth.json").exists());
    assert!(fixture.main.join("auth.json").exists());
    assert!(sessions.join("synthetic-history").exists());
    assert!(unrelated.join("auth.json").exists());
    assert!(registry_lock.exists());
    assert_eq!(fs::read(persistent_lease).unwrap(), b"retained-lock");
    assert!(
        fs::read_dir(fixture.data.join("locks"))
            .unwrap()
            .next()
            .is_some()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(registry_lock).unwrap().ino(), inode);
    }
}

#[test]
fn active_new_sign_in_refuses_purge_and_keeps_its_credentials_until_commit() {
    let fixture = Fixture::new();
    let mut sign_in = fixture.start(
        &[
            "add",
            "--login",
            "--email",
            "user@example.invalid",
            "--json",
        ],
        "new-login-",
    );
    let output = fixture.purge();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("busy"));
    assert!(sign_in.staging.join("auth.json").exists());
    assert!(!fixture.data.join("accounts.json").exists());
    fixture.release();
    sign_in.finish();
    assert!(!sign_in.staging.exists());
    assert!(fixture.data.join("accounts.json").exists());
    assert!(fixture.purge().status.success());
}

#[test]
fn force_terminated_new_and_repeat_sign_ins_leave_no_staging_credentials_after_purge() {
    for repeat in [false, true] {
        let fixture = Fixture::new();
        let (arguments, prefix) = if repeat {
            let account = fixture.data.join("accounts/saved");
            private_directory(&fixture.data.join("accounts"));
            private_directory(&account);
            fixture.registry(json!([{
                "number": 1, "alias": null, "home": account,
                "managed": true, "shareHistory": false, "identity": null,
            }]));
            (vec!["login", "1"], "login-")
        } else {
            (
                vec!["add", "--login", "--email", "user@example.invalid"],
                "new-login-",
            )
        };
        let mut sign_in = fixture.start(&arguments, prefix);
        #[cfg(windows)]
        let fake_process = {
            use std::os::windows::io::{FromRawHandle, OwnedHandle};
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
            let pid = fs::read_to_string(sign_in.staging.join("ready"))
                .unwrap()
                .parse()
                .unwrap();
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            assert!(!handle.is_null(), "open the synthetic login child's handle");
            unsafe { OwnedHandle::from_raw_handle(handle) }
        };
        sign_in.crash();
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{
                Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
            };
            assert_eq!(
                unsafe { WaitForSingleObject(fake_process.as_raw_handle(), 5000) },
                WAIT_OBJECT_0,
                "Windows must terminate the synthetic child with its launcher"
            );
        }
        assert!(sign_in.staging.join("auth.json").exists());
        let output = fixture.purge();
        assert!(output.status.success(), "{output:?}");
        assert!(!sign_in.staging.exists());
        assert!(!fixture.data.join("accounts.json").exists());
        assert!(fixture.main.exists());
    }
}

#[cfg(unix)]
#[test]
fn terminated_launcher_leaves_child_staging_leased_until_the_child_exits() {
    use fs2::FileExt;
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let mut sign_in = fixture.start(
        &["add", "--login", "--email", "user@example.invalid"],
        "new-login-",
    );
    sign_in.process.kill().unwrap();
    sign_in.process.wait().unwrap();
    let output = fixture.purge();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("busy"));
    assert!(sign_in.staging.join("auth.json").exists());
    fixture.release();
    let hash = format!(
        "{:x}",
        Sha256::digest(sign_in.staging.as_os_str().as_encoded_bytes())
    );
    let lease = fs::File::open(fixture.data.join("locks").join(format!("{hash}.lock"))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while lease.try_lock_exclusive().is_err() {
        assert!(
            Instant::now() < deadline,
            "fake child retained its staging lease"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(lease);
    sign_in.stopped = true;
    let output = fixture.purge();
    assert!(output.status.success(), "{output:?}");
    assert!(!sign_in.staging.exists());
    assert!(!fixture.data.join("accounts.json").exists());
}

#[test]
fn purge_refuses_link_staging_without_following_or_deleting_the_target() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside");
    private_directory(&outside);
    write_private(&outside.join("auth.json"), b"retained");
    let link = fixture.data.join("new-login-linked");
    link_directory(&outside, &link);
    let output = fixture.purge();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("real, private owned directory"));
    assert!(fs::symlink_metadata(link).is_ok());
    assert!(outside.join("auth.json").exists());
}

#[test]
fn purge_refuses_staging_overlapping_an_adopted_home() {
    let fixture = Fixture::new();
    let adopted = fixture.data.join("login-adopted");
    private_directory(&adopted);
    write_private(&adopted.join("auth.json"), b"retained");
    fixture.registry(json!([{
        "number": 1, "alias": null, "home": adopted,
        "managed": false, "shareHistory": false, "identity": null,
    }]));
    let output = fixture.purge();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("overlaps"));
    assert!(adopted.join("auth.json").exists());
    assert!(fixture.data.join("accounts.json").exists());
}

#[cfg(unix)]
#[test]
fn purge_refuses_staging_that_is_not_private() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let staging = fixture.data.join("login-not-private");
    private_directory(&staging);
    write_private(&staging.join("auth.json"), b"retained");
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.purge();
    assert!(!output.status.success(), "{output:?}");
    assert!(staging.join("auth.json").exists());
}

#[test]
fn staging_deletion_failure_is_visible_and_does_not_claim_success() {
    let fixture = Fixture::new();
    let staging = fixture.data.join("login-cannot-delete");
    private_directory(&staging);
    let auth = staging.join("auth.json");
    write_private(&auth, b"synthetic-staging");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // Root bypasses the directory write permission used for this failure.
        }
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o500)).unwrap();
    }
    #[cfg(windows)]
    let held_auth = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&auth)
            .unwrap()
    };
    let output = fixture.purge();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(windows)]
    drop(held_auth);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("remove login staging"));
    assert!(output.stdout.is_empty());
    assert!(auth.exists());
    assert!(fixture.purge().status.success());
}
