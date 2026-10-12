use super::*;

#[test]
fn exact_skill_keeps_arguments_instead_of_selecting_a_longer_name() {
    let mut draft = Draft::new();
    draft.edit().set("/research \"a b\"  c  \n");
    let skills = vec![
        ("research".into(), String::new()),
        ("research-more".into(), String::new()),
    ];
    assert_eq!(
        submit(&mut draft, &skills, &EffortView::default()),
        Intent::Message {
            text: "/research \"a b\"  c  \n".into(),
            kept: vec![],
        }
    );
}

#[test]
fn reserved_commands_win_over_same_named_skills() {
    for name in [
        "help",
        "exit",
        "quit",
        "model",
        "permission",
        "thinking",
        "remove",
    ] {
        let mut draft = Draft::new();
        let text = format!("/{name}");
        draft.edit().set(&text);
        let skills = vec![
            (name.into(), String::new()),
            (format!("{name}-more"), String::new()),
        ];
        assert_eq!(
            submit(&mut draft, &skills, &EffortView::default()),
            Intent::Command(text)
        );
    }
}

#[test]
fn a_path_is_not_a_command() {
    for command in ["/help", "/model gpt-5", "/effort high", "/exit", "/tmp"] {
        assert!(names_a_command(command), "{command:?}");
    }
    for said in [
        "/Users/example/Downloads/notes.md",
        "/etc/hosts and /etc/passwd",
        "look at /var/log/system.log",
        "",
        "/",
        "hello",
    ] {
        assert!(!names_a_command(said), "{said:?}");
    }
}

#[test]
fn classification_uses_expanded_text_but_candidates_use_the_raw_buffer() {
    let effort = EffortView::default();
    for (input, expected) in [
        ("   ", Intent::Empty),
        (" /he ", Intent::Command("/he".into())),
        (" /unknown arg ", Intent::Command("/unknown arg".into())),
        (
            "/etc/hosts",
            Intent::Message {
                text: "/etc/hosts".into(),
                kept: vec![],
            },
        ),
        (
            "/research arg",
            Intent::Message {
                text: "/research arg".into(),
                kept: vec![],
            },
        ),
        ("/res discarded", Intent::SkillCandidate("/research".into())),
    ] {
        let mut draft = Draft::new();
        draft.edit().set(input);
        assert_eq!(
            submit(&mut draft, &[("research".into(), String::new())], &effort),
            expected,
            "{input:?}"
        );
        let retained = matches!(input, " /he " | " /unknown arg " | "/res discarded");
        assert_eq!(draft.editor().is_empty(), !retained);
        if retained {
            assert_eq!(draft.editor().text(), input);
        }
    }
}
