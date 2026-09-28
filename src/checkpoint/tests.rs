use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Cursor, Write};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::PathBuf;

use serde_json::{json, Value};

use super::fs::Directory;
use super::objects::{transfer, Objects};
use super::*;

mod boundaries;

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    storage: PathBuf,
    archive: Archive,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let storage = temporary.path().join("archive");
        let archive = Archive::open(&storage, &root).unwrap();
        Self {
            _temporary: temporary,
            root,
            storage,
            archive,
        }
    }

    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn capture(&self) -> ObjectRef {
        self.archive
            .capture(ScopeInput::default(), &Limits::default())
            .unwrap()
    }

    fn bytes(&self, state: &FileState) -> Vec<u8> {
        fs::read(self.storage.join(&state.content.sha256)).unwrap()
    }

    fn record(&self, value: &Value) -> ObjectRef {
        Objects::open(&self.storage)
            .unwrap()
            .put_record(value)
            .unwrap()
    }
}

fn names(snapshot: &Snapshot) -> Vec<&str> {
    snapshot.files.keys().map(String::as_str).collect()
}

#[test]
fn addresses_are_digests_not_paths() {
    for invalid in [
        "",
        "../escape",
        "/absolute",
        &"a".repeat(63),
        &"A".repeat(64),
        &"g".repeat(64),
        &"é".repeat(32),
    ] {
        assert!(ObjectRef {
            sha256: invalid.to_owned(),
            bytes: 0
        }
        .validate()
        .is_err());
    }
    ObjectRef {
        sha256: "0123456789abcdef".repeat(4),
        bytes: 0,
    }
    .validate()
    .unwrap();
}

#[test]
fn private_objects_preserve_binary_bytes_and_reuse_content() {
    let fixture = Fixture::new();
    let objects = Objects::open(&fixture.storage).unwrap();
    let bytes: Vec<u8> = (0..=255).cycle().take(140_000).collect();
    let first = objects
        .put(&mut bytes.as_slice(), bytes.len() as u64)
        .unwrap();
    let second = objects
        .put(&mut bytes.as_slice(), bytes.len() as u64)
        .unwrap();
    assert_eq!(first, second);
    let mut restored = Vec::new();
    objects.copy(&first, &mut restored).unwrap();
    assert_eq!(restored, bytes);
    let empty = objects.put(&mut &b""[..], 0).unwrap();
    assert_eq!(empty.bytes, 0);
    objects.verify(&empty).unwrap();
    assert_eq!(fs::read_dir(&fixture.storage).unwrap().count(), 3);
    assert_eq!(
        fs::metadata(&fixture.storage).unwrap().mode() & 0o777,
        0o700
    );
    for entry in fs::read_dir(&fixture.storage).unwrap() {
        assert_eq!(entry.unwrap().metadata().unwrap().mode() & 0o777, 0o600);
    }
}

#[test]
fn same_length_corruption_is_not_accepted_or_repaired() {
    let fixture = Fixture::new();
    let objects = Objects::open(&fixture.storage).unwrap();
    let reference = objects.put(&mut &b"original"[..], 8).unwrap();
    let path = fixture.storage.join(&reference.sha256);
    fs::write(&path, b"tampered").unwrap();
    assert!(objects.verify(&reference).is_err());
    assert!(objects.put(&mut &b"original"[..], 8).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"tampered");
    fs::remove_file(&path).unwrap();
    assert!(objects.verify(&reference).is_err());
    let outside = fixture.root.join("outside");
    fs::write(&outside, b"original").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(objects.verify(&reference).is_err());
    assert!(objects.put(&mut &b"original"[..], 8).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"original");
}

#[test]
fn stream_limits_read_only_one_byte_past_the_boundary() {
    for limit in [0, 1, 2, 3] {
        let mut input = Cursor::new(b"abc".to_vec());
        let mut output = Vec::new();
        let result = transfer(&mut input, &mut output, limit);
        assert_eq!(result.is_ok(), limit == 3);
        assert_eq!(input.position(), (limit + 1).min(3));
        assert!(output.len() as u64 <= limit);
    }
    assert_eq!(
        transfer(&mut &b""[..], &mut io::sink(), 0).unwrap().bytes,
        0
    );
    let fixture = Fixture::new();
    let objects = Objects::open(&fixture.storage).unwrap();
    assert!(objects.put(&mut &b"too much"[..], 2).is_err());
    assert_eq!(
        fs::read_dir(&fixture.storage).unwrap().count(),
        1,
        "only the format marker remains"
    );
}

