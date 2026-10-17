use super::*;
use lattice::background_aliases::{Identity, Kind, OriginalId, Reference, Registry};

// Retained from the independent post-merge review of #129.
#[test]
fn review_multi_stream_rows_preserve_the_owner_to_task_mapping() {
    let mut view = Fixture::default();
    for (tab, stream) in [(1, "stream-z"), (2, "stream-a")] {
        view.live.push(Live {
            kind: "timer",
            key: format!("tab-{tab}:timer:1"),
            reference: Some(Reference {
                // Equal aliases in different streams are permitted, not global IDs.
                alias: "deadbeef".into(),
                identity: Identity {
                    stream: stream.into(),
                    kind: Kind::Timer,
                    original_id: OriginalId::Number(1),
                    created_by: format!("creation-{tab}"),
                },
            }),
            label: format!("tab {tab}: repeating reminder"),
            standing: true,
            fires: 0,
            since: 0,
            ledger: None,
            tools: 0,
            tokens: 0,
            read_len: 0,
        });
    }
    let render = |view: &Fixture| {
        background::lines(view, 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = render(&view);
    view.live[0].reference.as_mut().unwrap().identity.stream = "stream-a".into();
    view.live[1].reference.as_mut().unwrap().identity.stream = "stream-z".into();
    let after = render(&view);
    assert_ne!(
        before, after,
        "swapping each task's owning stream is invisible in the background panel:\n{before}"
    );
}

#[test]
fn background_aliases_show_kind_owner_and_whole_extended_ids_at_narrow_widths() {
    let mut registries = [
        Registry::new("owner-stream"),
        Registry::new("second-stream"),
    ];
    let mut view = Fixture::default();
    for (index, (kind, name)) in [
        (Kind::Expert, "expert"),
        (Kind::Run, "command"),
        (Kind::Timer, "timer"),
        (Kind::Watch, "watch"),
    ]
    .into_iter()
    .enumerate()
    {
        let identity = Identity {
            stream: ["owner-stream", "second-stream"][index % 2].into(),
            kind,
            original_id: OriginalId::Number(1),
            created_by: format!("creation-{index}"),
        };
        let alias = registries[index % 2].assign(identity.clone()).unwrap();
        view.live.push(Live {
            kind: name,
            key: format!("internal-{name}:1"),
            reference: Some(Reference { alias, identity }),
            label: "Recognizable work description".into(),
            standing: index >= 2,
            fires: 0,
            since: 0,
            ledger: None,
            tools: 0,
            tokens: 0,
            read_len: 0,
        });
    }
    // Presentation must also support the longest possible collision extension.
    view.live[3].reference.as_mut().unwrap().alias = "a".repeat(64);
    let aliases: Vec<_> = view
        .live
        .iter()
        .map(|row| row.reference.as_ref().unwrap().alias.clone())
        .collect();
    for width in [24, 40, 80] {
        let lines = background::lines(&view, width);
        println!(
            "\nBackground layout at {width} columns:\n{}",
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let data = &lines[3..];
        assert!(data
            .iter()
            .all(|line| lattice::wrap::str_cols(&line.to_string()) <= width));
        let primary: String = data
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.fg == Some(crate::terminal_host::theme::ACCENT))
            .map(|span| span.content.as_ref())
            .collect();
        for alias in &aliases {
            assert!(
                primary.contains(alias),
                "the complete alias must survive wrapping"
            );
        }
        assert!(!primary.contains("internal-"));
        let text = data
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for kind in ["Expert", "Run", "Timer", "Watch"] {
            assert!(text.contains(kind));
        }
        assert!(text.contains("running"));
        assert!(text.contains("armed"));
        assert!(text.contains("owner-stream"));
        assert!(text.contains("second-stream"));
        for live in &view.live {
            let reference = live.reference.as_ref().unwrap();
            let rows = background::rows(&view, width);
            let (_, note) = rows
                .iter()
                .find(|(alias, _)| alias == &reference.alias)
                .unwrap();
            assert!(note.contains(&live.label));
            assert!(note.contains(&format!("stream: {}", reference.identity.stream)));
        }
    }
    view.live.reverse();
    let keys: Vec<_> = background::rows(&view, 80)
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| !key.is_empty())
        .collect();
    for alias in aliases {
        assert!(keys.contains(&alias));
    }
}
