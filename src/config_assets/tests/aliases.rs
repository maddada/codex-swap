use super::*;
use std::os::unix::fs::symlink;

#[test]
fn log_parent_alias_cannot_redirect_a_runtime_file_to_a_shared_asset() {
    let fixture = Fixture::new();
    fsutil::private_dir(&fixture.home.join("log")).unwrap();
    symlink("log", fixture.home.join("alias")).unwrap();
    fixture.write("alias/codex-login.log", "shared instructions sentinel\n");
    fixture.write(
        "config.toml",
        "model_instructions_file = 'alias/codex-login.log'\n",
    );
    let error = share(&fixture.main, &fixture.home, &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("account runtime path"));
    assert!(fs::symlink_metadata(fixture.home.join("log/codex-login.log")).is_err());
    assert_eq!(
        fs::read_link(fixture.home.join("alias")).unwrap(),
        Path::new("log")
    );
    assert_eq!(
        fs::read_to_string(fixture.main.join("alias/codex-login.log")).unwrap(),
        "shared instructions sentinel\n"
    );
}

#[test]
fn sqlite_parent_aliases_preserve_future_private_database_files() {
    let fixture = Fixture::new();
    symlink(".", fixture.home.join("alias")).unwrap();
    for filename in [
        "state_5.sqlite",
        "state_5.sqlite-wal",
        "state_5.sqlite-shm",
        "state_5.sqlite-journal",
    ] {
        let reference = format!("alias/{filename}");
        fixture.write(&reference, "shared instructions sentinel\n");
        fixture.write(
            "config.toml",
            &format!("model_instructions_file = {reference:?}\n"),
        );
        let error = share(&fixture.main, &fixture.home, &[])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("account runtime path"),
            "{filename}: {error}"
        );
        assert!(fs::symlink_metadata(fixture.home.join(filename)).is_err());
        assert_eq!(
            fs::read_to_string(fixture.main.join(&reference)).unwrap(),
            "shared instructions sentinel\n"
        );
    }
    assert_eq!(
        fs::read_link(fixture.home.join("alias")).unwrap(),
        Path::new(".")
    );
}

#[test]
fn runtime_roots_also_follow_existing_parent_aliases() {
    for setting in ["log_dir", "sqlite_home"] {
        let fixture = Fixture::new();
        fsutil::private_dir(&fixture.home.join("private-runtime")).unwrap();
        symlink("private-runtime", fixture.home.join("runtime-alias")).unwrap();
        fixture.write(
            "private-runtime/instructions.md",
            "shared instructions sentinel\n",
        );
        fixture.write("config.toml", &format!("{setting} = 'runtime-alias'\nmodel_instructions_file = 'private-runtime/instructions.md'\n"));
        let error = share(&fixture.main, &fixture.home, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("account runtime path"), "{setting}: {error}");
        assert!(
            fs::symlink_metadata(fixture.home.join("private-runtime/instructions.md")).is_err()
        );
        assert_eq!(
            fs::read_link(fixture.home.join("runtime-alias")).unwrap(),
            Path::new("private-runtime")
        );
    }
}

#[test]
fn safe_parent_aliases_and_exact_source_role_directories_are_reused() {
    let fixture = Fixture::new();
    fsutil::private_dir(&fixture.home.join("assets")).unwrap();
    symlink("assets", fixture.home.join("alias")).unwrap();
    fixture.write("alias/instructions.md", "shared instructions sentinel\n");
    fixture.write(
        "config.toml",
        "model_instructions_file = 'alias/instructions.md'\n",
    );
    for _ in 0..2 {
        share(&fixture.main, &fixture.home, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(fixture.home.join("assets/instructions.md")).unwrap(),
            "shared instructions sentinel\n"
        );
    }
    assert_eq!(
        fs::read_link(fixture.home.join("alias")).unwrap(),
        Path::new("assets")
    );
    fs::remove_file(fixture.home.join("alias")).unwrap();
    symlink(fixture.main.join("alias"), fixture.home.join("alias")).unwrap();
    fixture.write(
        "alias/role.toml",
        "developer_instructions = 'synthetic role'\nmodel_instructions_file = 'instructions.md'\n",
    );
    fixture.write(
        "config.toml",
        "[agents.reviewer]\nconfig_file = 'alias/role.toml'\n",
    );
    share(&fixture.main, &fixture.home, &[]).unwrap();
    assert_eq!(
        fs::read_link(fixture.home.join("alias")).unwrap(),
        fixture.main.join("alias")
    );
}
