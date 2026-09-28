//! Read-only iteration over a logical ledger. Never open a writer to list or
//! summarize history, especially while another process is appending to it.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(super) struct Lines {
    paths: std::vec::IntoIter<PathBuf>,
    current: Option<io::Lines<BufReader<File>>>,
}

pub(super) fn lines(path: &Path) -> io::Result<Lines> {
    Ok(Lines {
        paths: crate::EventLog::source_paths(path)?.into_iter(),
        current: None,
    })
}

impl Iterator for Lines {
    type Item = io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.current.as_mut().and_then(Iterator::next) {
                return Some(line);
            }
            self.current = None;
            let path = self.paths.next()?;
            match File::open(path) {
                Ok(file) => self.current = Some(BufReader::new(file).lines()),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

pub(super) fn bytes(path: &Path) -> io::Result<u64> {
    crate::EventLog::source_paths(path)?
        .into_iter()
        .try_fold(0u64, |total, file| {
            total
                .checked_add(file.metadata()?.len())
                .ok_or_else(|| io::Error::other("ledger byte count overflow"))
        })
}

pub(super) fn visit_appended(
    path: &Path,
    offset: u64,
    mut visit: impl FnMut(&crate::EventEnvelope) -> io::Result<()>,
) -> io::Result<u64> {
    let files = crate::EventLog::source_paths(path)?;
    let mut sizes = Vec::with_capacity(files.len());
    let mut total = 0u64;
    for file in &files {
        let size = file.metadata()?.len();
        total = total
            .checked_add(size)
            .ok_or_else(|| io::Error::other("ledger byte count overflow"))?;
        sizes.push(size);
    }
    if offset > total {
        return Err(io::Error::other("ledger shrank behind its read cursor"));
    }
    let mut skip = offset;
    let mut committed = offset;
    for (path, size) in files.into_iter().zip(sizes) {
        if skip >= size {
            skip -= size;
            continue;
        }
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(skip))?;
        let mut remaining = size - skip;
        skip = 0;
        let mut input = BufReader::new(file.take(remaining));
        let mut line = Vec::new();
        while remaining > 0 {
            line.clear();
            let read = input.read_until(b'\n', &mut line)? as u64;
            if read == 0 {
                return Err(io::Error::other(
                    "ledger changed while reading appended records",
                ));
            }
            remaining -= read;
            // A live writer may still be halfway through its next record.
            if !line.ends_with(b"\n") {
                return Ok(committed);
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                let event = serde_json::from_slice(&line)?;
                visit(&event)?;
            }
            committed += read;
        }
    }
    Ok(committed)
}

pub(super) fn cwd(path: &Path) -> io::Result<Option<String>> {
    for path in crate::EventLog::source_paths(path)?.into_iter().rev() {
        if let Some(cwd) = super::recorded_cwd(&mut File::open(path)?)? {
            return Ok(Some(cwd));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core_events as ce, EventDraft, EventLog};
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn appended_cursor_crosses_rotations_without_recounting_or_consuming_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = super::super::named_path(dir.path(), "main");
        assert_eq!(path.extension().unwrap(), "ledger");
        let mut log =
            EventLog::open_segmented(ce::core_event_decls(), "main", path.clone(), 1).unwrap();
        let mut originals = Vec::new();
        let mut seen = Vec::new();
        let mut cursor = 0;
        for number in 1..=3 {
            originals.push(
                log.append(
                    EventDraft::new(ce::USER_MESSAGE, &[], json!({"text":number.to_string()})),
                    "ui",
                )
                .unwrap(),
            );
            cursor = visit_appended(&path, cursor, |event| {
                seen.push(event.id.clone());
                Ok(())
            })
            .unwrap();
            assert_eq!(seen.len(), number);
            assert_eq!(
                visit_appended(&path, cursor, |_| panic!("old record revisited")).unwrap(),
                cursor
            );
        }
        assert_eq!(lines(&path).unwrap().count(), 3);
        let legacy = dir.path().join("old.jsonl");
        let first = format!("{}\n", serde_json::to_string(&originals[0]).unwrap());
        let second = format!("{}\n", serde_json::to_string(&originals[1]).unwrap());
        let split = second.len() / 2;
        std::fs::write(&legacy, format!("{first}{}", &second[..split])).unwrap();
        let mut count = 0;
        let cursor = visit_appended(&legacy, 0, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(cursor, first.len() as u64);
        let mut output = std::fs::OpenOptions::new()
            .append(true)
            .open(&legacy)
            .unwrap();
        output.write_all(&second.as_bytes()[split..]).unwrap();
        let cursor = visit_appended(&legacy, cursor, |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(cursor, (first.len() + second.len()) as u64);
        assert_eq!(super::super::named_path(dir.path(), "old"), legacy);
        output.write_all(b"{}\n").unwrap();
        assert!(visit_appended(&legacy, cursor, |_| Ok(())).is_err());
        std::fs::write(&legacy, "").unwrap();
        assert!(visit_appended(&legacy, cursor, |_| Ok(())).is_err());
    }
}
