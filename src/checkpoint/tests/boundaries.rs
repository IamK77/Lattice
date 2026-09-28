use super::*;

#[test]
fn directory_replacement_between_scans_is_not_hidden_by_equal_file_maps() {
    let fixture = Fixture::new();
    fixture.write("a", "unchanged");
    fs::create_dir(fixture.root.join("empty")).unwrap();
    let result = fixture
        .archive
        .capture_after(ScopeInput::default(), &Limits::default(), || {
            // Keeping the old directory alive prevents inode reuse and makes
            // this independent of filesystem timestamp resolution.
            fs::rename(fixture.root.join("empty"), fixture.root.join("retained")).unwrap();
            fs::create_dir(fixture.root.join("empty")).unwrap();
        });
    assert!(result.is_err());
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"unchanged");
}

#[cfg(target_os = "macos")]
fn add_access(path: &std::path::Path, rule: &str) {
    let result = std::process::Command::new("/bin/chmod")
        .args(["+a", rule])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[cfg(target_os = "macos")]
fn private_mode_bits_cannot_hide_archive_directory_access_rules() {
    let fixture = Fixture::new();
    add_access(&fixture.storage, "everyone allow read");
    assert_eq!(
        fs::metadata(&fixture.storage).unwrap().mode() & 0o777,
        0o700
    );
    assert!(Archive::open(&fixture.storage, &fixture.root).is_err());
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn private_mode_bits_cannot_hide_object_access_rules() {
    let fixture = Fixture::new();
    let objects = Objects::open(&fixture.storage).unwrap();
    let reference = objects.put(&mut &b"backup"[..], 6).unwrap();
    let path = fixture.storage.join(&reference.sha256);
    add_access(&path, "everyone allow read");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert!(objects.verify(&reference).is_err());
    assert!(objects.put(&mut &b"backup"[..], 6).is_err());
}

#[test]
#[cfg(target_os = "macos")]
fn a_new_archive_refuses_inherited_access_rules_before_writing_backups() {
    let fixture = Fixture::new();
    let parent = fixture.storage.with_file_name("inherited-permissions");
    fs::create_dir(&parent).unwrap();
    add_access(
        &parent,
        "everyone allow read,file_inherit,directory_inherit",
    );
    let destination = parent.join("archive");
    assert!(Archive::open(&destination, &fixture.root).is_err());
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
}

#[test]
fn exclusions_cannot_be_bypassed_by_filesystem_spelling_aliases() {
    let fixture = Fixture::new();
    fixture.write("Runtime/secret", "CASE_EXCLUDED_SECRET");
    let result = fixture.archive.capture(
        ScopeInput {
            exclusions: ["runtime".to_owned()].into(),
            ..ScopeInput::default()
        },
        &Limits::default(),
    );
    if fixture.root.join("runtime").exists() {
        assert!(
            result.is_err(),
            "ambiguous exclusion spelling must be refused"
        );
        for entry in fs::read_dir(&fixture.storage).unwrap() {
            assert_ne!(
                fs::read(entry.unwrap().path()).unwrap(),
                b"CASE_EXCLUDED_SECRET"
            );
        }
    } else {
        let snapshot = fixture.archive.snapshot_record(&result.unwrap()).unwrap();
        assert!(
            snapshot.files.contains_key("Runtime/secret"),
            "distinct case-sensitive paths must not be conflated"
        );
    }
}

#[test]
fn inherited_parent_exclusion_applies_to_the_workspace_root() {
    let fixture = Fixture::new();
    fixture.write("secret", "excluded by the parent");
    fixture.write("tracked", "tracked override");
    let target = fixture
        .archive
        .capture(
            ScopeInput {
                tracked: ["tracked".to_owned()].into(),
                external_rules: vec![IgnoreRule {
                    kind: IgnoreKind::Git,
                    base: fixture.root.parent().unwrap().to_str().unwrap().to_owned(),
                    source: "parent ignore rule".to_owned(),
                    contents: "workspace/\n".to_owned(),
                }],
                ..ScopeInput::default()
            },
            &Limits::default(),
        )
        .unwrap();
    assert_eq!(
        names(&fixture.archive.snapshot_record(&target).unwrap()),
        ["tracked"]
    );
    fixture.write("new", "also excluded by the parent");
    let plan = fixture
        .archive
        .prepare(&target, &Limits::default())
        .unwrap();
    assert!(fixture
        .archive
        .plan_record(&plan)
        .unwrap()
        .changes
        .is_empty());
}

#[test]
fn untrusted_path_depth_is_bounded_before_expanding_ancestors() {
    let deep = format!("{}file", "a/".repeat(256));
    assert!(crate::checkpoint::fs::relative(&deep).is_err());
    let fixture = Fixture::new();
    fixture.write("a", "data");
    let target = fixture.capture();
    let mut value =
        serde_json::to_value(fixture.archive.snapshot_record(&target).unwrap()).unwrap();
    let state = value["files"]["a"].clone();
    value["files"] = json!({deep: state});
    value["directories"] = json!({});
    let malformed = fixture.record(&value);
    assert!(fixture.archive.snapshot_record(&malformed).is_err());
}

#[test]
fn rule_labels_and_total_line_counts_are_bounded() {
    let fixture = Fixture::new();
    for (source, contents) in [
        ("s".repeat(4097), "x\n".repeat(32)),
        ("label".to_owned(), "# comment\n".repeat(32_769)),
    ] {
        let rule = IgnoreRule {
            kind: IgnoreKind::Global,
            base: fixture.root.to_str().unwrap().to_owned(),
            source,
            contents,
        };
        assert!(fixture
            .archive
            .capture(
                ScopeInput {
                    external_rules: vec![rule],
                    ..ScopeInput::default()
                },
                &Limits::default()
            )
            .is_err());
    }
}

#[test]
fn archive_creation_keeps_the_parent_resolved_before_the_scope_check() {
    let fixture = Fixture::new();
    let outside = fixture.storage.with_file_name("outside");
    fs::create_dir(&outside).unwrap();
    let alias = fixture.storage.with_file_name("alias");
    symlink(&outside, &alias).unwrap();
    let archive = Archive::open_after_check(&alias.join("another-archive"), &fixture.root, || {
        fs::remove_file(&alias).unwrap();
        symlink(&fixture.root, &alias).unwrap();
    })
    .unwrap();
    assert!(!fixture.root.join("another-archive").exists());
    assert!(outside.join("another-archive/format").exists());
    archive
        .capture(ScopeInput::default(), &Limits::default())
        .unwrap();
}

#[test]
fn exclusion_validation_shares_one_directory_entry_budget() {
    let fixture = Fixture::new();
    for parent in ["a", "b"] {
        for index in 0..3 {
            let path = format!("{parent}/{index}");
            fixture.write(&path, &path);
        }
    }
    let input = ScopeInput {
        exclusions: ["a/0".to_owned(), "b/0".to_owned()].into(),
        ..ScopeInput::default()
    };
    let limits = Limits {
        entries: 5,
        ..Limits::default()
    };
    assert!(fixture.archive.capture(input, &limits).is_err());
    assert_eq!(
        fs::read_dir(&fixture.storage).unwrap().count(),
        1,
        "exclusion discovery must exhaust one shared budget before file copying starts"
    );
}

#[test]
fn a_fully_excluded_directory_does_not_require_read_access() {
    let fixture = Fixture::new();
    fixture.write("keep.txt", "keep");
    fixture.write("private/secret", "do not read");
    let private = fixture.root.join("private");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o0)).unwrap();
    let (_, root) = Directory::open(&fixture.root).unwrap();
    if root.child("private").is_ok() {
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("directory access cannot be denied under this test identity");
        return;
    }
    let outcome = fixture
        .archive
        .capture(
            ScopeInput {
                exclusions: ["private".to_owned()].into(),
                ..ScopeInput::default()
            },
            &Limits::default(),
        )
        .and_then(|target| {
            let snapshot = fixture.archive.snapshot_record(&target)?;
            let plan = fixture.archive.prepare(&target, &Limits::default())?;
            fixture.archive.validate_plan(&plan, &Limits::default())?;
            Ok(snapshot)
        });
    // Restore access even when the operation failed, before asserting, so
    // the fixture remains removable by the same unprivileged test process.
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(names(&outcome.unwrap()), ["keep.txt"]);
}
