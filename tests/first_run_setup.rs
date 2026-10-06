//! Real inquire prompts and TUI handoff in a private terminal; synthetic keys only.
#![cfg(unix)]
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Terminal {
    child: Child,
    master: File,
    slave: Option<File>,
    output: mpsc::Receiver<Vec<u8>>,
    bytes: Vec<u8>,
    mark: usize,
    original: libc::termios,
    parser: vt100::Parser,
}
impl Terminal {
    fn start(home: &std::path::Path) -> Self {
        Self::with_args(home, &[])
    }
    fn with_args(home: &std::path::Path, args: &[&str]) -> Self {
        Self::with_locale(home, args, "en_US.UTF-8")
    }
    fn with_locale(home: &std::path::Path, args: &[&str], locale: &str) -> Self {
        Self::with_size(home, args, locale, 40, 120)
    }
    fn with_size(
        home: &std::path::Path,
        args: &[&str],
        locale: &str,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self::with_output(home, args, locale, rows, cols, false)
    }
    fn with_output(
        home: &std::path::Path,
        args: &[&str],
        locale: &str,
        rows: u16,
        cols: u16,
        redirect_stderr: bool,
    ) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::addr_of_mut!(size),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let mut slave = unsafe { File::from_raw_fd(slave) };
        slave.write_all(b"PREVIOUS_SHELL_CONTENT\r\n").unwrap();
        let mut original = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut original) },
            0
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_lattice"));
        command
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("LANG", locale)
            .env("LATTICE_OVERLAY", "")
            .current_dir(home)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(if redirect_stderr {
                File::create(home.join("stderr.txt")).unwrap()
            } else {
                slave.try_clone().unwrap()
            }));
        unsafe {
            command.pre_exec(|| {
                #[allow(clippy::unnecessary_cast)]
                let request = libc::TIOCSCTTY as _;
                if libc::setsid() < 0 || libc::ioctl(0, request, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(command);
        let mut reader = master.try_clone().unwrap();
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 || tx.send(bytes[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            master,
            slave: Some(slave),
            output,
            bytes: vec![],
            mark: 0,
            original,
            parser: vt100::Parser::new(rows, cols, 100),
        }
    }
    fn wait(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if String::from_utf8_lossy(&self.bytes[self.mark..]).contains(text) {
                return;
            }
            let next = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!(
                        "missing {text:?}; terminal output: {}",
                        String::from_utf8_lossy(&self.bytes)
                    )
                });
            // Some terminal backends query cursor position; emulate the terminal response.
            let query_start = self.bytes.len().saturating_sub(3);
            self.bytes.extend_from_slice(&next);
            self.parser.process(&next);
            if self.bytes[query_start..]
                .windows(4)
                .any(|w| w == b"\x1b[6n")
            {
                let (row, col) = self.parser.screen().cursor_position();
                write!(self.master, "\x1b[{};{}R", row + 1, col + 1).unwrap();
            }
            assert!(
                self.bytes.len() < 2_000_000,
                "unexpected unbounded terminal output"
            );
        }
    }
    fn wait_prompt(&mut self, text: &str) {
        let previous = self.mark;
        // A submitted answer can contain the next question's wording. Wait
        // for its new page, not the previous widget's completion echo.
        self.wait("\x1b[1;1H");
        let repaint = self.bytes[self.mark..]
            .windows(6)
            .position(|w| w == b"\x1b[1;1H")
            .unwrap();
        self.mark += repaint + 6;
        self.wait(text);
        let found = self.bytes[self.mark..]
            .windows(text.len())
            .position(|w| w == text.as_bytes())
            .unwrap();
        self.mark += found + text.len();
        // Inquire shows the cursor only after the entire frame is written.
        // Seeing the question alone does not mean its options have arrived.
        self.wait("\x1b[?25h");
        self.mark = previous;
    }
    fn send(&mut self, bytes: &[u8]) {
        self.mark = self.bytes.len();
        self.master.write_all(bytes).unwrap();
    }
    fn exited_cleanly(&mut self) {
        assert!(self.child.wait().unwrap().success());
        self.assert_restored();
    }
    fn assert_restored(&self) {
        let mut now = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            // macOS revokes the slave after its session leader exits; the
            // master still exposes the terminal flags we need to verify.
            unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut now) },
            0
        );
        assert_eq!(
            now.c_lflag, self.original.c_lflag,
            "terminal local flags must be restored"
        );
        assert_eq!(
            now.c_iflag, self.original.c_iflag,
            "terminal input flags must be restored"
        );
        assert_eq!(
            now.c_oflag, self.original.c_oflag,
            "terminal output flags must be restored"
        );
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.slave.take();
    }
}

