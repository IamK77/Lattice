//! One decoding and bounded, cancellable I/O path for hits and context.
use std::io::{self, Read};
use std::path::Path;

use grep_searcher::{Searcher, Sink, SinkMatch};

const READ_BLOCK: usize = 8192;
const HEAP_LIMIT: usize = 8 * 1024 * 1024;

struct CheckedReader<'a, R> {
    inner: R,
    cancelled: &'a dyn Fn() -> bool,
}

impl<R: Read> Read for CheckedReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::other("search cancelled"));
        }
        let length = bytes.len().min(READ_BLOCK);
        self.inner.read(&mut bytes[..length])
    }
}

struct Lines<F> {
    receive: F,
    binary: bool,
}

impl<F: FnMut(u64, &str) -> io::Result<bool>> Sink for Lines<F> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, matched: &SinkMatch<'_>) -> io::Result<bool> {
        let text = std::str::from_utf8(matched.bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        (self.receive)(matched.line_number().expect("line numbers enabled"), text)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> io::Result<bool> {
        self.binary = true;
        Ok(false)
    }
}

pub(super) fn path(
    path: &Path,
    matcher: &grep_regex::RegexMatcher,
    cancelled: &dyn Fn() -> bool,
    receive: impl FnMut(u64, &str) -> io::Result<bool>,
) -> io::Result<bool> {
    reader(std::fs::File::open(path)?, matcher, cancelled, receive)
}

fn reader(
    input: impl Read,
    matcher: &grep_regex::RegexMatcher,
    cancelled: &dyn Fn() -> bool,
    receive: impl FnMut(u64, &str) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut sink = Lines {
        receive,
        binary: false,
    };
    grep_searcher::SearcherBuilder::new()
        .binary_detection(grep_searcher::BinaryDetection::quit(0))
        .heap_limit(Some(HEAP_LIMIT))
        .line_number(true)
        .build()
        .search_reader(
            matcher,
            CheckedReader {
                inner: input,
                cancelled,
            },
            &mut sink,
        )?;
    Ok(sink.binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn stopping_at_the_last_context_line_does_not_buffer_the_following_long_line() {
        struct Counted<R> {
            inner: R,
            bytes: usize,
        }
        impl<R: Read> Read for Counted<R> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let count = self.inner.read(bytes)?;
                self.bytes += count;
                Ok(count)
            }
        }
        let mut bytes = b"first\nsecond\n".to_vec();
        bytes.resize(HEAP_LIMIT * 2, b'x');
        let mut input = Counted {
            inner: &bytes[..],
            bytes: 0,
        };
        let matcher = grep_regex::RegexMatcher::new("").unwrap();
        let binary = reader(&mut input, &matcher, &|| false, |line, _| Ok(line < 2)).unwrap();
        assert!(!binary);
        // encoding_rs_io peeks three BOM bytes before filling the search block.
        assert!(
            input.bytes <= READ_BLOCK + 3,
            "context reader consumed {} bytes",
            input.bytes
        );
    }

    #[test]
    fn cancellation_is_checked_even_when_no_lines_match() {
        struct CancelAfterRead<'a> {
            read: &'a Cell<usize>,
            cancel: &'a Cell<bool>,
        }
        impl Read for CancelAfterRead<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.read.get() >= HEAP_LIMIT * 2 {
                    return Ok(0);
                }
                bytes.fill(b'x');
                self.read.set(self.read.get() + bytes.len());
                self.cancel.set(true);
                Ok(bytes.len())
            }
        }
        let read = Cell::new(0);
        let cancel = Cell::new(false);
        let matcher = grep_regex::RegexMatcher::new("needle").unwrap();
        let result = reader(
            CancelAfterRead {
                read: &read,
                cancel: &cancel,
            },
            &matcher,
            &|| cancel.get(),
            |_, _| Ok(true),
        );
        assert!(result.is_err());
        assert!(read.get() <= READ_BLOCK);
    }

    #[test]
    fn a_line_exceeding_the_heap_limit_is_an_error_not_a_missing_match() {
        let matcher = grep_regex::RegexMatcher::new("needle").unwrap();
        let bytes = vec![b'x'; HEAP_LIMIT + READ_BLOCK];
        assert!(reader(&bytes[..], &matcher, &|| false, |_, _| Ok(true)).is_err());
    }
}
