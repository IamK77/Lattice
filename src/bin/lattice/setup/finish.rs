//! Report pending safe diagnostics only after terminal restoration was attempted.
use super::{i18n, screen, Error, Language, Result, M};
use std::io::{self, Write};

pub(super) fn report<T>(
    result: Result<T>,
    restored: io::Result<()>,
    pending: &[String],
    language: Language,
    output: &mut impl Write,
) -> io::Result<Option<T>> {
    let mut messages = pending.to_vec();
    let mut failed = false;
    let value = match result {
        Ok(value) => Some(value),
        Err(Error::Back | Error::Cancelled) => {
            messages.push(i18n::text(language, M::Exited, &[]));
            None
        }
        Err(Error::Failed(message)) => {
            failed = true;
            messages.push(i18n::text(language, M::Failure, &[&message]));
            None
        }
    };
    if let Err(error) = restored {
        failed = true;
        messages.push(i18n::text(
            language,
            M::RestoreFailed,
            &[&error.to_string()],
        ));
    }
    let message = screen::safe(&messages.join("\n"));
    // Return all diagnostics together to the CLI's ordinary error path. A
    // restoration failure must not replace the original request/display error.
    if failed {
        return Err(io::Error::other(message));
    }
    if !message.is_empty() {
        writeln!(output, "{message}")
            .and_then(|()| output.flush())
            .map_err(|error| {
                io::Error::other(format!(
                    "{message}\n{}",
                    i18n::text(language, M::ReportFailed, &[&error.to_string()])
                ))
            })?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_display_and_restoration_failures_are_all_retained() {
        let mut output = Vec::new();
        let error = report::<()>(
            Err(Error::Failed("synthetic display failure".into())),
            Err(io::Error::other("synthetic restoration failure")),
            &["HTTP 401; safe request failure".into()],
            Language::English,
            &mut output,
        )
        .unwrap_err()
        .to_string();
        for expected in [
            "HTTP 401",
            "synthetic display failure",
            "synthetic restoration failure",
        ] {
            assert!(error.contains(expected), "missing {expected}: {error}");
        }
        assert!(
            output.is_empty(),
            "the CLI must receive one complete error, not fragmented reports"
        );
    }

    #[test]
    fn cancellation_and_success_display_pending_notes_after_cleanup() {
        for result in [Ok(7), Err(Error::Cancelled), Err(Error::Back)] {
            let success = result.is_ok();
            let mut output = Vec::new();
            let value = report(
                result,
                Ok(()),
                &["safe pending diagnostic".into()],
                Language::English,
                &mut output,
            )
            .unwrap();
            assert_eq!(value, success.then_some(7));
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("safe pending diagnostic"));
            assert_eq!(output.contains("Setup exited"), !success);
        }
    }

    #[test]
    fn failure_to_write_fallback_returns_the_undisplayed_notes() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("synthetic fallback failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = report(
            Ok(()),
            Ok(()),
            &["safe pending diagnostic".into()],
            Language::English,
            &mut Broken,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("safe pending diagnostic"));
        assert!(error.contains("synthetic fallback failure"));
    }
}