#[test]
fn archive_never_adopts_unrelated_directories_or_enters_the_workspace() {
    let fixture = Fixture::new();
    assert!(Archive::open(&fixture.root.join("backup"), &fixture.root).is_err());
    assert!(!fixture.root.join("backup").exists());
    let unrelated = fixture.storage.with_file_name("unrelated");
    fs::create_dir(&unrelated).unwrap();
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(unrelated.join("important"), b"keep").unwrap();
    assert!(Archive::open(&unrelated, &fixture.root).is_err());
    assert!(!unrelated.join("format").exists());
    assert_eq!(fs::read(unrelated.join("important")).unwrap(), b"keep");
    fs::write(fixture.storage.join("format"), b"future format").unwrap();
    assert!(Archive::open(&fixture.storage, &fixture.root).is_err());
    assert_eq!(
        fs::read(fixture.storage.join("format")).unwrap(),
        b"future format"
    );
}

#[test]
fn capture_keeps_tracked_and_hidden_files_but_not_protected_or_ignored_data() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.ignored\nbuild/\n");
    fixture.write(".ignore", "extra.skip\n");
    fixture.write(".config", "visible hidden config");
    fixture.write("src/a.rs", "source");
    fixture.write("tracked.ignored", "tracked overrides ignore");
    fixture.write("build/keep.txt", "tracked inside an ignored directory");
    for path in [
        "private.ignored",
        "build/other.txt",
        "extra.skip",
        ".git/HEAD",
        "runtime/live",
    ] {
        fixture.write(path, "NEVER_COPY_THIS_CONTENT");
    }
    let input = ScopeInput {
        tracked: ["tracked.ignored", "build/keep.txt", "runtime/live"]
            .map(str::to_owned)
            .into(),
        exclusions: ["runtime".to_owned()].into(),
        ..ScopeInput::default()
    };
    let reference = fixture.archive.capture(input, &Limits::default()).unwrap();
    let snapshot = fixture.archive.snapshot_record(&reference).unwrap();
    assert_eq!(
        names(&snapshot),
        [
            ".config",
            ".gitignore",
            ".ignore",
            "build/keep.txt",
            "src/a.rs",
            "tracked.ignored"
        ]
    );
    assert_eq!(
        snapshot
            .directories
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["build", "src"]
    );
    for entry in fs::read_dir(&fixture.storage).unwrap() {
        let contents = fs::read(entry.unwrap().path()).unwrap();
        assert!(!contents
            .windows(23)
            .any(|part| part == b"NEVER_COPY_THIS_CONTENT"));
    }
}

#[test]
fn nested_rules_and_parent_exclusions_have_explicit_precedence() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.rs\nblocked/\n!blocked/wanted.rs\n");
    fixture.write("src/.ignore", "!good.rs\n");
    fixture.write("src/good.rs", "include");
    fixture.write("src/bad.rs", "exclude");
    fixture.write("blocked/wanted.rs", "cannot unignore an excluded parent");
    fixture.write("blocked/tracked.rs", "explicit tracked path");
    let reference = fixture
        .archive
        .capture(
            ScopeInput {
                tracked: ["blocked/tracked.rs".to_owned()].into(),
                ..ScopeInput::default()
            },
            &Limits::default(),
        )
        .unwrap();
    let snapshot = fixture.archive.snapshot_record(&reference).unwrap();
    assert!(snapshot.files.contains_key("src/good.rs"));
    assert!(snapshot.files.contains_key("blocked/tracked.rs"));
    assert!(!snapshot.files.contains_key("src/bad.rs"));
    assert!(!snapshot.files.contains_key("blocked/wanted.rs"));
}

#[test]
fn saved_external_rules_are_data_not_live_configuration() {
    let fixture = Fixture::new();
    fixture.write("a.local", "ignored");
    fixture.write("a.keep", "keep");
    let reference = fixture
        .archive
        .capture(
            ScopeInput {
                external_rules: vec![IgnoreRule {
                    kind: IgnoreKind::Global,
                    base: fixture.root.to_str().unwrap().to_owned(),
                    source: "saved external rule".to_owned(),
                    contents: "*.local\n".to_owned(),
                }],
                ..ScopeInput::default()
            },
            &Limits::default(),
        )
        .unwrap();
    fixture.write("a.local", "still outside the saved scope");
    fixture.write("b.local", "also outside the saved scope");
    let plan = fixture
        .archive
        .prepare(&reference, &Limits::default())
        .unwrap();
    assert!(fixture
        .archive
        .plan_record(&plan)
        .unwrap()
        .changes
        .is_empty());
}

