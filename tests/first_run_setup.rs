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
}
impl Terminal {
    fn start(home: &std::path::Path) -> Self {
        Self::with_args(home, &[])
    }
    fn with_args(home: &std::path::Path, args: &[&str]) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
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
        let slave = unsafe { File::from_raw_fd(slave) };
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
            .env("LATTICE_OVERLAY", "")
            .current_dir(home)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
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
            if self.bytes[query_start..]
                .windows(4)
                .any(|w| w == b"\x1b[6n")
            {
                self.master.write_all(b"\x1b[1;1R").unwrap();
            }
            assert!(
                self.bytes.len() < 2_000_000,
                "unexpected unbounded terminal output"
            );
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.mark = self.bytes.len();
        self.master.write_all(bytes).unwrap();
    }
    fn exited_cleanly(&mut self) {
        assert!(self.child.wait().unwrap().success());
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
        assert!(!terminal.bytes.windows(8).any(|w| w == b"\x1b[?1049h"));
    }
}

#[test]
fn guided_save_hands_off_to_tui_without_echoing_key_or_testing_automatically() {
    let home = tempfile::tempdir().unwrap();
    let mut terminal = Terminal::start(home.path());
    terminal.wait("Choose how to configure the model");
    terminal.send(b"\r");
    terminal.wait("Local name for this model");
    terminal.send(b"\r");
    terminal.wait("How should Lattice obtain the key?");
    terminal.send(b"\r");
    terminal.wait("API key");
    terminal.send(b"SYNTHETIC_PTY_KEY_NO_ECHO\r");
    terminal.wait("Save as the default model");
    terminal.send(b"\r");
    terminal.wait("Review complete");
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
    assert!(!String::from_utf8_lossy(&terminal.bytes).contains("SYNTHETIC_PTY_KEY_NO_ECHO"));
    assert!(!home.path().join(".lattice/setup-tests").exists());
    assert!(home.path().join(".lattice/ledgers").is_dir());
    let preferences: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".lattice/preferences.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(preferences["model"], "deepseek");
    // A subsequent launch uses the saved entry without repeating setup.
    let mut second = Terminal::with_args(home.path(), &["-c"]);
    second.wait("\x1b[?1049h");
    second.send(b"\x04");
    second.wait("\x1b[?1049l");
    second.exited_cleanly();
    assert!(!String::from_utf8_lossy(&second.bytes).contains("Choose how to configure the model"));
}