#[test]
fn escape_and_control_c_exit_the_real_guide_without_creating_a_conversation() {
    for key in [b"\x1b".as_slice(), b"\x03".as_slice()] {
        let home = tempfile::tempdir().unwrap();
        let mut terminal = Terminal::start(home.path());
        terminal.wait("Choose how to configure the model");
        assert!(!home.path().join(".lattice").exists());
        terminal.send(key);
        terminal.wait("Setup exited");
        terminal.exited_cleanly();
        assert!(!home.path().join(".lattice").exists());
        assert_eq!(
            terminal
                .bytes
                .windows(8)
                .filter(|w| *w == b"\x1b[?1049h")
                .count(),
            1
        );
        assert_eq!(
            terminal
                .bytes
                .windows(8)
                .filter(|w| *w == b"\x1b[?1049l")
                .count(),
            1
        );
        assert!(!terminal.parser.screen().alternate_screen());
        assert!(!terminal.parser.screen().hide_cursor());
        assert!(!terminal.parser.screen().bracketed_paste());
        assert!(terminal
            .parser
            .screen()
            .contents()
            .contains("PREVIOUS_SHELL_CONTENT"));
    }
}

#[test]
fn guided_save_hands_off_to_tui_without_echoing_key_or_testing_automatically() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\r");
    terminal.wait("Choose a provider");
    terminal.send(b"\x1b[B\x1b[B\r");
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_PTY_KEY_NO_ECHO\r");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("Exact model identifier");
    terminal.send(b"deepseek-flash\r");
    terminal.wait_prompt("3/3 Review configuration");
    let review = terminal.parser.screen().contents();
    assert_eq!(
        review
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
        include_str!("fixtures/first_run_setup_review.txt").trim_end(),
        "the complete page must not contain remnants of prior prompts",
    );
    assert!(review.contains("OpenAI Chat Completions"), "{review}");
    assert!(!review.contains("Choose a provider"), "{review}");
    assert!(
        !review.contains("How should Lattice obtain the key?"),
        "{review}"
    );
    assert!(!review.contains("Usage field mapping"), "{review}");
    terminal.send(b"More\r");
    terminal.wait("View complete configuration and sources");
    terminal.send(b"\r");
    terminal.wait_prompt("page 1/");
    assert!(terminal
        .parser
        .screen()
        .contents()
        .contains("Usage field mapping"));
    terminal.send(b"\r");
    terminal.wait_prompt("3/3 Review configuration");
    assert!(!terminal
        .parser
        .screen()
        .contents()
        .contains("Usage field mapping"));
    assert!(!home.path().join(".lattice").exists());
    terminal.send(b"\r");
    terminal.wait("Connection test (optional)");
    assert!(home.path().join(".lattice/models.json").exists());
    assert!(!home.path().join(".lattice/ledgers").exists());
    terminal.send(b"\r");
    terminal.wait("\x1b[?1049h");
    terminal.send(b"\x04");
    terminal.wait("\x1b[?1049l");
    terminal.exited_cleanly();
    let transitions: Vec<_> = terminal
        .bytes
        .windows(8)
        .filter(|w| *w == b"\x1b[?1049h" || *w == b"\x1b[?1049l")
        .collect();
    assert_eq!(
        transitions,
        vec![
            b"\x1b[?1049h".as_slice(),
            b"\x1b[?1049l",
            b"\x1b[?1049h",
            b"\x1b[?1049l"
        ]
    );
    assert!(!terminal.parser.screen().alternate_screen());
    assert!(terminal
        .parser
        .screen()
        .contents()
        .contains("PREVIOUS_SHELL_CONTENT"));
    assert!(!String::from_utf8_lossy(&terminal.bytes).contains("SYNTHETIC_PTY_KEY_NO_ECHO"));
    assert!(!home.path().join(".lattice/setup-tests").exists());
    assert!(home.path().join(".lattice/ledgers").is_dir());
    let preferences: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".lattice/preferences.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(preferences["model"], "deepseek-flash");
    // A subsequent launch uses the saved entry without repeating setup.
    let mut second = Terminal::with_args(home.path(), &["-c"]);
    second.wait("\x1b[?1049h");
    second.send(b"\x04");
    second.wait("\x1b[?1049l");
    second.exited_cleanly();
    assert!(!String::from_utf8_lossy(&second.bytes).contains("Choose how to configure the model"));
}