#[test]
fn planning_uses_the_saved_scope_and_retains_both_sides_without_restoring() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.log\noutput/\n");
    fixture.write("edit.bin", [0, 255, 1]);
    fixture.write("deleted.rs", "bring back");
    fixture.write("keep.log", "not in the snapshot");
    let target = fixture.capture();
    fixture.write(".gitignore", "");
    fixture.write("edit.bin", [9, 0, 8]);
    fs::remove_file(fixture.root.join("deleted.rs")).unwrap();
    fixture.write("new.rs", "candidate deletion");
    fixture.write("new.log", "must not be mistaken for absent-at-capture");
    fixture.write("output/untracked", "also excluded by the old policy");
    let reference = fixture
        .archive
        .prepare(&target, &Limits::default())
        .unwrap();
    let plan = fixture.archive.plan_record(&reference).unwrap();
    assert_eq!(
        plan.changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        [".gitignore", "deleted.rs", "edit.bin", "new.rs"]
    );
    let original = fixture.archive.snapshot_record(&target).unwrap();
    let safety = fixture.archive.snapshot_record(&plan.before).unwrap();
    assert_eq!(fixture.bytes(&original.files["edit.bin"]), [0, 255, 1]);
    assert_eq!(fixture.bytes(&safety.files["edit.bin"]), [9, 0, 8]);
    assert!(!safety.files.contains_key("new.log"));
    assert!(!safety.files.contains_key("output/untracked"));
    assert_eq!(fs::read(fixture.root.join("edit.bin")).unwrap(), [9, 0, 8]);
    assert_eq!(fs::read(fixture.root.join(".gitignore")).unwrap(), b"");
    assert!(fixture.root.join("new.rs").exists());
    assert!(!fixture.root.join("deleted.rs").exists());
    fixture
        .archive
        .validate_plan(&reference, &Limits::default())
        .unwrap();
    let reopened = Archive::open(&fixture.storage, &fixture.root).unwrap();
    reopened
        .validate_plan(&reference, &Limits::default())
        .unwrap();
}

