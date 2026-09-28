//! The system prompt, as a list of ordered sections rather than one string.
//!
//! It used to be two Rust string literals — eight kilobytes of prose inside
//! `preset.rs`, where changing one sentence showed up in a diff as a rewritten
//! paragraph and nothing could be replaced piece by piece. They are files now,
//! compiled in with `include_str!`, the same way the JSON contracts already
//! were.
//!
//! ONE ORDERING, by file name. The components' own fragments — what a tool
//! says about itself, what the environment probe found, the project's rules —
//! are a section too, and they sit at [`SETUP`]. So a section's number says
//! exactly where it lands, including relative to those, and a person adding
//! one only has to pick a number.
//!
//! Numbered in hundreds so there is always room between any two.
//!
//! WHAT THE ORDER IS FOR is the prompt cache. The whole thing is the cached
//! prefix of every call, so a change anywhere throws away the cache from that
//! point on. Steady things go first, and the sections that move — the setup,
//! the project's rules — go last.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the components' fragments land in the ordering. A section numbered
/// below this comes before them; above, after.
pub const SETUP: u32 = 500;

/// The sections this build ships with.
///
/// Whatever is here is what Eva is, in the absence of a user who says
/// otherwise — see [`load`] for how they say it.
const BUILT_IN: &[(&str, &str)] = &[
    (
        "100-identity.md",
        include_str!("../prompts/100-identity.md"),
    ),
    (
        "200-situation.md",
        include_str!("../prompts/200-situation.md"),
    ),
    ("300-rigour.md", include_str!("../prompts/300-rigour.md")),
    (
        "400-collaboration.md",
        include_str!("../prompts/400-collaboration.md"),
    ),
    (
        "450-reporting.md",
        include_str!("../prompts/450-reporting.md"),
    ),
    (
        "600-house-rules.md",
        include_str!("../prompts/600-house-rules.md"),
    ),
];

/// One section of the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The file it came from — also its sort key and its identity, which is
    /// what makes a user file of the same name a REPLACEMENT rather than an
    /// addition.
    pub name: String,
    /// The number the name starts with; where it lands relative to [`SETUP`].
    pub at: u32,
    pub text: String,
}

/// Where a person's own sections go: `~/.lattice/prompts`.
pub fn user_dir(home: &Path) -> PathBuf {
    home.join(".lattice").join("prompts")
}

/// Every section, in order: what this build ships, with the user's directory
/// laid over it.
///
/// Three things a user file can do, and they are the same three the assembly
/// overlay already offers, so there is one idea to learn rather than two:
///
/// - a file with the same name REPLACES that section;
/// - a file with a new name is INSERTED, at wherever its number puts it;
/// - a file that is empty SUPPRESSES the section it names — deleting is
///   saying "not this", which needs a way to be said.
pub fn load(home: Option<&Path>) -> Vec<Section> {
    let mut by_name: BTreeMap<String, String> = BUILT_IN
        .iter()
        .map(|(name, text)| (name.to_string(), text.to_string()))
        .collect();

    if let Some(home) = home {
        if let Ok(entries) = std::fs::read_dir(user_dir(home)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".md") {
                    continue;
                }
                match std::fs::read_to_string(entry.path()) {
                    Ok(text) => {
                        by_name.insert(name, text);
                    }
                    // Unreadable is not the same as absent, and the difference
                    // is not worth a startup failure: the shipped section
                    // stands and the person can see the file is unreadable.
                    Err(_) => continue,
                }
            }
        }
    }

    by_name
        .into_iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(name, text)| Section {
            at: leading_number(&name),
            name,
            text: text.trim().to_string(),
        })
        .collect()
}

/// The sections before the components' fragments, joined — what the model
/// reads first.
pub fn before_setup(sections: &[Section]) -> String {
    join(sections.iter().filter(|s| s.at < SETUP))
}

/// The sections after them.
pub fn after_setup(sections: &[Section]) -> String {
    join(sections.iter().filter(|s| s.at >= SETUP))
}

fn join<'a>(sections: impl Iterator<Item = &'a Section>) -> String {
    sections
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The number a name starts with. A name that starts with none sorts by name
/// among the others and lands after the setup — a section nobody placed is
/// more likely to be an addition than a replacement of the opening.
fn leading_number(name: &str) -> u32 {
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn what_ships_is_ordered_and_lands_on_both_sides_of_the_setup() {
        let sections = load(None);
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "100-identity.md",
                "200-situation.md",
                "300-rigour.md",
                "400-collaboration.md",
                "450-reporting.md",
                "600-house-rules.md",
            ]
        );
        assert!(
            before_setup(&sections).starts_with("# Who you are"),
            "the opening is who Eva is"
        );
        assert!(
            after_setup(&sections).starts_with("# House rules"),
            "and the rules come after the setup, which they refer to"
        );
        assert!(!before_setup(&sections).contains("# House rules"));
    }

    /// The same three things the assembly overlay offers, so there is one idea
    /// to learn rather than two.
    #[test]
    fn a_user_file_replaces_inserts_or_suppresses() {
        let home = tempfile::tempdir().unwrap();
        let dir = user_dir(home.path());
        write(&dir, "100-identity.md", "You are somebody else entirely.");
        write(&dir, "150-mine.md", "And this is mine.");
        write(&dir, "300-rigour.md", "   \n");

        let sections = load(Some(home.path()));
        let names: Vec<&str> = sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "100-identity.md",
                "150-mine.md",
                "200-situation.md",
                "400-collaboration.md",
                "450-reporting.md",
                "600-house-rules.md",
            ],
            "replaced, inserted in its numbered place, and one suppressed"
        );
        let opening = before_setup(&sections);
        assert!(opening.starts_with("You are somebody else entirely."));
        assert!(
            opening.contains("And this is mine."),
            "the inserted one is where its number puts it: {opening}"
        );
        assert!(
            !opening.contains("KNOW WHAT YOU DO NOT KNOW"),
            "and the suppressed one is gone"
        );
    }

    #[test]
    fn a_section_nobody_numbered_lands_after_the_setup() {
        let home = tempfile::tempdir().unwrap();
        write(&user_dir(home.path()), "notes.md", "Something I added.");
        let sections = load(Some(home.path()));
        assert!(after_setup(&sections).contains("Something I added."));
        assert!(!before_setup(&sections).contains("Something I added."));
    }
}
