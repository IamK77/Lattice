//! The project's own rules reach the model on every call, because a rule that
//! must hold every turn cannot live in the model's recollection. What is being
//! pinned here is mostly about what happens when the ordinary case does NOT
//! hold: no rules file, a rules file one directory up, a rules file too big to
//! inline.

use lattice::components::project_rules;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_rules_file_arrives_whole() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        "# Rules\n\nComments are English. Run the formatter.\n",
    )
    .unwrap();

    let text = project_rules::fragment(dir.path(), &names(&["CLAUDE.md"]), 65_536)
        .expect("a project with rules gets a fragment");
    assert!(
        text.contains("Comments are English. Run the formatter."),
        "the rules themselves, not a reminder to read them: {text}"
    );
    assert!(
        text.contains("They bind you"),
        "and their standing is stated: {text}"
    );
}

#[test]
fn a_project_that_wrote_nothing_down_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "not a rules file").unwrap();
    assert!(
        project_rules::fragment(dir.path(), &names(project_rules::DEFAULT_FILES), 65_536).is_none(),
        "no rules, no fragment — an empty heading would be worse than silence"
    );
}

#[test]
fn an_empty_rules_file_is_the_same_as_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "   \n\n").unwrap();
    assert!(
        project_rules::fragment(dir.path(), &names(&["AGENTS.md"]), 65_536).is_none(),
        "whitespace is not a rule"
    );
}

/// Started in a subdirectory — which is where people actually start things.
#[test]
fn the_search_walks_up_to_the_project_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "the root rules").unwrap();
    let deep = dir.path().join("src/components");
    std::fs::create_dir_all(&deep).unwrap();

    let text = project_rules::fragment(&deep, &names(&["CLAUDE.md"]), 65_536)
        .expect("found by walking up");
    assert!(text.contains("the root rules"), "{text}");
}

/// The nearest one wins: a subdirectory that states its own rules is more
/// specific than the root's, and walking past it would be wrong.
#[test]
fn the_nearest_rules_file_wins() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "the root rules").unwrap();
    let deep = dir.path().join("subproject");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("CLAUDE.md"), "the nearer rules").unwrap();

    let text = project_rules::fragment(&deep, &names(&["CLAUDE.md"]), 65_536).unwrap();
    assert!(text.contains("the nearer rules"), "{text}");
    assert!(
        !text.contains("the root rules"),
        "and only that one: {text}"
    );
}

/// Both conventional names, when a project keeps both.
#[test]
fn both_names_are_read_when_both_exist() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "from claude").unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "from agents").unwrap();

    let text =
        project_rules::fragment(dir.path(), &names(project_rules::DEFAULT_FILES), 65_536).unwrap();
    assert!(
        text.contains("from claude") && text.contains("from agents"),
        "{text}"
    );
    // Each is labelled, so a rule can be traced back to the file it came from
    assert!(
        text.contains("--- CLAUDE.md ---") && text.contains("--- AGENTS.md ---"),
        "{text}"
    );
}

/// Too big to inline. Half a rules file in the prompt is worse than none —
/// the agent would follow the first half and never learn there was a second.
/// But silence would read as "this project has no rules", which is the one
/// wrong thing to say, so the file is named instead.
#[test]
fn a_rules_file_too_large_is_named_rather_than_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(2000);
    std::fs::write(dir.path().join("CLAUDE.md"), &big).unwrap();

    let text = project_rules::fragment(dir.path(), &names(&["CLAUDE.md"]), 1000).unwrap();
    assert!(
        !text.contains(&big),
        "not inlined, and not half-inlined either"
    );
    assert!(text.contains("CLAUDE.md"), "but named: {text}");
    assert!(text.contains("too large"), "and the reason given: {text}");
    assert!(text.contains("2000 bytes"), "with the size: {text}");
}

/// A rules file opens with `# Project`, and the prompt around it also uses
/// level-one headings. Left alone, the file's own heading becomes a sibling of
/// the prompt's, and everything printed afterwards reads as belonging to the
/// file's last section. So included headings are pushed down — except inside
/// fenced code, where a leading `#` is a shell comment and mangling it would
/// corrupt an instruction rather than nest it.
#[test]
fn included_headings_are_pushed_below_the_prompts_own() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        "# Project\n\n## Build\n\n```sh\n# not a heading\ncargo test\n```\n\n# Rules\n",
    )
    .unwrap();

    let text = project_rules::fragment(dir.path(), &names(&["CLAUDE.md"]), 65_536).unwrap();
    assert!(text.contains("\n### Project"), "demoted two levels: {text}");
    assert!(
        text.contains("\n#### Build"),
        "and so is the next level: {text}"
    );
    assert!(
        text.contains("\n### Rules"),
        "including one after a code block: {text}"
    );
    assert!(
        text.contains("\n# not a heading"),
        "a shell comment inside a fence is left exactly as written: {text}"
    );
    // And the file's own top-level headings are gone from the top level, so
    // they cannot become siblings of "# Who you are" and adopt what follows.
    assert!(!text.contains("\n# Project"), "no longer top level: {text}");
    assert!(!text.contains("\n# Rules"), "no longer top level: {text}");
}

/// The rules file is usually named after — and often written to — whichever
/// agent read it last. Following it is the point; answering to its name is not.
#[test]
fn the_rules_do_not_hand_over_a_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("CLAUDE.md"),
        "Claude has full authority over implementation.",
    )
    .unwrap();
    let text = project_rules::fragment(dir.path(), &names(&["CLAUDE.md"]), 65_536).unwrap();
    assert!(
        text.contains("the name is not yours to take"),
        "the rules bind, the name does not: {text}"
    );
}