#[test]
fn changes_after_preview_require_a_new_plan() {
    for mutation in ["content", "new", "delete", "mode"] {
        let fixture = Fixture::new();
        fixture.write("a", "old");
        let target = fixture.capture();
        fixture.write("a", "now");
        let plan = fixture
            .archive
            .prepare(&target, &Limits::default())
            .unwrap();
        match mutation {
            "content" => fixture.write("a", "bad"),
            "new" => fixture.write("new", "new"),
            "delete" => fs::remove_file(fixture.root.join("a")).unwrap(),
            "mode" => {
                fs::set_permissions(fixture.root.join("a"), fs::Permissions::from_mode(0o700))
                    .unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .archive
                .validate_plan(&plan, &Limits::default())
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn changes_outside_the_saved_scope_do_not_invalidate_the_preview() {
    let fixture = Fixture::new();
    fixture.write(".gitignore", "*.log\n");
    fixture.write("a", "old");
    let target = fixture.capture();
    fixture.write("a", "new");
    let plan = fixture
        .archive
        .prepare(&target, &Limits::default())
        .unwrap();
    fixture.write("unrelated.log", "outside scope");
    fixture
        .archive
        .validate_plan(&plan, &Limits::default())
        .unwrap();
}

#[test]
fn capture_detects_changes_between_copy_and_verification_without_a_clock() {
    let fixture = Fixture::new();
    fixture.write("a", "old");
    let result = fixture
        .archive
        .capture_after(ScopeInput::default(), &Limits::default(), || {
            fixture.write("a", "new")
        });
    assert!(result.is_err());
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"new");

    // Even when an ignore file excludes itself, its policy cannot change
    // unnoticed after the archived file set has been collected.
    fixture.write(".gitignore", ".gitignore\n*.log\n");
    let result = fixture
        .archive
        .capture_after(ScopeInput::default(), &Limits::default(), || {
            fixture.write(".gitignore", ".gitignore\n")
        });
    assert!(result.is_err());
}

#[test]
fn limits_refuse_a_complete_snapshot_instead_of_silently_omitting_files() {
    let fixture = Fixture::new();
    fixture.write("a", "1234");
    fixture.write("nested/b", "1234");
    for limits in [
        Limits {
            file_bytes: 3,
            ..Limits::default()
        },
        Limits {
            total_bytes: 7,
            ..Limits::default()
        },
        Limits {
            entries: 2,
            ..Limits::default()
        },
    ] {
        assert!(fixture
            .archive
            .capture(ScopeInput::default(), &limits)
            .is_err());
    }
    let exact = Limits {
        file_bytes: 4,
        total_bytes: 8,
        entries: 3,
    };
    let snapshot = fixture
        .archive
        .capture(ScopeInput::default(), &exact)
        .unwrap();
    assert_eq!(
        fixture
            .archive
            .snapshot_record(&snapshot)
            .unwrap()
            .files
            .len(),
        2
    );
    let (_, root) = Directory::open(&fixture.root).unwrap();
    assert!(root.entries(1).is_err());
    assert_eq!(root.entries(2).unwrap().len(), 2);
}

#[test]
fn missing_or_corrupt_backup_bytes_cannot_be_used_to_prepare_a_restore() {
    for corruption in ["missing", "changed"] {
        let fixture = Fixture::new();
        fixture.write("a", "old");
        let target = fixture.capture();
        let snapshot = fixture.archive.snapshot_record(&target).unwrap();
        fixture.write("a", "new");
        let blob = fixture.storage.join(&snapshot.files["a"].content.sha256);
        match corruption {
            "missing" => fs::remove_file(&blob).unwrap(),
            "changed" => fs::write(&blob, b"bad").unwrap(),
            _ => unreachable!(),
        }
        assert!(
            fixture
                .archive
                .prepare(&target, &Limits::default())
                .is_err(),
            "{corruption}"
        );
        assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"new");
    }
}

#[test]
fn source_links_special_modes_and_conflicting_file_types_are_refused() {
    let fixture = Fixture::new();
    fixture.write("plain", "data");
    symlink("plain", fixture.root.join("link")).unwrap();
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
    fs::remove_file(fixture.root.join("link")).unwrap();
    fs::hard_link(fixture.root.join("plain"), fixture.root.join("alias")).unwrap();
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
    fs::remove_file(fixture.root.join("alias")).unwrap();
    fs::set_permissions(
        fixture.root.join("plain"),
        fs::Permissions::from_mode(0o4700),
    )
    .unwrap();
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
    fs::set_permissions(
        fixture.root.join("plain"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let target = fixture.capture();
    fs::remove_file(fixture.root.join("plain")).unwrap();
    fixture.write("plain/child", "new directory occupant");
    assert!(fixture
        .archive
        .prepare(&target, &Limits::default())
        .is_err());
    assert_eq!(
        fs::read(fixture.root.join("plain/child")).unwrap(),
        b"new directory occupant"
    );
}

#[test]
fn held_directory_operations_do_not_follow_a_replaced_ancestor() {
    let fixture = Fixture::new();
    fixture.write("src/a", "inside");
    let outside = fixture.storage.with_file_name("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("private"), b"outside").unwrap();
    let (_, root) = Directory::open(&fixture.root).unwrap();
    let held = root.child("src").unwrap();
    fs::rename(fixture.root.join("src"), fixture.root.join("retained")).unwrap();
    symlink(&outside, fixture.root.join("src")).unwrap();
    assert!(root.child("src").is_err());
    assert_eq!(held.entries(10).unwrap(), ["a"]);
    let mut temporary = held.temporary().unwrap();
    temporary.file.write_all(b"held directory").unwrap();
    temporary.publish_new(&held, "new").unwrap();
    assert_eq!(
        fs::read(fixture.root.join("retained/new")).unwrap(),
        b"held directory"
    );
    assert!(!outside.join("new").exists());
    assert_eq!(fs::read(outside.join("private")).unwrap(), b"outside");
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
}

#[test]
fn root_replacement_and_unsafe_relative_paths_cannot_be_accepted() {
    let fixture = Fixture::new();
    fixture.write("a", "keep");
    for path in ["../escape", "/absolute", "a//b", "a/./b", "", ".", "a/../b"] {
        assert!(super::fs::relative(path).is_err(), "{path}");
        assert!(fixture
            .archive
            .capture(
                ScopeInput {
                    exclusions: [path.to_owned()].into(),
                    ..ScopeInput::default()
                },
                &Limits::default()
            )
            .is_err());
    }
    let retained = fixture.root.with_file_name("retained");
    fs::rename(&fixture.root, &retained).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    assert!(fixture
        .archive
        .capture(ScopeInput::default(), &Limits::default())
        .is_err());
    assert_eq!(fs::read(retained.join("a")).unwrap(), b"keep");
}

#[test]
fn content_addressing_does_not_replace_validation_of_snapshot_and_plan_records() {
    let fixture = Fixture::new();
    fixture.write("a", "old");
    let target = fixture.capture();
    let snapshot = fixture.archive.snapshot_record(&target).unwrap();
    let original = serde_json::to_value(&snapshot).unwrap();
    for field in ["version", "workspace", "escape", "protected", "unknown"] {
        let mut value = original.clone();
        match field {
            "version" => value["version"] = json!(2),
            "workspace" => value["workspace"] = json!("/another/workspace"),
            "escape" => value["files"]["../escape"] = value["files"]["a"].clone(),
            "protected" => value["files"][".git/private"] = value["files"]["a"].clone(),
            "unknown" => value["futureMeaning"] = json!(true),
            _ => unreachable!(),
        }
        let malformed = fixture.record(&value);
        assert!(
            fixture.archive.snapshot_record(&malformed).is_err(),
            "{field}"
        );
    }
    fixture.write("a", "new");
    let plan = fixture
        .archive
        .prepare(&target, &Limits::default())
        .unwrap();
    let mut value = serde_json::to_value(fixture.archive.plan_record(&plan).unwrap()).unwrap();
    value["changes"][0]["path"] = json!("../escape");
    let malformed = fixture.record(&value);
    assert!(fixture.archive.plan_record(&malformed).is_err());
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"new");
}

#[test]
fn binary_capture_retains_basic_permissions_without_claiming_all_metadata() {
    let fixture = Fixture::new();
    fixture.write("tools/run", [0, 255, 0, 7]);
    fs::set_permissions(
        fixture.root.join("tools/run"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let reference = fixture.capture();
    let snapshot = fixture.archive.snapshot_record(&reference).unwrap();
    assert_eq!(snapshot.files["tools/run"].mode, 0o750);
    assert_eq!(fixture.bytes(&snapshot.files["tools/run"]), [0, 255, 0, 7]);
    assert_eq!(
        snapshot.files.keys().cloned().collect::<BTreeSet<_>>(),
        ["tools/run".to_owned()].into()
    );
}

#[test]
fn preview_limits_cover_archived_data_even_when_the_workspace_is_empty() {
    let fixture = Fixture::new();
    fixture.write("a", "1234");
    let target = fixture.capture();
    fs::remove_file(fixture.root.join("a")).unwrap();
    let plan = fixture
        .archive
        .prepare(&target, &Limits::default())
        .unwrap();
    for limits in [
        Limits {
            file_bytes: 3,
            ..Limits::default()
        },
        Limits {
            total_bytes: 3,
            ..Limits::default()
        },
        Limits {
            entries: 0,
            ..Limits::default()
        },
    ] {
        assert!(fixture.archive.prepare(&target, &limits).is_err());
        assert!(fixture.archive.validate_plan(&plan, &limits).is_err());
    }
    assert!(!fixture.root.join("a").exists());
}

#[test]
fn metadata_serialization_stops_at_the_limit_instead_of_buffering_everything() {
    use serde::ser::SerializeSeq;
    use std::cell::Cell;

    struct LargeRecord<'a> {
        written: &'a Cell<usize>,
        chunk: String,
    }

    impl serde::Serialize for LargeRecord<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(Some(1024))?;
            for _ in 0..1024 {
                self.written.set(self.written.get() + 1);
                sequence.serialize_element(&self.chunk)?;
            }
            sequence.end()
        }
    }

    let fixture = Fixture::new();
    let objects = Objects::open(&fixture.storage).unwrap();
    let written = Cell::new(0);
    let record = LargeRecord {
        written: &written,
        chunk: "x".repeat(64 * 1024),
    };
    assert!(objects.put_record(&record).is_err());
    assert!(
        written.get() < 1024,
        "serialization must stop before producing the entire oversized value"
    );
    assert_eq!(fs::read_dir(&fixture.storage).unwrap().count(), 1);
}
