//! Ordinary-terminal questions. Inquire restores its terminal on prompt return.
use super::{Error, Questions, Result};
use inquire::{error::InquireError, Confirm, Password, PasswordDisplayMode, Select, Text};

pub struct Terminal;

struct Choice {
    index: usize,
    label: String,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            &self
                .label
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>(),
        )
    }
}

fn error(error: InquireError) -> Error {
    match error {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => Error::Cancelled,
        other => Error::Failed(format!("terminal prompt failed: {other}")),
    }
}

impl Questions for Terminal {
    fn tell(&mut self, message: &str) {
        let safe: String = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        eprintln!("{safe}");
    }
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize> {
        let choices = options
            .iter()
            .enumerate()
            .map(|(index, label)| Choice {
                index,
                label: label.clone(),
            })
            .collect();
        Ok(Select::new(message, choices).prompt().map_err(error)?.index)
    }
    fn text(&mut self, message: &str, default: &str) -> Result<String> {
        Text::new(message)
            .with_default(default)
            .prompt()
            .map_err(error)
    }
    fn secret(&mut self, message: &str) -> Result<String> {
        Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()
            .map_err(error)
    }
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool> {
        Confirm::new(message)
            .with_default(default)
            .prompt()
            .map_err(error)
    }
}
