//! Keep process stderr off the terminal while the full-screen frontend owns it.
//! Capture bytes to a private per-run file, not an unbounded in-memory queue.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

pub(super) struct Capture {
    previous: Option<OwnedFd>,
    file: File,
    path: PathBuf,
    seen: u64,
}

fn duplicate_to(source: i32, destination: i32) -> io::Result<()> {
    loop {
        if unsafe { libc::dup2(source, destination) } >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

impl Capture {
    pub fn start(directory: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let temporary = tempfile::Builder::new()
            .prefix("terminal-diagnostics-")
            .suffix(".log")
            .tempfile_in(directory)?;
        let (file, path) = temporary.keep().map_err(|error| error.error)?;
        let mut stderr = io::stderr().lock();
        stderr.flush()?;
        let fd = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let previous = unsafe { OwnedFd::from_raw_fd(fd) };
        duplicate_to(file.as_raw_fd(), libc::STDERR_FILENO)?;
        Ok(Self {
            previous: Some(previous),
            file,
            path,
            seen: 0,
        })
    }

    /// Called by the existing draw loop. Only stat this already-open file; do
    /// not parse diagnostic text, inject control characters or modify input.
    pub fn notice(&mut self) -> io::Result<Option<String>> {
        let length = self.file.metadata()?.len();
        if length == self.seen {
            return Ok(None);
        }
        self.seen = length;
        Ok(Some(self.description()))
    }

    pub fn description(&self) -> String {
        let path: String = self
            .path
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        format!("Diagnostics saved to {path}")
    }

    pub fn has_output(&self) -> io::Result<bool> {
        Ok(self.file.metadata()?.len() > 0)
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if let Some(previous) = self.previous.as_ref() {
            let mut stderr = io::stderr().lock();
            stderr.flush()?;
            duplicate_to(previous.as_raw_fd(), libc::STDERR_FILENO)?;
            self.previous = None;
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Also restore on early returns. Never print a restoration failure onto
        // an active screen; retain it in the same diagnostic file instead.
        if let Err(error) = self.restore() {
            let _ = writeln!(self.file, "cannot restore terminal stderr: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn stderr_is_retained_off_screen_and_restored_on_scope_exit() {
        const CHILD: &str = "LATTICE_DIAGNOSTICS_TEST_DIR";
        if let Some(directory) = std::env::var_os(CHILD) {
            let path;
            {
                let mut capture = Capture::start(Path::new(&directory)).unwrap();
                path = capture.path.clone();
                assert!(capture.notice().unwrap().is_none());
                eprintln!("recovery-marker-main");
                std::thread::spawn(|| eprintln!("recovery-marker-background"))
                    .join()
                    .unwrap();
                assert!(capture
                    .notice()
                    .unwrap()
                    .unwrap()
                    .starts_with("Diagnostics saved to "));
                assert!(capture.notice().unwrap().is_none());
                assert!(capture.has_output().unwrap());
            }
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("recovery-marker-main"));
            assert!(text.contains("recovery-marker-background"));
            eprintln!("restored-stderr-marker");
            return;
        }
        // Redirecting fd 2 is process-wide. Isolate the test instead of racing
        // other tests or altering the test runner's own stderr.
        let directory = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal_host::diagnostics::tests::stderr_is_retained_off_screen_and_restored_on_scope_exit",
                "--nocapture",
            ])
            .env(CHILD, directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("restored-stderr-marker"));
        assert!(!stderr.contains("recovery-marker-"));
    }
}
