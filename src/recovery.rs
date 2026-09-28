//! Host-owned, read-only access that does not open an EventLog or build an assembly.
//! A window is an observation of a pinned file handle, not a consistent snapshot
//! of a file another process may still be writing. No JSON body is materialized.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

pub const DEFAULT_WINDOW_BYTES: usize = 16 * 1024;
pub const MAX_WINDOW_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub offset: u64,
    pub next_offset: u64,
    pub observed_file_bytes: u64,
    pub starts_on_line_boundary: bool,
    pub ends_on_line_boundary: bool,
    pub reaches_observed_end: bool,
    pub utf8_lossy: bool,
    pub text: String,
}

pub struct Reader {
    file: File,
}

impl Reader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A FIFO must not strand the independent recovery entry while open
            // waits for a writer. Check the opened handle, not a racy path probe.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recovery requires a regular file",
            ));
        }
        Ok(Self { file })
    }

    /// None starts at the bounded tail. Exact byte positions also work inside
    /// malformed JSON, a huge line, or an incomplete UTF-8 sequence.
    pub fn window(&mut self, offset: Option<u64>, bytes: usize) -> io::Result<Window> {
        if bytes == 0 || bytes > MAX_WINDOW_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("recovery window must contain 1..={MAX_WINDOW_BYTES} bytes"),
            ));
        }
        let total = self.file.metadata()?.len();
        let offset = offset.unwrap_or_else(|| total.saturating_sub(bytes as u64));
        if offset > total {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recovery offset is beyond the observed file end",
            ));
        }
        let starts_on_line_boundary = if offset == 0 {
            true
        } else {
            self.file.seek(SeekFrom::Start(offset - 1))?;
            let mut previous = [0];
            self.file.read_exact(&mut previous)?;
            previous[0] == b'\n'
        };
        self.file.seek(SeekFrom::Start(offset))?;
        let count = (total - offset).min(bytes as u64) as usize;
        let mut raw = vec![0; count];
        // A concurrent truncation is an error, not a shorter successful window.
        self.file.read_exact(&mut raw)?;
        let next_offset = offset + count as u64;
        let ends_on_line_boundary = raw
            .last()
            .map_or(starts_on_line_boundary, |byte| *byte == b'\n');
        let text = String::from_utf8_lossy(&raw);
        Ok(Window {
            offset,
            next_offset,
            observed_file_bytes: total,
            starts_on_line_boundary,
            ends_on_line_boundary,
            reaches_observed_end: next_offset == total,
            utf8_lossy: matches!(text, std::borrow::Cow::Owned(_)),
            text: text.into_owned(),
        })
    }
}

/// This branch runs before configuration, credentials, terminals, and factories.
/// JSON output quotes control bytes instead of replaying terminal instructions
/// found in a damaged or malicious ledger. Paging is by explicit byte offset.
pub fn run(args: &[String], output: &mut impl io::Write) -> io::Result<()> {
    let Some(path) = args.first().filter(|path| !path.starts_with("--")) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: lattice --recover PATH [--offset BYTES] [--bytes COUNT]",
        ));
    };
    let mut offset = None;
    let mut bytes = None;
    let (options, remainder) = args[1..].as_chunks::<2>();
    for pair in options {
        match pair[0].as_str() {
            "--offset" if offset.is_none() => {
                offset = Some(pair[1].parse::<u64>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid recovery offset")
                })?);
            }
            "--bytes" if bytes.is_none() => {
                bytes = Some(pair[1].parse::<usize>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid recovery window size")
                })?);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown or repeated recovery option",
                ))
            }
        }
    }
    if !remainder.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "recovery option requires a value",
        ));
    }
    let window =
        Reader::open(Path::new(path))?.window(offset, bytes.unwrap_or(DEFAULT_WINDOW_BYTES))?;
    serde_json::to_writer_pretty(
        &mut *output,
        &serde_json::json!({
            "mode": "read-only recovery", "path": path, "window": window,
            "notice": "Raw bounded window only; no ledger repair, assembly restore, credential lookup, or replay. The file may still be changing. This is not a consistency check.",
            "paging": "Use --offset with window.nextOffset for the next window, or an earlier byte offset to look back. No automatic resume is performed."
        }),
    )?;
    writeln!(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn damaged_and_unterminated_ledgers_are_observed_without_repair() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let original = b"{broken}\n{\"unfinished\":\"\xf0\x9f";
        file.write_all(original).unwrap();
        let mut reader = Reader::open(file.path()).unwrap();
        let first = reader.window(Some(0), 9).unwrap();
        assert_eq!(first.text, "{broken}\n");
        assert!(first.starts_on_line_boundary && first.ends_on_line_boundary);
        let tail = reader.window(Some(first.next_offset), 100).unwrap();
        assert!(tail.utf8_lossy);
        assert!(tail.reaches_observed_end);
        assert!(!tail.ends_on_line_boundary);
        assert_eq!(std::fs::read(file.path()).unwrap(), original);
    }

    #[test]
    fn a_huge_single_line_is_paged_without_loading_or_parsing_it() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![b'x'; DEFAULT_WINDOW_BYTES * 4])
            .unwrap();
        let mut reader = Reader::open(file.path()).unwrap();
        let tail = reader.window(None, DEFAULT_WINDOW_BYTES).unwrap();
        assert_eq!(tail.text.len(), DEFAULT_WINDOW_BYTES);
        assert_eq!(tail.offset, (DEFAULT_WINDOW_BYTES * 3) as u64);
        assert!(!tail.starts_on_line_boundary);
        assert!(!tail.ends_on_line_boundary);
        let first = reader.window(Some(0), 8).unwrap();
        assert_eq!(first.next_offset, 8);
        assert_eq!(first.text.len(), 8);
        assert!(!first.reaches_observed_end);
        assert!(reader.window(Some(u64::MAX), 8).is_err());
        assert!(reader.window(None, MAX_WINDOW_BYTES + 1).is_err());
        assert!(reader.window(None, 0).is_err());
    }

    #[test]
    fn empty_files_and_terminal_control_bytes_are_safe_to_display() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let empty = Reader::open(file.path()).unwrap().window(None, 8).unwrap();
        assert_eq!(empty.next_offset, 0);
        assert!(empty.reaches_observed_end);
        file.write_all(b"\x1b[2Jbad\n").unwrap();
        let mut output = Vec::new();
        run(&[file.path().to_string_lossy().into_owned()], &mut output).unwrap();
        assert!(!output.contains(&0x1b));
        let decoded: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(decoded["window"]["text"], "\x1b[2Jbad\n");
        assert_eq!(std::fs::read(file.path()).unwrap(), b"\x1b[2Jbad\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_without_a_writer_cannot_strand_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("not-a-ledger");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            tx.send(
                Reader::open(&path)
                    .err()
                    .expect("non-files must be rejected")
                    .kind(),
            )
            .unwrap();
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("opening a FIFO must not wait for a writer"),
            io::ErrorKind::InvalidInput
        );
        thread.join().unwrap();
    }

    #[test]
    fn malformed_options_are_rejected_before_opening_a_file() {
        for options in [
            vec![],
            vec!["missing", "--offset"],
            vec!["missing", "--offset", "-1"],
            vec!["missing", "--unknown", "0"],
            vec!["missing", "--bytes", "8", "--bytes", "9"],
        ] {
            let error = run(
                &options.into_iter().map(str::to_string).collect::<Vec<_>>(),
                &mut Vec::new(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
}
