use super::*;

#[test]
fn url_opener_uses_one_argument_and_rejects_unsafe_links() {
    let raw = "https://example.com/a?x=$(touch%20/tmp/not-run)&y=1";
    let (message, command) = prepare(raw).unwrap();
    assert_eq!(message, format!("Opening {raw}"));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [std::ffi::OsStr::new(raw)]
    );
    assert_eq!(
        command.get_program(),
        if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }
    );
    for raw in [
        "file:///tmp/x",
        "javascript:alert(1)",
        "https://user:secret@example.com/",
        "https://example.com/\u{1b}[0m",
        "--help",
    ] {
        assert!(url_open_command(raw).is_none(), "{raw}");
        assert_eq!(
            prepare(raw).unwrap_err(),
            "Only HTTP(S) links without credentials can be opened"
        );
    }
}

#[test]
fn launcher_results_report_spawn_exit_and_success_without_a_browser() {
    let mut links = LinkOpener::default();
    let dir = tempfile::tempdir().unwrap();
    for (command, succeeds) in [
        (Command::new(dir.path().join("missing-opener")), false),
        (Command::new("false"), false),
        (Command::new("true"), true),
    ] {
        links.launch(command);
        let result = links
            .results
            .pop()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(result.is_ok(), succeeds);
        if !succeeds {
            assert!(result.as_ref().unwrap_err().contains("Could not open link"));
        }
        let (tx, rx) = mpsc::channel();
        tx.send(result).unwrap();
        links.results.push(rx);
        let failures = links.drain();
        assert_eq!(failures.is_empty(), succeeds);
        if !succeeds {
            assert!(failures[0].contains("Could not open link"));
        }
        assert!(links.results.is_empty());
    }
}
