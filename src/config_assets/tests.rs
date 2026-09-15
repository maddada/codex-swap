use super::*;

struct Fixture {
    root: tempfile::TempDir,
    main: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let root_path = fsutil::resolve_config_path(Path::new("."), &root_path, &root_path);
        let main = root_path.join("main");
        let home = root_path.join("data/account");
        fsutil::private_dir(&main).unwrap();
        fsutil::private_dir(&home).unwrap();
        Self { root, main, home }
    }

    fn write(&self, name: &str, contents: &str) {
        let path = self.main.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn stage(&self) -> tempfile::TempDir {
        fsutil::private_tempdir(self.root.path(), "staging-").unwrap()
    }
}

fn value(path: &Path, key: &str) -> PathBuf {
    let config = read_config(path).unwrap();
    PathBuf::from(config[key].as_str().unwrap())
}

#[test]
fn login_copies_resolve_files_profiles_and_roles_without_source_changes() {
    let fixture = Fixture::new();
    let original = "# main settings\nmodel = 'synthetic/path/model-label'\nnotify = ['relative-program.sh', 'relative/argument']\nmodel_instructions_file = 'instructions.md'\nmodel_catalog_json = 'models.json'\n[agents.reviewer]\nconfig_file = 'roles/reviewer.toml'\n";
    fixture.write("config.toml", original);
    fixture.write("work.config.toml", "model_instructions_file = 'work.md'\n");
    fixture.write(
        "roles/reviewer.toml",
        "model_instructions_file = 'role.md'\n",
    );
    fixture.write("auth.json", "synthetic credentials sentinel");
    fixture.write(
        "sessions/conversation.jsonl",
        "synthetic conversation sentinel",
    );
    let stage = fixture.stage();
    copy_for_login(&fixture.main, None, stage.path()).unwrap();
    assert_eq!(
        value(&stage.path().join("config.toml"), "model_instructions_file"),
        fixture.main.join("instructions.md")
    );
    assert_eq!(
        value(&stage.path().join("config.toml"), "model_catalog_json"),
        fixture.main.join("models.json")
    );
    assert_eq!(
        value(
            &stage.path().join("work.config.toml"),
            "model_instructions_file"
        ),
        fixture.main.join("work.md")
    );
    let config = read_config(&stage.path().join("config.toml")).unwrap();
    assert_eq!(config["model"].as_str(), Some("synthetic/path/model-label"));
    assert_eq!(config["notify"][0].as_str(), Some("relative-program.sh"));
    assert_eq!(config["notify"][1].as_str(), Some("relative/argument"));
    assert_eq!(
        Path::new(
            config["agents"]["reviewer"]["config_file"]
                .as_str()
                .unwrap()
        ),
        fixture.main.join("roles/reviewer.toml")
    );
    assert!(!stage.path().join("auth.json").exists());
    assert!(!stage.path().join("sessions").exists());
    fs::write(
        stage.path().join("config.toml"),
        "model = 'synthetic change'\n",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.main.join("config.toml")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(fixture.main.join("auth.json")).unwrap(),
        "synthetic credentials sentinel"
    );
    assert_eq!(
        fs::read_to_string(fixture.main.join("sessions/conversation.jsonl")).unwrap(),
        "synthetic conversation sentinel"
    );
}

#[test]
fn adopted_login_keeps_its_own_base_and_absolute_and_home_paths() {
    let fixture = Fixture::new();
    let absolute = fixture.main.join("absolute.json");
    let config = format!(
        "model_instructions_file = 'adopted.md'\nmodel_catalog_json = {}\nexperimental_compact_prompt_file = '~/synthetic-compact.md'\n",
        toml::Value::String(absolute.to_string_lossy().into_owned())
    );
    fs::write(fixture.home.join("config.toml"), config).unwrap();
    let stage = fixture.stage();
    copy_for_login(&fixture.home, None, stage.path()).unwrap();
    let path = stage.path().join("config.toml");
    assert_eq!(
        value(&path, "model_instructions_file"),
        fixture.home.join("adopted.md")
    );
    assert_eq!(value(&path, "model_catalog_json"), absolute);
    assert_eq!(
        value(&path, "experimental_compact_prompt_file"),
        fsutil::config_user_home()
            .unwrap()
            .join("synthetic-compact.md")
    );
}

