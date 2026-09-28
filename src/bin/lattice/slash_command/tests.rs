use super::*;

#[test]
fn command_names_aliases_and_arguments_have_one_interpretation() {
    for (text, expected) in [
        ("/exit ignored", Intent::Exit),
        ("/quit", Intent::Exit),
        ("/btw  a  b ", Intent::Open("a  b")),
        ("/tab", Intent::Next),
        ("/tab 0", Intent::Select(0)),
        ("/tab +2", Intent::Select(2)),
        ("/back ignored", Intent::Parent),
        ("/clear ignored", Intent::Clear),
        ("/help", Intent::Panel(AT_COMMANDS)),
        ("/context", Intent::Panel(AT_CONTEXT)),
        ("/usage", Intent::Panel(AT_USAGE)),
        ("/config", Intent::Panel(AT_CONFIG)),
        ("/components", Intent::Panel(AT_COMPONENTS)),
        ("/background", Intent::Panel(AT_BACKGROUND)),
        ("/effort nonsense", Intent::Effort("nonsense")),
        ("/thinking high", Intent::Effort("high")),
        ("/model  a  b ", Intent::Model("a  b")),
        ("/compact \t ", Intent::Compact),
        ("/uninstall  a  b ", Intent::Uninstall("a  b")),
        ("/remove a", Intent::Uninstall("a")),
    ] {
        assert_eq!(parse(text), expected, "{text:?}");
    }
}

#[test]
fn malformed_parameters_and_unknown_names_preserve_diagnostics() {
    for (text, expected) in [
        ("/tab -1", "Use /tab or /tab <number>"),
        (
            "/tab 999999999999999999999999999999999999999",
            "Use /tab or /tab <number>",
        ),
        ("/tab\t2", "unknown command /tab\t2 — try /help"),
        (" /tab 2", "unknown command  — try /help"),
        (
            "/unknown private arguments",
            "unknown command /unknown — try /help",
        ),
        ("/compact no", "usage: /compact (no arguments)"),
        (
            "/remove  ",
            "usage: /uninstall <instance>  — the name an install gave it",
        ),
        (
            "/uninstall",
            "usage: /uninstall <instance>  — the name an install gave it",
        ),
    ] {
        assert_eq!(parse(text), Intent::Notice(expected.into()), "{text:?}");
    }
}
