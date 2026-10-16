use super::*;
use lattice::background_aliases::{Identity, Kind, OriginalId, Reference, Registry};

#[test]
fn background_aliases_show_kind_owner_and_whole_extended_ids_at_narrow_widths() {
    let mut registry = Registry::new("owner-stream");
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
            stream: "owner-stream".into(),
            kind,
            original_id: OriginalId::Number(1),
            created_by: format!("creation-{index}"),
        };
        let alias = registry.assign(identity.clone()).unwrap();
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
        assert!(text.contains("owner-stream"));
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