#[test]
fn login_does_not_require_an_existing_main_home_or_clobber_config() {
    let fixture = Fixture::new();
    let stage = fixture.stage();
    copy_for_login(&fixture.main.join("absent-home"), None, stage.path()).unwrap();
    fixture.write(
        "config.toml",
        "model_instructions_file = 'instructions.md'\n",
    );
    fs::write(stage.path().join("config.toml"), "keep destination").unwrap();
    assert!(copy_for_login(&fixture.main, None, stage.path()).is_err());
    assert_eq!(
        fs::read_to_string(stage.path().join("config.toml")).unwrap(),
        "keep destination"
    );
}

#[test]
fn profile_selection_respects_separator_and_supported_flag_forms() {
    for args in [
        vec!["--profile", "work"],
        vec!["--profile=work"],
        vec!["-p", "work"],
        vec!["-pwork"],
        vec!["-p=work"],
        vec!["features", "list", "--profile", "work"],
        vec!["--profile", "first", "exec", "--profile", "work"],
    ] {
        let args: Vec<_> = args.into_iter().map(OsString::from).collect();
        assert_eq!(selected_profile(&args), Some("work"));
    }
    for args in [vec!["--", "--profile", "work"], vec!["--profile=../escape"]] {
        let args: Vec<_> = args.into_iter().map(OsString::from).collect();
        assert_eq!(selected_profile(&args), None);
    }
}

