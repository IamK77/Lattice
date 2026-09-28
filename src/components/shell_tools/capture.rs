//! Stream command output to evidence files while retaining only bounded preview
//! windows. Sealing closes the writer; inherited pipes may still be drained.
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) struct Capture {
    path: PathBuf,
    file: Option<File>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    max: usize,
    bytes: u64,
    newlines: u64,
    last_newline: bool,
    hash: Sha256,
    error: Option<String>,
    sealed: bool,
}

pub(super) type Reader = (Arc<Mutex<Capture>>, Arc<AtomicBool>);

impl Capture {
    pub(super) fn new(dir: &Path, max: usize, name: &str) -> Result<Self, String> {
        let staged = tempfile::Builder::new()
            .prefix(name)
            .suffix(".log")
            .tempfile_in(dir)
            .map_err(|e| e.to_string())?;
        let (file, path) = staged.keep().map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            file: Some(file),
            head: Vec::new(),
            tail: VecDeque::new(),
            max,
            bytes: 0,
            newlines: 0,
            last_newline: false,
            hash: Sha256::new(),
            error: None,
            sealed: false,
        })
    }

    pub(super) fn live_reference(&self) -> Value {
        json!({"path": self.path, "complete": false, "sealed": false})
    }

    fn feed(&mut self, chunk: &[u8]) {
        if self.sealed {
            return;
        }
        if let Some(file) = &mut self.file {
            if let Err(e) = file.write_all(chunk) {
                self.error = Some(e.to_string());
                self.file = None;
            }
        }
        self.hash.update(chunk);
        self.bytes += chunk.len() as u64;
        self.newlines += chunk.iter().filter(|&&b| b == b'\n').count() as u64;
        if let Some(last) = chunk.last() {
            self.last_newline = *last == b'\n';
        }
        let take = (self.max - self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..take]);
        if chunk.len() >= self.max {
            self.tail.clear();
            self.tail.extend(&chunk[chunk.len() - self.max..]);
        } else {
            let remove = (self.tail.len() + chunk.len()).saturating_sub(self.max);
            self.tail.drain(..remove);
            self.tail.extend(chunk);
        }
    }

    fn seal(&mut self, eof: bool) -> (String, Value) {
        if let Some(mut file) = self.file.take() {
            if let Err(e) = file.flush() {
                self.error = Some(e.to_string());
            }
        }
        self.sealed = true;
        let mut reference = json!({"path": self.path, "bytes": self.bytes, "sealed": true, "complete": eof && self.error.is_none()});
        if let Some(error) = &self.error {
            reference["captureError"] = json!(error);
            reference["bytes"] = json!(std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0));
            reference["observedBytes"] = json!(self.bytes);
        } else {
            reference["sha256"] = json!(format!("{:x}", self.hash.clone().finalize()));
        }
        let mut preview = self.preview();
        reference["previewTruncated"] =
            json!(self.bytes > self.max as u64 || preview.len() > self.max);
        if preview.len() > self.max {
            let mut end = self.max;
            while !preview.is_char_boundary(end) {
                end -= 1;
            }
            preview.truncate(end);
        }
        (preview, reference)
    }

    fn preview(&self) -> String {
        if self.bytes <= self.max as u64 {
            return String::from_utf8_lossy(&self.head).into_owned();
        }
        let half = self.max.saturating_sub(100) / 2;
        let head = String::from_utf8_lossy(&self.head);
        let tail_bytes: Vec<u8> = self.tail.iter().copied().collect();
        let tail = String::from_utf8_lossy(&tail_bytes);
        let mut opening = String::new();
        let mut head_lines = 0u64;
        for line in head.split_inclusive('\n') {
            if !line.ends_with('\n') || opening.len() + line.len() > half {
                break;
            }
            opening.push_str(line);
            head_lines += 1;
        }
        let mut closing = Vec::new();
        let mut tail_len = 0;
        // The first tail line may start partway through an original line.
        let complete_tail = tail.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        for line in complete_tail.lines().rev() {
            if tail_len + line.len() + 1 > half {
                break;
            }
            closing.push(line);
            tail_len += line.len() + 1;
        }
        closing.reverse();
        let total_lines = self.newlines + u64::from(!self.last_newline);
        let dropped = total_lines.saturating_sub(head_lines + closing.len() as u64);
        if opening.is_empty() && closing.is_empty() {
            let start = head.chars().take(half / 4).collect::<String>();
            let end = tail
                .chars()
                .rev()
                .take(half / 4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>();
            return format!("{start}\n…[output truncated; read the full log]\n{end}");
        }
        format!(
            "{opening}…[{dropped} lines omitted; read the full log]\n{}",
            closing.join("\n")
        )
    }
}

pub(super) fn drain(mut pipe: impl Read + Send + 'static, capture: Capture) -> Reader {
    let buffer = Arc::new(Mutex::new(capture));
    let writing = Arc::clone(&buffer);
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    std::thread::spawn(move || {
        let mut chunk = [0; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => writing
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .feed(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let mut capture = writing.lock().unwrap_or_else(|e| e.into_inner());
                    if !capture.sealed {
                        capture.error = Some(e.to_string());
                    }
                    break;
                }
            }
        }
        finished.store(true, Ordering::Release);
    });
    (buffer, done)
}

pub(super) fn finish(out: &Reader, err: &Reader) -> (String, String, Value, bool) {
    let grace = std::time::Instant::now() + std::time::Duration::from_millis(200);
    while std::time::Instant::now() < grace
        && !(out.1.load(Ordering::Acquire) && err.1.load(Ordering::Acquire))
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out_done = out.1.load(Ordering::Acquire);
    let err_done = err.1.load(Ordering::Acquire);
    let (stdout, out_ref) = out
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .seal(out_done);
    let (stderr, err_ref) = err
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .seal(err_done);
    (
        stdout,
        stderr,
        json!({"stdout": out_ref, "stderr": err_ref}),
        !out_done || !err_done,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_and_binary_previews_are_bounded_and_incomplete_pipes_are_honest() {
        let dir = tempfile::tempdir().unwrap();
        for max in [0, 1, 3, 40, 400] {
            let mut capture = Capture::new(dir.path(), max, "binary-").unwrap();
            capture.feed(&[255; 500]);
            let (preview, reference) = capture.seal(false);
            assert!(preview.len() <= max);
            assert_eq!(reference["complete"], false);
            assert_eq!(reference["previewTruncated"], true);
            assert_eq!(
                std::fs::read(reference["path"].as_str().unwrap())
                    .unwrap()
                    .len(),
                500
            );
        }
    }

    #[test]
    fn complete_logs_outlive_bounded_previews_and_sealed_logs_do_not_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut capture = Capture::new(dir.path(), 400, "stdout-").unwrap();
        let text = (0..1000)
            .map(|i| format!("line-{i:04}\n"))
            .collect::<String>();
        for chunk in text.as_bytes().chunks(77) {
            capture.feed(chunk);
        }
        assert!(capture.head.len() + capture.tail.len() <= 800);
        let (preview, reference) = capture.seal(true);
        assert!(preview.contains("line-0000"));
        assert!(preview.contains("line-0999"));
        assert!(preview.contains("lines omitted"));
        assert!(preview.len() <= 400);
        assert_eq!(reference["complete"], true);
        assert_eq!(
            std::fs::read_to_string(reference["path"].as_str().unwrap()).unwrap(),
            text
        );
        capture.feed(b"must not append");
        assert_eq!(
            std::fs::read_to_string(reference["path"].as_str().unwrap()).unwrap(),
            text
        );
    }
}