#[test]
fn chinese_setup_can_go_back_switch_language_and_remember_the_explicit_choice() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::with_locale(home.path(), &[], "zh_CN.UTF-8");
    terminal.wait("选择模型配置方式");
    assert!(!home.path().join(".lattice").exists());
    terminal.send(b"\r");
    terminal.wait("选择提供方");
    terminal.send(b"\x1b[B\x1b[B\r");
    terminal.wait("如何提供密钥？");
    terminal.send(b"\r");
    terminal.wait("API 密钥");
    terminal.send(b"SYNTHETIC_CHINESE_KEY\r");
    terminal.wait("2/3 选择模型");
    terminal.send(b"\x1b");
    terminal.wait("如何提供密钥？");
    terminal.send(b"\x1b[B\x1b[B\x1b[B\r");
    terminal.wait("2/3 选择模型");
    terminal.send(b"\x1b[B\r");
    terminal.wait("模型的准确名称");
    terminal.send(b"deepseek-flash\r");
    terminal.wait("3/3 检查配置");
    terminal.send("更多\r".as_bytes());
    terminal.wait("Language / 语言");
    terminal.send(b"Language\r");
    terminal.wait("选择配置界面语言 / Choose the setup language");
    terminal.send(b"\r");
    terminal.wait_prompt("3/3 Review configuration");
    terminal.send(b"\r");
    terminal.wait("Connection test (optional)");
    terminal.send(b"\x1b[B\x1b[B\x1b[B\r");
    terminal.wait("Setup exited");
    terminal.exited_cleanly();
    assert!(!String::from_utf8_lossy(&terminal.bytes).contains("SYNTHETIC_CHINESE_KEY"));
    assert!(!home.path().join(".lattice/ledgers").exists());
    assert!(!home.path().join(".lattice/setup-tests").exists());
    let catalog_path = home.path().join(".lattice/models.json");
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
    assert_eq!(
        catalog["models"]["deepseek-flash"]["apiKey"],
        "SYNTHETIC_CHINESE_KEY"
    );
    // Simulate a key needing repair: the saved UI language, not the locale,
    // must decide how the next guide speaks.
    catalog["models"]["deepseek-flash"]["apiKey"] = serde_json::json!("[redacted]");
    std::fs::write(&catalog_path, catalog.to_string()).unwrap();
    let mut second = Terminal::with_locale(home.path(), &[], "zh_CN.UTF-8");
    second.wait("Choose how to configure the model");
    second.send(b"\x03");
    second.wait("Setup exited");
    second.exited_cleanly();
}

#[test]
fn model_discovery_search_and_capability_edit_work_in_the_real_terminal() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /models "));
        assert!(request.contains("Bearer SYNTHETIC_PTY_DISCOVERY_KEY"));
        let body = r#"{"data":[{"id":"text-model"},{"id":"vision-model","context_window":65536,"max_output_tokens":4096,"input_modalities":["text","image"]}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("API format");
    terminal.send(b"\x1b[B\r");
    terminal.wait("API base URL");
    terminal.send(format!("http://{address}\r").as_bytes());
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_PTY_DISCOVERY_KEY\r");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\r");
    terminal.wait("Available models (type to filter)");
    terminal.send(b"vision\r");
    terminal.wait_prompt("3/3 Review configuration");
    terminal.send(b"\x1b[B\x1b[B\r");
    terminal.wait("2/3 Choose a model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("Exact model identifier");
    terminal.send(&[vec![0x7f; 12], b"vis\x1b[B\t\r".to_vec()].concat());
    terminal.wait_prompt("3/3 Review configuration");
    terminal.send(b"\x1b[B\x1b[B\x1b[B\r");
    terminal.wait("Model capability settings");
    terminal.send(b"\x1b[B\x1b[B\r");
    terminal.wait("Enabled capabilities (Space to toggle)");
    terminal.send(b" \x1b[B \x1b[B \r");
    terminal.wait("Model capability settings");
    terminal.send(b"\x1b[B\r");
    terminal.wait("Context window in tokens");
    terminal.send(b"\x7f\x7f\x7f\x7f\x7f1Mi\r");
    terminal.wait("Enter a positive whole token count");
    terminal.send(b"\x7f\r");
    terminal.wait("Maximum output tokens");
    terminal.send(b"\x7f\x7f\x7f\x7f32k\r");
    terminal.wait("Model capability settings");
    terminal.send(b"\x1b[B\x1b[B\x1b[B\r");
    terminal.wait("Supported thinking effort rungs (Space to toggle)");
    terminal.send(b"\x1b[B\x1b[B \x1b[B\x1b[B \r");
    terminal.wait("Model capability settings");
    terminal.send(b"\r");
    terminal.wait_prompt("3/3 Review configuration");
    assert!(!home.path().join(".lattice/models.json").exists());
    assert!(!home.path().join(".lattice/ledgers").exists());
    terminal.send(b"\r");
    terminal.wait("Connection test (optional)");
    terminal.send(b"\r");
    terminal.wait("\x1b[?1049h");
    terminal.send(b"\x04");
    terminal.wait("\x1b[?1049l");
    terminal.exited_cleanly();
    server.join().unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join(".lattice/models.json")).unwrap())
            .unwrap();
    let entry = &saved["models"]["vision-model"];
    assert_eq!(entry["adapter"], "responses");
    assert_eq!(entry["profile"]["acceptsImages"], false);
    assert_eq!(entry["profile"]["contextWindow"], 1_000_000);
    assert_eq!(entry["profile"]["maxOutputTokens"], 32_000);
    assert_eq!(entry["profile"]["nativeWebSearch"], true);
    assert_eq!(entry["profile"]["nativeImageGeneration"], true);
    assert_eq!(
        entry["profile"]["effort"],
        serde_json::json!(["low", "high"])
    );
    assert!(!String::from_utf8_lossy(&terminal.bytes).contains("SYNTHETIC_PTY_DISCOVERY_KEY"));
    let records: Vec<_> = std::fs::read_dir(home.path().join(".lattice/setup-tests"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(records.len(), 1);
    let journal = std::fs::read_to_string(&records[0]).unwrap();
    assert!(!journal.contains("SYNTHETIC_PTY_DISCOVERY_KEY"));
    assert!(
        !journal.contains("vision-model"),
        "discovery journals omit provider bodies"
    );
    let events: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["operation"], "models");
    assert_eq!(events[0]["phase"], "requested");
    assert_eq!(events[1]["ok"], true);
    assert_eq!(events[1]["count"], 2);
}

