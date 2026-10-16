use super::*;
use std::io::{self, Write};

struct FailingOutput {
    bytes: Vec<u8>,
    limit: usize,
    fail_final_flush: bool,
    flushes: usize,
}
impl Write for FailingOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(self.limit.saturating_sub(self.bytes.len()));
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic draw failure",
            ));
        }
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.fail_final_flush && self.flushes == 2 {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic flush failure",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn pending_diagnostics_survive_partial_draw_and_final_flush_failures() {
    for final_flush in [false, true] {
        let mut notes = vec!["HTTP 401; safe request failure".into()];
        let expected = notes.clone();
        let mut out = FailingOutput {
            bytes: vec![],
            limit: if final_flush { usize::MAX } else { 25 },
            fail_final_flush: final_flush,
            flushes: 0,
        };
        let drawn = screen::draw_to(&mut out, &notes).map_err(io_error);
        assert!(finish_display(&mut notes, drawn).is_err());
        assert!(
            !out.bytes.is_empty(),
            "this must fail after drawing has begun"
        );
        assert_eq!(
            notes, expected,
            "undelivered notes remain available for fallback"
        );
        let mut success = Vec::new();
        let drawn = screen::draw_to(&mut success, &notes).map_err(io_error);
        finish_display(&mut notes, drawn).unwrap();
        assert!(
            notes.is_empty(),
            "successful display must not replay old diagnostics"
        );
        assert!(String::from_utf8(success).unwrap().contains(&expected[0]));
    }
}
