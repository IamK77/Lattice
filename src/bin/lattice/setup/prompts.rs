//! Inquire owns each widget; Screen owns the bounded setup-only display lifetime.
use super::{
    i18n::{self, Id as M, Language},
    input::Field,
    screen::{self, Screen},
    Error, Questions, Result,
};
use inquire::{
    error::InquireError, validator::Validation, Confirm, MultiSelect, Password,
    PasswordDisplayMode, Select, Text,
};

pub struct Terminal {
    language: Language,
    screen: Screen,
    title: M,
    context: Vec<String>,
    notes: Vec<String>,
}
impl Terminal {
    pub fn new(language: Language) -> std::io::Result<Self> {
        Ok(Self {
            language,
            screen: Screen::enter()?,
            title: M::PageTitle,
            context: vec![],
            notes: vec![],
        })
    }
    pub fn close(&mut self) -> std::io::Result<()> {
        self.screen.close()
    }
    fn dimensions(&self) -> Result<(usize, usize)> {
        let size = self.screen.size().map_err(io_error)?;
        if size.0 < 40 || size.1 < 14 {
            return Err(Error::Failed(self.label(M::SmallTerminal)));
        }
        Ok(size)
    }
    fn render_config(&self) -> inquire::ui::RenderConfig<'static> {
        inquire::ui::RenderConfig::default().with_canceled_prompt_indicator(
            inquire::ui::Styled::new(i18n::phrase(self.language, M::Back)),
        )
    }
    fn header(&self, width: usize) -> Vec<String> {
        let title = if self.title == M::PageTitle {
            self.label(M::PageTitle)
        } else {
            format!("Lattice · {}", self.label(self.title))
        };
        let mut lines = screen::wrap(&title, width);
        lines.push(String::new());
        lines
    }
    fn start_prompt(&mut self, message: &str, count: usize, help: &str) -> Result<usize> {
        let mut body = self.context.clone();
        body.extend(std::mem::take(&mut self.notes));
        let mut explained = false;
        loop {
            // A detail page may have outlived a window resize. Recompute the
            // outer page too instead of restoring its previous dimensions.
            let (width, height) = self.dimensions()?;
            let mut lines = self.header(width);
            let question_rows = screen::wrap(message, width.saturating_sub(4)).len();
            let help_rows = screen::wrap(help, width.saturating_sub(4)).len();
            let fixed = lines.len() + question_rows + help_rows + 4;
            let page_size = count.min(5).min(height.saturating_sub(fixed + 2).max(1));
            let room = height.saturating_sub(fixed + page_size);
            let mut wrapped = screen::wrap(&body.join("\n"), width);
            if wrapped.len() > room {
                if !explained {
                    // Never silently drop long safety explanations or errors.
                    self.show_details(M::PageDetails, &body)?;
                    explained = true;
                    continue;
                }
                let notice = screen::wrap(&self.label(M::DetailsShown), width);
                wrapped.truncate(room.saturating_sub(notice.len()));
                wrapped.extend(notice.into_iter().take(room));
            }
            lines.extend(wrapped);
            self.screen.draw(&lines).map_err(io_error)?;
            return Ok(page_size.max(1));
        }
    }
    fn show_details(&mut self, title: M, body: &[String]) -> Result<()> {
        let mut offset = 0;
        loop {
            let (width, height) = self.dimensions()?;
            let lines = screen::wrap(&body.join("\n"), width);
            let mut frame = self.header(width);
            let bound = lines.len().max(1).to_string();
            let caption = self.message(M::PageCounter, &[&self.label(title), &bound, &bound]);
            let fixed = frame.len()
                + screen::wrap(&caption, width.saturating_sub(4)).len()
                + screen::wrap(&self.label(M::PageHelp), width.saturating_sub(4)).len()
                + 6;
            let per_page = height.saturating_sub(fixed).max(1);
            let pages = lines.len().max(1).div_ceil(per_page);
            offset = offset.min(pages - 1);
            frame.extend(lines.iter().skip(offset * per_page).take(per_page).cloned());
            self.screen.draw(&frame).map_err(io_error)?;
            let mut actions = vec![M::Back];
            if offset + 1 < pages {
                actions.push(M::NextPage);
            }
            if offset > 0 {
                actions.push(M::PreviousPage);
            }
            let labels = actions.iter().map(|id| self.label(*id)).collect::<Vec<_>>();
            let message = self.message(
                M::PageCounter,
                &[
                    &self.label(title),
                    &(offset + 1).to_string(),
                    &pages.to_string(),
                ],
            );
            let result = Select::new(&message, choices(&labels, width))
                .with_page_size(3)
                .without_filtering()
                .with_help_message(&self.label(M::PageHelp))
                .with_render_config(self.render_config())
                .prompt()
                .map_err(error);
            match result {
                Ok(choice) => match actions[choice.index] {
                    M::NextPage => offset += 1,
                    M::PreviousPage => offset -= 1,
                    _ => return Ok(()),
                },
                Err(Error::Back) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }
}
struct Choice {
    index: usize,
    label: String,
    width: usize,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&screen::clip(&self.label, self.width))
    }
}
fn choices(options: &[String], columns: usize) -> Vec<Choice> {
    options
        .iter()
        .enumerate()
        .map(|(index, label)| Choice {
            index,
            label: label.clone(),
            width: columns.saturating_sub(10),
        })
        .collect()
}
fn score(query: &str, choice: &Choice, _display: &str, _index: usize) -> Option<i64> {
    // Search the complete identifier even when its visible label is shortened.
    choice
        .label
        .to_lowercase()
        .contains(&query.to_lowercase())
        .then_some(0)
}
fn fitting_suggestions(values: &[String], columns: usize) -> (Vec<String>, bool) {
    use unicode_width::UnicodeWidthStr;
    // Inquire commits a displayed Text suggestion verbatim. Unlike indexed
    // menu choices, truncating these strings would change the actual model ID.
    let fitting: Vec<_> = values
        .iter()
        .filter(|s| s.width() <= columns.saturating_sub(10))
        .cloned()
        .collect();
    let omitted = fitting.len() != values.len();
    (fitting, omitted)
}
fn io_error(error: std::io::Error) -> Error {
    Error::Failed(format!("setup display failed: {error}"))
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
    fn page(&mut self, title: M, context: &[String]) {
        self.title = title;
        self.context = context.to_vec();
    }
    fn busy(&mut self, message: M) -> Result<()> {
        let (width, height) = self.dimensions()?;
        let mut lines = self.header(width);
        lines.extend(screen::wrap(&self.context.join("\n"), width));
        lines.extend(screen::wrap(&self.label(message), width));
        if lines.len() >= height {
            lines = self.header(width);
            lines.extend(screen::wrap(&self.label(message), width));
        }
        self.screen.busy(&lines).map_err(io_error)
    }
    fn details(&mut self, title: M, lines: &[String]) -> Result<()> {
        self.show_details(title, lines)
    }
    fn language(&self) -> Language {
        self.language
    }
    fn set_language(&mut self, language: Language) {
        self.language = language;
    }
    fn tell(&mut self, message: &str) {
        self.notes.push(screen::safe(message));
    }
    fn select(&mut self, message: &str, options: &[String]) -> Result<usize> {
        let help = self.label(M::SelectHelp);
        let page_size = self.start_prompt(message, options.len(), &help)?;
        Ok(Select::new(message, choices(options, self.dimensions()?.0))
            .with_scorer(&score)
            .with_page_size(page_size)
            .with_help_message(&help)
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
        let help = self.label(M::MultiHelp);
        let page_size = self.start_prompt(message, options.len(), &help)?;
        Ok(
            MultiSelect::new(message, choices(options, self.dimensions()?.0))
                .with_scorer(&score)
                .with_default(selected)
                .with_page_size(page_size)
                .with_help_message(&help)
                .with_render_config(self.render_config())
                .prompt()
                .map_err(error)?
                .into_iter()
                .map(|choice| choice.index)
                .collect(),
        )
    }
    fn text(&mut self, message: &str, default: &str) -> Result<String> {
        let help = self.label(M::InputHelp);
        self.start_prompt(message, 0, &help)?;
        Text::new(message)
            .with_initial_value(default)
            .with_help_message(&help)
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
        let (suggestions, omitted) = fitting_suggestions(suggestions, self.dimensions()?.0);
        if omitted {
            self.say(M::LongSuggestions, &[]);
        }
        let help = self.label(if matches!(field, Field::Tokens | Field::OutputTokens(_)) {
            M::TokenHelp
        } else {
            M::InputHelp
        });
        let page_size = self.start_prompt(message, suggestions.len(), &help)?;
        let formatter = |value: &str| field.answer(value);
        Text::new(message)
            .with_initial_value(default)
            .with_help_message(&help)
            .with_page_size(page_size)
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
        let help = self.label(M::SecretHelp);
        self.start_prompt(message, 0, &help)?;
        Password::new(message)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .with_help_message(&help)
            .with_validator(move |value: &str| Ok(validation(&Field::Key, language, value)))
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)
    }
    fn confirm(&mut self, message: &str, default: bool) -> Result<bool> {
        let help = self.label(M::ConfirmHelp);
        self.start_prompt(message, 0, &help)?;
        let yes = self.label(M::Yes);
        let no = self.label(M::No);
        let formatter = |value| if value { yes.clone() } else { no.clone() };
        Confirm::new(message)
            .with_default(default)
            .with_help_message(&help)
            .with_error_message(&help)
            .with_formatter(&formatter)
            .with_render_config(self.render_config())
            .prompt()
            .map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn inline_suggestions_never_wrap_or_commit_truncated_identifiers() {
        let values = vec!["short-model".into(), "x".repeat(200), "模型".repeat(9)];
        let (fitting, omitted) = fitting_suggestions(&values, 40);
        assert!(omitted);
        assert_eq!(fitting, vec!["short-model"]);
        let (wide, omitted) = fitting_suggestions(&values, 240);
        assert!(!omitted);
        assert_eq!(wide, values);
    }

    #[test]
    fn shortened_labels_keep_the_full_searchable_identifier_and_selection() {
        let labels = vec![
            "other".into(),
            format!("{}TAIL\u{1b}\n", "模型e\u{301}".repeat(30)),
        ];
        let choices = choices(&labels, 40);
        let choice = &choices[1];
        assert!(choice.to_string().width() <= 30);
        assert!(choice.to_string().ends_with('…'));
        assert!(!choice.to_string().contains(['\u{1b}', '\n']));
        assert_eq!(score("tail", choice, &choice.to_string(), 1), Some(0));
        assert_eq!(choice.index, 1);
        assert_eq!(choice.label, labels[1]);
        assert_eq!(screen::clip("e\u{301}中文", 4), "e\u{301}中…");
    }
}