#[test]
fn narrow_pages_replace_each_other_and_too_small_errors_restore_the_primary_screen() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::with_size(home.path(), &[], "en_US.UTF-8", 14, 40);
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("API format");
    terminal.send(b"\r");
    terminal.wait("API base URL");
    terminal.send(format!("https://example.invalid/{}TAIL\r", "x".repeat(240)).as_bytes());
    terminal.wait_prompt("page 1/");
    let mut pages = 1;
    while terminal.parser.screen().contents().contains("Next page") {
        assert!(pages < 20, "detail pagination must terminate");
        terminal.send(b"\x1b[B\r");
        pages += 1;
        terminal.wait_prompt(&format!("page {pages}/"));
    }
    assert!(pages > 1);
    let last = terminal.parser.screen().contents();
    assert!(last.contains("TAIL"), "{last}");
    assert!(!last.contains("https://example.invalid"), "{last}");
    terminal.send(b"\r");
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\x03");
    terminal.wait("Setup exited");
    terminal.exited_cleanly();
    assert!(terminal
        .parser
        .screen()
        .contents()
        .contains("PREVIOUS_SHELL_CONTENT"));
    assert!(!home.path().join(".lattice").exists());

    let mut small = Terminal::with_size(home.path(), &[], "en_US.UTF-8", 13, 39);
    small.wait("at least 40 columns and 14 rows");
    assert!(!small.child.wait().unwrap().success());
    small.assert_restored();
    assert!(!small.parser.screen().alternate_screen());
    assert!(small
        .parser
        .screen()
        .contents()
        .contains("PREVIOUS_SHELL_CONTENT"));
    assert!(!home.path().join(".lattice").exists());
}

#[test]
fn narrow_chinese_pages_and_redirected_stderr_have_explicit_terminal_boundaries() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::with_size(home.path(), &[], "zh_CN.UTF-8", 14, 40);
    terminal.wait_prompt("选择模型配置方式");
    terminal.send(b"\r");
    terminal.wait_prompt("选择提供方");
    let page = terminal.parser.screen().contents();
    assert!(page.contains("Lattice"), "{page}");
    assert!(!page.contains("选择模型配置方式"), "{page}");
    terminal.send(b"\x03");
    terminal.wait("已退出配置");
    terminal.exited_cleanly();
    assert!(terminal
        .parser
        .screen()
        .contents()
        .contains("PREVIOUS_SHELL_CONTENT"));

    let mut redirected = Terminal::with_output(home.path(), &[], "en_US.UTF-8", 40, 120, true);
    redirected.slave.take();
    // EOF is causal: every slave descriptor has closed. A broken detection
    // must time out rather than hang the test waiting for an invisible prompt.
    loop {
        match redirected.output.recv_timeout(Duration::from_secs(10)) {
            Ok(bytes) => redirected.parser.process(&bytes),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(e) => panic!("redirected setup did not exit: {e}"),
        }
    }
    assert!(!redirected.child.wait().unwrap().success());
    redirected.assert_restored();
    let diagnostic = std::fs::read_to_string(home.path().join("stderr.txt")).unwrap();
    assert!(diagnostic.contains("interactive terminal"), "{diagnostic}");
    assert!(!diagnostic.contains('\u{1b}'));
    assert!(!redirected.parser.screen().alternate_screen());
    assert!(!home.path().join(".lattice").exists());
}

