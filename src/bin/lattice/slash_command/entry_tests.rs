use super::*;

#[test]
fn slash_panels_keep_exact_targets_and_unrelated_state() {
    for (command, target) in [
        ("/help", AT_COMMANDS),
        ("/context", AT_CONTEXT),
        ("/usage", AT_USAGE),
        ("/config", AT_CONFIG),
        ("/components", AT_COMPONENTS),
        ("/background", AT_BACKGROUND),
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old notice".into());
        ui.navigation = Some(tabs::Navigation::Parent);
        ui.draft.edit().set("unsent draft");
        ui.panel.scroll_down(17);
        ui.panel.select_row(3);
        ui.panel.toggle_details();
        assert!(!run_slash(
            &mut ui,
            &format!("{command} ignored arguments"),
            None
        ));
        assert_eq!(ui.panel.active(), Some(target), "{command}");
        assert_eq!(ui.flash.as_deref(), Some("old notice"));
        assert_eq!(ui.navigation, Some(tabs::Navigation::Parent));
        assert_eq!(ui.draft.editor().text(), "unsent draft");
        assert_eq!(ui.panel.scroll_offset(), 17);
        assert_eq!(ui.panel.selected_row(), 3);
        assert!(ui.panel.details_expanded());
        assert!(!ui.busy());
    }
}

#[test]
fn slash_navigation_keeps_ascii_separator_and_unvalidated_indices() {
    for (command, expected) in [
        ("/btw  a  b  ", tabs::Navigation::Open("a  b".into())),
        ("/btw", tabs::Navigation::Open(String::new())),
        ("/tab", tabs::Navigation::Next),
        ("/tab   ", tabs::Navigation::Next),
        ("/tab 0", tabs::Navigation::Select(0)),
        ("/tab +2", tabs::Navigation::Select(2)),
        ("/tab  2 ", tabs::Navigation::Select(2)),
        ("/back ignored", tabs::Navigation::Parent),
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old notice".into());
        assert!(!run_slash(&mut ui, command, None));
        assert_eq!(ui.navigation, Some(expected), "{command}");
        assert_eq!(ui.flash.as_deref(), Some("old notice"));
        assert!(!ui.busy());
    }
    for (command, diagnostic) in [
        ("/tab -1", "Use /tab or /tab <number>"),
        (
            "/tab 999999999999999999999999999999999999999",
            "Use /tab or /tab <number>",
        ),
        ("/tab\t2", "unknown command /tab\t2 — try /help"),
        (" /tab 2", "unknown command  — try /help"),
        (
            "/unknown secret argument",
            "unknown command /unknown — try /help",
        ),
        ("/compact no", "usage: /compact (no arguments)"),
        ("/compact  ", "no session to compact"),
        (
            "/uninstall",
            "usage: /uninstall <instance>  — the name an install gave it",
        ),
        (
            "/remove  ",
            "usage: /uninstall <instance>  — the name an install gave it",
        ),
    ] {
        let mut ui = Ui::replayed(&[]);
        ui.navigation = Some(tabs::Navigation::Parent);
        assert!(!run_slash(&mut ui, command, None));
        assert_eq!(ui.flash.as_deref(), Some(diagnostic), "{command}");
        assert_eq!(ui.navigation, Some(tabs::Navigation::Parent));
        assert!(!ui.busy());
    }
}

#[test]
fn slash_uninstall_without_session_is_silent_and_exit_aliases_ignore_arguments() {
    for command in ["/uninstall  target with spaces  ", "/remove target"] {
        let mut ui = Ui::replayed(&[]);
        ui.flash = Some("old notice".into());
        ui.draft.edit().set("unsent");
        assert!(!run_slash(&mut ui, command, None));
        assert_eq!(ui.flash.as_deref(), Some("old notice"));
        assert_eq!(ui.draft.editor().text(), "unsent");
        assert!(!ui.busy());
    }
    for command in ["/exit ignored", "/quit ignored"] {
        assert!(run_slash(&mut Ui::replayed(&[]), command, None));
    }
}
