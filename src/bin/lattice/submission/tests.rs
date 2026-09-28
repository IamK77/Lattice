use super::*;

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
        assert!(draft.editor().is_empty());
    }
}