#[test]
fn long_custom_rungs_remain_searchable_in_the_actual_multiselect() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\r");
    terminal.wait("Choose a provider");
    terminal.send(b"DeepSeek\r");
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_LONG_RUNG_KEY\r");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("Exact model identifier");
    terminal.send(b"deepseek-flash\r");
    terminal.wait_prompt("3/3 Review configuration");
    terminal.send(b"capabilities\r");
    terminal.wait_prompt("Model capability settings");
    terminal.send(b"Advanced\r");
    terminal.wait_prompt("Advanced model settings");
    terminal.send(b"custom\r");
    terminal.wait("Provider effort names");
    let name = format!("{}TAIL", "x".repeat(160));
    terminal.send(&[vec![0x7f; 64], format!("{name}\r").into_bytes()].concat());
    terminal.wait_prompt("Model capability settings");
    terminal.send(b"thinking\r");
    terminal.wait_prompt("Supported thinking effort rungs");
    assert!(!terminal.parser.screen().contents().contains("TAIL"));
    // Search an invisible suffix, then deselect the checked rung. If this
    // widget searches only the clipped label, the original selection survives.
    terminal.send(b"TAIL \r");
    terminal.wait_prompt("Model capability settings");
    let page = terminal.parser.screen().contents();
    assert!(page.contains("thinking rungs: none configured"), "{page}");
    terminal.send(b"\r");
    terminal.wait_prompt("3/3 Review configuration");
    terminal.send(b"\r");
    terminal.wait("Connection test (optional)");
    terminal.send(b"Exit\r");
    terminal.wait("Setup exited");
    terminal.exited_cleanly();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join(".lattice/models.json")).unwrap())
            .unwrap();
    assert_eq!(
        saved["models"]["deepseek-flash"]["profile"]["effort"],
        serde_json::json!([])
    );
    assert!(!home.path().join(".lattice/setup-tests").exists());
}

#[test]
fn failed_requests_replace_the_error_and_busy_cancellation_restores_the_terminal() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (started, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for (index, status) in [401, 403, 401].into_iter().enumerate() {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).unwrap(), 1);
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"GET /models "));
            if index == 2 {
                started.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            write!(
                stream,
                "HTTP/1.1 {status} Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            )
            .unwrap();
        }
        listener
    });
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\x1b[B\r");
    terminal.wait("API format");
    terminal.send(b"\r");
    terminal.wait("API base URL");
    terminal.send(format!("http://{address}\r").as_bytes());
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_RETRY_KEY\r");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\r");
    terminal.wait("401");
    terminal.wait_prompt("Choose a model");
    terminal.send(b"\r");
    terminal.wait("403");
    terminal.wait_prompt("Choose a model");
    let latest = terminal.parser.screen().contents();
    assert!(latest.contains("403"), "{latest}");
    assert!(!latest.contains("401"), "{latest}");
    terminal.send(b"\r");
    terminal.wait("Fetching models");
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(!terminal.parser.screen().contents().contains("403"));
    let mut busy = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.master.as_raw_fd(), &mut busy) },
        0
    );
    assert_eq!(
        busy.c_lflag & libc::ISIG,
        0,
        "busy Ctrl+C must remain input rather than kill the process"
    );
    terminal.send(b"\x03");
    release.send(()).unwrap();
    terminal.wait("Setup exited");
    terminal.exited_cleanly();
    assert!(terminal
        .parser
        .screen()
        .contents()
        .contains("PREVIOUS_SHELL_CONTENT"));
    assert!(!terminal.parser.screen().alternate_screen());
    assert!(!home.path().join(".lattice/models.json").exists());
    assert!(!home.path().join(".lattice/ledgers").exists());
    assert!(!String::from_utf8_lossy(&terminal.bytes).contains("SYNTHETIC_RETRY_KEY"));
    assert_eq!(
        std::fs::read_dir(home.path().join(".lattice/setup-tests"))
            .unwrap()
            .count(),
        3
    );
    let listener = server.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