#[test]
fn absent_role_parent_keeps_directory_link_kind() {
    let fixture = Fixture::new();
    let user_home = fsutil::config_user_home().unwrap();
    let mut assets = SharedAssets {
        home: &fixture.home,
        user_home,
        visited: HashSet::new(),
        links: Vec::new(),
        runtime_roots: Vec::new(),
    };
    assets
        .collect_asset(
            &fixture.main.join("config.toml"),
            &fixture.home.join("config.toml"),
            "agents.reviewer.config_file",
            "absent/role.toml",
            true,
        )
        .unwrap();
    assert_eq!(assets.links.len(), 1);
    assert!(assets.links[0].directory);
    assert_eq!(assets.links[0].source, fixture.main.join("absent"));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::{
        auth,
        cli::Cli,
        launch,
        store::{Account, Store},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use clap::Parser;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn shared_files_and_selected_profile_follow_main_edits() {
        let fixture = Fixture::new();
        fixture.write(
            "config.toml",
            "model_instructions_file = 'instructions.md'\n",
        );
        fixture.write("instructions.md", "main instructions");
        fixture.write("work.config.toml", "model_catalog_json = 'catalog.json'\n");
        fixture.write("catalog.json", "synthetic catalog");
        fixture.write(
            "unused.config.toml",
            "model_instructions_file = 'missing.md'\n",
        );
        fixture.write("broken.config.toml", "invalid [ TOML");
        symlink(
            fixture.main.join("absent-profile"),
            fixture.main.join("dangling.config.toml"),
        )
        .unwrap();
        fs::create_dir(fixture.main.join("directory.config.toml")).unwrap();
        sharing::config(&fixture.main, &fixture.home).unwrap();
        share(&fixture.main, &fixture.home, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.home.join("instructions.md")).unwrap(),
            "main instructions"
        );
        assert!(!fixture.home.join("catalog.json").exists());
        share(&fixture.main, &fixture.home, &["--profile=work".into()]).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.home.join("catalog.json")).unwrap(),
            "synthetic catalog"
        );
        fixture.write("updated.json", "updated catalog");
        fixture.write("work.config.toml", "model_catalog_json = 'updated.json'\n");
        share(&fixture.main, &fixture.home, &["-pwork".into()]).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.home.join("updated.json")).unwrap(),
            "updated catalog"
        );
        fs::write(
            fixture.home.join("config.toml"),
            "model_instructions_file = 'instructions.md'\nmodel = 'shared edit'\n",
        )
        .unwrap();
        assert!(
            fs::read_to_string(fixture.main.join("config.toml"))
                .unwrap()
                .contains("shared edit")
        );
        let stage = fixture.stage();
        copy_for_login(&fixture.home, Some(&fixture.main), stage.path()).unwrap();
        assert_eq!(
            value(&stage.path().join("config.toml"), "model_instructions_file"),
            fixture.main.join("instructions.md")
        );
        assert_eq!(
            fs::read_to_string(stage.path().join("broken.config.toml")).unwrap(),
            "invalid [ TOML"
        );
        assert!(!stage.path().join("dangling.config.toml").exists());
        assert!(!stage.path().join("directory.config.toml").exists());
    }

    #[test]
    fn selected_profile_replaces_shadowed_base_file_and_role_paths() {
        let fixture = Fixture::new();
        fixture.write("config.toml", "model_instructions_file = 'missing-base.md'\n[agents.reviewer]\nconfig_file = 'unsupported-root-role.toml'\n");
        fixture.write("work.config.toml", "model_instructions_file = 'work.md'\n[agents.reviewer]\nconfig_file = 'custom/role.toml'\n");
        fixture.write("work.md", "profile instructions");
        fixture.write(
            "custom/role.toml",
            "developer_instructions = 'profile role'\n",
        );
        sharing::config(&fixture.main, &fixture.home).unwrap();
        share(&fixture.main, &fixture.home, &["--profile=work".into()]).unwrap();
        assert!(!fixture.home.join("missing-base.md").exists());
        assert!(fs::symlink_metadata(fixture.home.join("missing-base.md")).is_err());
        assert_eq!(
            fs::read_to_string(fixture.home.join("work.md")).unwrap(),
            "profile instructions"
        );
        assert!(
            !fs::symlink_metadata(fixture.home.join("custom/role.toml"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn role_directories_preserve_nested_bases_and_cycles() {
        let fixture = Fixture::new();
        fixture.write("config.toml", "model_instructions_file = 'custom/role.md'\n[agents.reviewer]\nconfig_file = 'custom/reviewer.toml'\n");
        fixture.write("custom/reviewer.toml", "model_instructions_file = 'role.md'\nmodel_catalog_json = '../models.json'\n[agents.nested]\nconfig_file = 'nested/role.toml'\n");
        fixture.write("custom/nested/role.toml", "experimental_compact_prompt_file = '../compact.md'\n[agents.cycle]\nconfig_file = '../reviewer.toml'\n");
        fixture.write("custom/role.md", "role instructions");
        fixture.write("custom/compact.md", "compact instructions");
        fixture.write("models.json", "synthetic catalog");
        sharing::config(&fixture.main, &fixture.home).unwrap();
        share(&fixture.main, &fixture.home, &[]).unwrap();
        assert!(
            fs::symlink_metadata(fixture.home.join("custom"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !fs::symlink_metadata(fixture.home.join("custom/reviewer.toml"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(fixture.home.join("custom/role.md")).unwrap(),
            "role instructions"
        );
        assert_eq!(
            fs::read_to_string(fixture.home.join("models.json")).unwrap(),
            "synthetic catalog"
        );
    }

    #[test]
    fn absent_assets_link_to_source_and_unsafe_or_conflicting_paths_are_preserved() {
        let fixture = Fixture::new();
        fixture.write("config.toml", "model_instructions_file = 'missing.md'\n");
        share(&fixture.main, &fixture.home, &[]).unwrap();
        assert_eq!(
            fs::read_link(fixture.home.join("missing.md")).unwrap(),
            fixture.main.join("missing.md")
        );
        assert!(!fixture.home.join("missing.md").exists());
        fixture.write("config.toml", "model_instructions_file = '../outside.md'\n");
        fs::write(
            fixture.main.parent().unwrap().join("outside.md"),
            "source outside",
        )
        .unwrap();
        let error = share(&fixture.main, &fixture.home, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("use an absolute path"));
        assert!(!fixture.home.parent().unwrap().join("outside.md").exists());
        fixture.write(
            "config.toml",
            "[agents.reviewer]\nconfig_file = 'role.toml'\n",
        );
        fixture.write("role.toml", "developer_instructions = 'synthetic role'\n");
        assert!(
            share(&fixture.main, &fixture.home, &[])
                .unwrap_err()
                .to_string()
                .contains("agents/")
        );
        fixture.write(
            "config.toml",
            "model_instructions_file = 'instructions.md'\n",
        );
        fixture.write("instructions.md", "source instructions");
        fs::write(
            fixture.home.join("instructions.md"),
            "keep private destination",
        )
        .unwrap();
        assert!(share(&fixture.main, &fixture.home, &[]).is_err());
        assert_eq!(
            fs::read_to_string(fixture.home.join("instructions.md")).unwrap(),
            "keep private destination"
        );
        fs::remove_file(fixture.home.join("instructions.md")).unwrap();
        symlink(
            fixture.main.join("role.toml"),
            fixture.home.join("instructions.md"),
        )
        .unwrap();
        assert!(share(&fixture.main, &fixture.home, &[]).is_err());
        assert_eq!(
            fs::read_link(fixture.home.join("instructions.md")).unwrap(),
            fixture.main.join("role.toml")
        );
    }

    #[test]
    fn divergent_shared_config_is_not_silently_replaced_for_login() {
        let fixture = Fixture::new();
        fixture.write("config.toml", "model = 'main'\n");
        fs::write(fixture.home.join("config.toml"), "model = 'private'\n").unwrap();
        let stage = fixture.stage();
        assert!(
            copy_for_login(&fixture.home, Some(&fixture.main), stage.path())
                .unwrap_err()
                .to_string()
                .contains("divergent config")
        );
        assert_eq!(
            fs::read_to_string(fixture.home.join("config.toml")).unwrap(),
            "model = 'private'\n"
        );
        fs::remove_file(fixture.main.join("config.toml")).unwrap();
        assert!(copy_for_login(&fixture.home, Some(&fixture.main), stage.path()).is_err());
        fs::remove_file(fixture.home.join("config.toml")).unwrap();
        symlink(
            fixture.main.join("wrong-missing.toml"),
            fixture.home.join("config.toml"),
        )
        .unwrap();
        assert!(copy_for_login(&fixture.home, Some(&fixture.main), stage.path()).is_err());
    }

    #[test]
    fn runtime_paths_do_not_become_new_asset_shares() {
        for directory in ["sessions", "logs", "auth.json", "state_99.sqlite"] {
            let fixture = Fixture::new();
            fixture.write(
                "config.toml",
                &format!("[agents.reviewer]\nconfig_file = '{directory}/role.toml'\n"),
            );
            fixture.write(
                &format!("{directory}/role.toml"),
                "developer_instructions = 'synthetic role'\n",
            );
            assert!(
                share(&fixture.main, &fixture.home, &[])
                    .unwrap_err()
                    .to_string()
                    .contains("account runtime path")
            );
            assert!(fs::symlink_metadata(fixture.home.join(directory)).is_err());
        }
        for setting in ["log_dir", "sqlite_home"] {
            let fixture = Fixture::new();
            fixture.write("config.toml", &format!("{setting} = 'custom-runtime'\n[agents.reviewer]\nconfig_file = 'custom-runtime/role.toml'\n"));
            fixture.write(
                "custom-runtime/role.toml",
                "developer_instructions = 'synthetic role'\n",
            );
            assert!(
                share(&fixture.main, &fixture.home, &[])
                    .unwrap_err()
                    .to_string()
                    .contains("account runtime path")
            );
            assert!(fs::symlink_metadata(fixture.home.join("custom-runtime")).is_err());
        }
        let fixture = Fixture::new();
        fixture.write("config.toml", "model_instructions_file = 'auth.json'\n");
        fixture.write("auth.json", "synthetic source auth sentinel");
        fs::write(fixture.home.join("auth.json"), "private auth sentinel").unwrap();
        assert!(share(&fixture.main, &fixture.home, &[]).is_err());
        assert_eq!(
            fs::read_to_string(fixture.home.join("auth.json")).unwrap(),
            "private auth sentinel"
        );
    }

    #[test]
    fn existing_runtime_share_is_reused_only_for_the_exact_source_target() {
        let fixture = Fixture::new();
        fixture.write(
            "config.toml",
            "[agents.reviewer]\nconfig_file = 'sessions/role.toml'\n",
        );
        fixture.write(
            "sessions/role.toml",
            "developer_instructions = 'synthetic role'\n",
        );
        symlink(fixture.main.join("sessions"), fixture.home.join("sessions")).unwrap();
        share(&fixture.main, &fixture.home, &[]).unwrap();
        assert_eq!(
            fs::read_link(fixture.home.join("sessions")).unwrap(),
            fixture.main.join("sessions")
        );
        let other = Fixture::new();
        other.write(
            "config.toml",
            "[agents.reviewer]\nconfig_file = 'sessions/role.toml'\n",
        );
        other.write(
            "sessions/role.toml",
            "developer_instructions = 'other synthetic role'\n",
        );
        assert!(share(&other.main, &fixture.home, &[]).is_err());
        assert_eq!(
            fs::read_link(fixture.home.join("sessions")).unwrap(),
            fixture.main.join("sessions")
        );
    }

    fn credentials(id: &str, email: &str) -> serde_json::Value {
        let claims = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&serde_json::json!({"email": email})).unwrap());
        serde_json::json!({"auth_mode":"chatgpt","tokens":{"account_id":id,"id_token":format!("synthetic.{claims}.synthetic"),"access_token":"synthetic-not-valid","refresh_token":"synthetic-not-valid"}})
    }

    fn login_fixture(wrong: bool) -> (Fixture, Cli, PathBuf) {
        let fixture = Fixture::new();
        fixture.write(
            "config.toml",
            "model_instructions_file = 'instructions.md'\n",
        );
        fixture.write("instructions.md", "synthetic instructions");
        sharing::config(&fixture.main, &fixture.home).unwrap();
        let binary = fixture.root.path().join("fake-codex");
        let auth = if wrong {
            format!(
                "cat <<'AUTH' > \"$CODEX_HOME/auth.json\"\n{}\nAUTH\nexit 0\n",
                credentials("wrong", "wrong@example.test")
            )
        } else {
            "exit 1\n".to_owned()
        };
        fs::write(&binary, format!("#!/bin/sh\ncp \"$CODEX_HOME/config.toml\" \"$0.config-copy\"\nprintf 'model = \"staging write\"\\n' > \"$CODEX_HOME/config.toml\"\n{auth}")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let cli = Cli::parse_from([
            OsString::from("xswap"),
            "--data-dir".into(),
            fixture.root.path().join("registry").into(),
            "--codex-home".into(),
            fixture.main.clone().into(),
            "--codex-bin".into(),
            binary.clone().into(),
            "login".into(),
            "fixture".into(),
        ]);
        let mut store = Store::open(&cli).unwrap();
        fsutil::atomic_json(
            &fixture.home.join("auth.json"),
            &credentials("registered", "fixture@example.test"),
        )
        .unwrap();
        store.data.accounts.push(Account {
            number: 1,
            alias: Some("fixture".to_owned()),
            home: fixture.home.clone(),
            managed: true,
            share_history: false,
            identity: Some(auth::require(&fixture.home).unwrap()),
            enabled: true,
        });
        store.data.next_number = 2;
        store.save().unwrap();
        (fixture, cli, binary.with_extension("config-copy"))
    }

    #[test]
    fn reauthentication_stages_source_based_config_and_preserves_failed_credentials() {
        for wrong in [false, true] {
            let (fixture, cli, capture) = login_fixture(wrong);
            let before = fs::read(fixture.home.join("auth.json")).unwrap();
            assert!(launch::login(&cli, "fixture", false).is_err());
            assert_eq!(
                value(&capture, "model_instructions_file"),
                fixture.main.join("instructions.md")
            );
            assert_eq!(fs::read(fixture.home.join("auth.json")).unwrap(), before);
            assert_eq!(
                value(&fixture.main.join("config.toml"), "model_instructions_file"),
                Path::new("instructions.md")
            );
        }
    }

    #[test]
    fn new_login_stages_source_based_config_and_rejects_cancelled_or_wrong_login() {
        for wrong in [false, true] {
            let (fixture, cli, capture) = login_fixture(wrong);
            let args = crate::cli::Add {
                alias: Some("new".to_owned()),
                slot: None,
                home: None,
                login: true,
                email: Some("expected@example.test".to_owned()),
                device_auth: false,
                share_history: false,
                output: crate::cli::Output { json: true },
            };
            let before = fs::read(fixture.root.path().join("registry/accounts.json")).unwrap();
            assert!(crate::commands::add(&cli, &args).is_err());
            assert_eq!(
                value(&capture, "model_instructions_file"),
                fixture.main.join("instructions.md")
            );
            assert_eq!(
                fs::read(fixture.root.path().join("registry/accounts.json")).unwrap(),
                before
            );
            assert_eq!(
                value(&fixture.main.join("config.toml"), "model_instructions_file"),
                Path::new("instructions.md")
            );
        }
    }
}
