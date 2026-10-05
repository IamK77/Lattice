//! Ordinary-terminal questions. Inquire restores its terminal on prompt return.
use super::{
    i18n::{self, Id as M, Language},
    input::Field,
    Error, Questions, Result,
};
use inquire::{
    error::InquireError, validator::Validation, Confirm, MultiSelect, Password,
    PasswordDisplayMode, Select, Text,
};

pub struct Terminal {
    pub language: Language,
}
impl Terminal {
    fn render_config(&self) -> inquire::ui::RenderConfig<'static> {
        inquire::ui::RenderConfig::default().with_canceled_prompt_indicator(
            inquire::ui::Styled::new(i18n::phrase(self.language, M::Back)),
        )
    }
}

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
fn choices(options: &[String]) -> Vec<Choice> {
    options
        .iter()
        .enumerate()
        .map(|(index, label)| Choice {
            index,
            label: label.clone(),
        })
        .collect()
}
fn error(error: InquireError) -> Error {
    match error {
        InquireError::OperationCanceled => Error::Back,
        InquireError::OperationInterrupted => Error::Cancelled,
        other => Error::Failed(format!("terminal prompt failed: {other}")),
    }
}
fn validation(field: &Field, language: Language, text: &str) -> Validation {
    match field.validate(text) {
        Ok(()) => Validation::Valid,
        Err(id) => Validation::Invalid(i18n::text(language, id, &[]).into()),
    }
}
impl Questions for Terminal {
    fn language(&self) -> Language {
        self.language
    }
    fn set_language(&mut self, language: Language) {
        self.language = language;
    }
    fn tell(&mut self, message: &str) {
        let safe: String = message
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        eprintln!("{safe}");
    }
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize> {
        Ok(Select::new(message, choices(options))
            .with_page_size(9)
            .with_help_message(&self.label(M::SelectHelp))
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)?
            .index)
    }
    fn multi_select(
        &mut self,
        message: &str,
        options: &[String],
        selected: &[usize],
    ) -> Result<Vec<usize>> {
        Ok(MultiSelect::new(message, choices(options))
            .with_default(selected)
            .with_help_message(&self.label(M::MultiHelp))
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)?
            .into_iter()
            .map(|choice| choice.index)
            .collect())
    }
    fn text(&mut self, message: &str, default: &str) -> Result<String> {
        Text::new(message)
            .with_initial_value(default)
            .with_help_message(&self.label(M::InputHelp))
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)
    }
    fn input(
        &mut self,
        message: &str,
        default: &str,
        field: Field,
        suggestions: &[String],
    ) -> Result<String> {
        let language = self.language;
        let check = field.clone();
        let suggestions = suggestions.to_vec();
        let help = self.label(if matches!(field, Field::Tokens | Field::OutputTokens(_)) {
            M::TokenHelp
        } else {
            M::InputHelp
        });
        let formatter = |value: &str| field.answer(value);
        Text::new(message)
            .with_initial_value(default)
            .with_help_message(&help)
            .with_validator(move |value: &str| Ok(validation(&check, language, value)))
            .with_autocomplete(move |query: &str| {
                let query = query.to_ascii_lowercase();
                Ok(suggestions
                    .iter()
                    .filter(|s| s.to_ascii_lowercase().contains(&query))
                    .cloned()
                    .collect())
            })
            .with_formatter(&formatter)
            .with_render_config(self.render_config())
            .prompt()
            .map(|s| s.trim().to_owned())
            .map_err(error)
    }
    fn secret(&mut self, message: &str) -> Result<String> {
        let language = self.language;
        Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .with_help_message(&self.label(M::SecretHelp))
            .with_validator(move |value: &str| Ok(validation(&Field::Key, language, value)))
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)
    }
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool> {
        let yes = self.label(M::Yes);
        let no = self.label(M::No);
        let formatter = |value| if value { yes.clone() } else { no.clone() };
        Confirm::new(message)
            .with_default(default)
            .with_help_message(&self.label(M::ConfirmHelp))
            .with_error_message(&self.label(M::ConfirmHelp))
            .with_formatter(&formatter)
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)
    }
}
