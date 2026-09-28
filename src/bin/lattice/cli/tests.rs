use super::*;

fn request(mode: Option<&str>, tail: &[&str]) -> Command {
    parse(mode.map(str::to_string), tail.iter().map(OsString::from)).unwrap()
}

#[test]
fn launch_modes_keep_only_the_existing_aliases_and_optional_resume_name() {
    assert_eq!(request(None, &[]), Command::Tui(Resume::Fresh));
    for mode in ["-c", "--continue", "--resume"] {
        assert_eq!(request(Some(mode), &[]), Command::Tui(Resume::Latest));
    }
    assert_eq!(
        request(Some("--resume"), &["named", "ignored"]),
        Command::Tui(Resume::Named("named".into()))
    );
    assert_eq!(
        request(Some("--resume"), &[""]),
        Command::Tui(Resume::Named(String::new()))
    );
    assert_eq!(request(Some("component"), &[]), Command::Component(None));
    assert_eq!(
        request(Some("component"), &["bridge", "ignored"]),
        Command::Component(Some("bridge".into()))
    );
    for mode in ["--version", "-V", "version"] {
        assert_eq!(request(Some(mode), &["ignored"]), Command::Version);
    }
    for mode in ["--help", "-h", "help"] {
        assert_eq!(request(Some(mode), &["ignored"]), Command::Help);
    }
    for mode in ["-r", "recover", "run", "assemble", "Serve", ""] {
        assert_eq!(request(Some(mode), &[]), Command::Unknown(mode.into()));
    }
}

#[test]
fn command_tails_remain_with_their_existing_consumers() {
    let args = vec!["one".into(), "--option".into()];
    for (mode, expected) in [
        ("--recover", Command::Recover(args.clone())),
        ("compact", Command::Compact(args.clone())),
        ("export", Command::Export(args.clone())),
        ("debug-frame", Command::DebugFrame(args.clone())),
        ("debug-tui", Command::DebugTui(args)),
    ] {
        assert_eq!(request(Some(mode), &["one", "--option"]), expected);
    }
    for (mode, expected) in [
        ("serve", Command::Serve),
        ("prompt", Command::Prompt),
        ("assembly", Command::Assembly),
        ("index", Command::Index),
        ("tidy", Command::Tidy),
    ] {
        assert_eq!(request(Some(mode), &["one", "--option"]), expected);
    }
}

#[test]
fn offline_paths_require_exactly_one_argument_with_the_original_diagnostic() {
    for (mode, usage) in [
        (
            "--migrate-ledger",
            "usage: lattice --migrate-ledger OFFLINE_LEGACY.jsonl",
        ),
        (
            "--verify-ledger",
            "usage: lattice --verify-ledger SEGMENTED_LEDGER_DIRECTORY",
        ),
    ] {
        for args in [vec![], vec![OsString::from("one"), OsString::from("two")]] {
            let error = parse(Some(mode.into()), args.into_iter()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(error.to_string(), usage);
        }
    }
    assert_eq!(
        request(Some("--migrate-ledger"), &["a path.jsonl"]),
        Command::Migrate(PathBuf::from("a path.jsonl"))
    );
    assert_eq!(
        request(Some("--verify-ledger"), &["a path.ledger"]),
        Command::Verify(PathBuf::from("a path.ledger"))
    );
}

#[cfg(unix)]
#[test]
fn raw_paths_and_unconsumed_tails_are_not_forced_through_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'p', 0xff]);
    for (mode, expected) in [
        ("--migrate-ledger", Command::Migrate(PathBuf::from(&raw))),
        ("--verify-ledger", Command::Verify(PathBuf::from(&raw))),
    ] {
        assert_eq!(
            parse(Some(mode.into()), [raw.clone()].into_iter()).unwrap(),
            expected
        );
    }
    for mode in [
        "--continue",
        "serve",
        "prompt",
        "assembly",
        "index",
        "tidy",
        "--help",
        "--version",
        "unknown",
    ] {
        assert!(parse(Some(mode.into()), [raw.clone()].into_iter()).is_ok());
    }
    for mode in ["--resume", "component"] {
        assert!(parse(
            Some(mode.into()),
            [OsString::from("name"), raw.clone()].into_iter()
        )
        .is_ok());
    }
    assert!(
        std::panic::catch_unwind(|| parse(Some("--recover".into()), [raw].into_iter())).is_err()
    );
}
