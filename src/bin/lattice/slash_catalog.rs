//! Frontend command descriptions shared by completion and the help panel.
//! Execution belongs to the interaction coordinator, never the runtime core.

pub(super) struct Slash {
    pub(super) name: &'static str,
    pub(super) summary: &'static str,
}

pub(super) const SLASH: &[Slash] = &[
    Slash {
        name: "/help",
        summary: "list the commands",
    },
    Slash {
        name: "/clear",
        summary: "clear the screen (the log keeps everything)",
    },
    Slash {
        name: "/effort",
        summary: "how hard the model thinks (/effort off|low|medium|high|max)",
    },
    Slash {
        name: "/context",
        summary: "how full the context window is",
    },
    Slash {
        name: "/compact",
        summary: "request one compaction attempt, including when automatic compaction is paused",
    },
    Slash {
        name: "/background",
        summary: "what is running or armed away from this conversation",
    },
    Slash {
        name: "/usage",
        summary: "how much has been spent, day by day",
    },
    Slash {
        name: "/config",
        summary: "what is in force, and which file said so",
    },
    Slash {
        name: "/components",
        summary: "what is assembled right now",
    },
    Slash {
        name: "/model",
        summary: "which model does the thinking (/model, or /model <name>)",
    },
    Slash {
        name: "/uninstall",
        summary: "take out a component that was installed (/uninstall <instance>)",
    },
    Slash {
        name: "/exit",
        summary: "quit lattice",
    },
    Slash {
        name: "/btw",
        summary: "open a side conversation (/btw [question])",
    },
    Slash {
        name: "/tab",
        summary: "switch conversation (/tab [number], Ctrl-PgUp/PgDn)",
    },
    Slash {
        name: "/back",
        summary: "return to this side conversation's parent",
    },
];

/// Computed from actual names: a fixed column was outgrown by /uninstall.
pub(super) fn slash_col() -> usize {
    SLASH
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2
}
