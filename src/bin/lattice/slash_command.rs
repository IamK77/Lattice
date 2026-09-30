//! Interpret frontend command text without executing it or consulting UI state.
use crate::terminal_host::panels::{
    AT_BACKGROUND, AT_COMMANDS, AT_COMPONENTS, AT_CONFIG, AT_CONTEXT, AT_USAGE,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Intent<'a> {
    Exit,
    Open(&'a str),
    Next,
    Select(usize),
    Parent,
    Clear,
    Panel((usize, usize)),
    Effort(&'a str),
    Model(&'a str),
    Compact,
    Uninstall(&'a str),
    Notice(String),
}

pub(super) fn parse(line: &str) -> Intent<'_> {
    // Only ASCII space separates the name. Do not normalize the caller's text.
    let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    match name {
        "/exit" | "/quit" => Intent::Exit,
        "/btw" => Intent::Open(rest),
        "/tab" if rest.is_empty() => Intent::Next,
        "/tab" => match rest.parse::<usize>() {
            Ok(index) => Intent::Select(index),
            Err(_) => Intent::Notice("Use /tab or /tab <number>".into()),
        },
        "/back" => Intent::Parent,
        "/clear" => Intent::Clear,
        "/help" => Intent::Panel(AT_COMMANDS),
        "/context" => Intent::Panel(AT_CONTEXT),
        "/background" => Intent::Panel(AT_BACKGROUND),
        "/experts" => Intent::Panel(crate::terminal_host::panels::AT_EXPERTS),
        "/usage" => Intent::Panel(AT_USAGE),
        "/config" => Intent::Panel(AT_CONFIG),
        "/components" => Intent::Panel(AT_COMPONENTS),
        "/effort" | "/thinking" => Intent::Effort(rest),
        "/model" => Intent::Model(rest),
        "/compact" if !rest.is_empty() => Intent::Notice("usage: /compact (no arguments)".into()),
        "/compact" => Intent::Compact,
        "/uninstall" | "/remove" if rest.is_empty() => {
            Intent::Notice("usage: /uninstall <instance>  — the name an install gave it".into())
        }
        "/uninstall" | "/remove" => Intent::Uninstall(rest),
        _ => Intent::Notice(format!("unknown command {name} — try /help")),
    }
}

#[cfg(test)]
#[path = "slash_command/tests.rs"]
mod tests;
