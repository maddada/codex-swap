use super::*;

fn collect(fixture: &Fixture, reference: &str, insensitive: bool, runtime: &str) -> Result<()> {
    let mut assets = SharedAssets {
        home: &fixture.home,
        user_home: fsutil::config_user_home()?,
        visited: HashSet::new(),
        links: Vec::new(),
        runtime_roots: vec![fixture.home.join(runtime)],
        case_insensitive: insensitive,
    };
    assets.collect_asset(
        &fixture.main.join("config.toml"),
        &fixture.home.join("config.toml"),
        "model_instructions_file",
        reference,
        false,
    )
}

#[test]
fn unicode_alias_policy_only_limits_new_links_on_insensitive_filesystems() {
    let fixture = Fixture::new();
    for reference in [
        "ſessions/role.toml",
        "ſeſſions/role.toml",
        "ſtate_5.ſqlite",
        "ſtate_5.ſqlite-wal",
        "ſtate_5.ſqlite-shm",
        "ſtate_5.ſqlite-journal",
        "安全/instructions.md",
    ] {
        assert!(
            collect(&fixture, reference, true, "sessions")
                .unwrap_err()
                .to_string()
                .contains("absolute reference")
        );
        collect(&fixture, reference, false, "sessions").unwrap();
    }
    collect(&fixture, "safe/instructions.md", true, "sessions").unwrap();
    // An ASCII destination can also alias a Unicode custom runtime root.
    assert!(collect(&fixture, "safe/instructions.md", true, "ſafe/log").is_err());
    collect(&fixture, "safe/instructions.md", false, "ſafe/log").unwrap();
    let absolute = fixture.main.join("安全/instructions.md");
    collect(&fixture, absolute.to_str().unwrap(), true, "ſafe/log").unwrap();
}

#[cfg(unix)]
#[test]
fn native_unicode_runtime_aliases_cannot_create_asset_shares() {
    let fixture = Fixture::new();
    let probe = tempfile::tempdir_in(&fixture.home).unwrap();
    fs::create_dir(probe.path().join("sessions")).unwrap();
    fs::write(
        probe.path().join("state_5.sqlite"),
        "private runtime sentinel",
    )
    .unwrap();
    let role_alias =
        probe.path().join("ſessions").exists() && probe.path().join("ſeſſions").exists();
    let sqlite_alias = probe.path().join("ſtate_5.ſqlite").exists();
    for suffix in ["-wal", "-shm", "-journal"] {
        fs::write(
            probe.path().join(format!("state_5.sqlite{suffix}")),
            "private runtime sentinel",
        )
        .unwrap();
    }
    let sidecar_alias = ["-wal", "-shm", "-journal"].iter().all(|suffix| {
        probe
            .path()
            .join(format!("ſtate_5.ſqlite{suffix}"))
            .exists()
    });
    drop(probe);
    if !(role_alias || sqlite_alias) {
        return;
    }
    for (reference, role, aliases) in [
        ("ſessions/role.toml", true, role_alias),
        ("ſeſſions/role.toml", true, role_alias),
        ("ſtate_5.ſqlite", false, sqlite_alias),
        ("ſtate_5.ſqlite-wal", false, sidecar_alias),
        ("ſtate_5.ſqlite-shm", false, sidecar_alias),
        ("ſtate_5.ſqlite-journal", false, sidecar_alias),
    ] {
        if !aliases {
            continue;
        }
        fixture.write(
            reference,
            if role {
                "developer_instructions = 'synthetic role instructions'\n"
            } else {
                "synthetic instructions"
            },
        );
        let config = if role {
            format!("[agents.reviewer]\nconfig_file = {reference:?}\n")
        } else {
            format!("model_instructions_file = {reference:?}\n")
        };
        fixture.write("config.toml", &config);
        let error = share(&fixture.main, &fixture.home, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-ASCII path components"));
        assert!(fs::symlink_metadata(fixture.home.join(reference)).is_err());
        assert!(fs::symlink_metadata(fixture.home.join("sessions")).is_err());
    }
    fixture.write("safe/instructions.md", "ordinary asset sentinel");
    fixture.write(
        "config.toml",
        "model_instructions_file = 'safe/instructions.md'\n",
    );
    share(&fixture.main, &fixture.home, &[]).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.home.join("safe/instructions.md")).unwrap(),
        "ordinary asset sentinel"
    );
}
