//! The terminal host also reads transcript files, so its public failure type
//! remains io::Error. Preserve real I/O errors and eliminate the test backend's
//! impossible error without turning ordinary terminal failures into panics.

use std::convert::Infallible;
use std::io;

pub(super) trait IntoIoError {
    fn into_io_error(self) -> io::Error;
}

impl IntoIoError for io::Error {
    fn into_io_error(self) -> io::Error {
        self
    }
}

impl IntoIoError for Infallible {
    fn into_io_error(self) -> io::Error {
        match self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_errors_keep_their_kind_and_payload() {
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "fixture");
        let converted = error.into_io_error();
        assert_eq!(converted.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(converted.to_string(), "fixture");
        assert_eq!(
            io::Error::from_raw_os_error(5)
                .into_io_error()
                .raw_os_error(),
            Some(5)
        );
    }

    #[test]
    fn infallible_backend_success_needs_no_fabricated_error() {
        let result: Result<(), Infallible> = Ok(());
        let result: io::Result<()> = result.map_err(IntoIoError::into_io_error);
        assert!(result.is_ok());
    }
}
