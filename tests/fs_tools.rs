//! File tools: real read/write/list confined to a root, and — the point of
//! declaring an effect surface — an effects-policy gate judging the write by
//! what it touches, not by its name.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{effects_policy, fs_tools, minimal_loop, scripted_model, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// A driver that injects tool requests; handles nothing itself.
struct Driver;
impl Component for Driver {
    fn handle(&mut self, _port: &str, _event: &EventEnvelope, _ctx: &mut Ctx) {}
}
fn driver_manifest() -> ComponentManifest {
    ComponentManifest {
        name: "driver".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Inproc,
        entry: "builtin:driver".to_string(),
        inputs: vec![],
        outputs: vec![PortDecl::new("out", &[ce::TOOL_EXEC_STARTED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    }
}

/// driver → fs-tools, rooted at `root`.
fn fs_kernel(root: &std::path::Path) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (fs_tools::NAME.to_string(), fs_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let root = root.to_path_buf();
    factories.insert(
        fs_tools::NAME.to_string(),
        Box::new(move |_| {
            Box::new(fs_tools::FsTools::from_config(Some(&json!({
                "root": root.to_string_lossy(),
            }))))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "fs".to_string(),
                ComponentInstance {
                    component: fs_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "fs.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

/// driver → fs-tools with NO root: the default, where paths are taken as given.
fn open_fs_kernel() -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (fs_tools::NAME.to_string(), fs_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        fs_tools::NAME.to_string(),
        // No config at all — this is what the preset now builds by default
        Box::new(move |_| Box::new(fs_tools::FsTools::from_config(None))),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "driver".to_string(),
                ComponentInstance {
                    component: "driver".to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "fs".to_string(),
                ComponentInstance {
                    component: fs_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "fs.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

fn call(kernel: &mut Kernel, id: &str, tool: &str, args: serde_json::Value) -> serde_json::Value {
    kernel.injector("driver").emit(
        "out",
        EventDraft::new(
            ce::TOOL_EXEC_STARTED,
            &[],
            json!({"call": id, "tool": tool, "arguments": args}),
        ),
    );
    kernel.run_until_quiescent().unwrap();
    kernel
        .log()
        .replay(1)
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == id)
        .expect("an outcome for this call")
        .payload
}

#[test]
fn write_then_read_round_trips_through_the_real_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fs_kernel(dir.path());

    // Write into a nested path (parents created), then read it back
    let wrote = call(
        &mut kernel,
        "w1",
        "Write",
        json!({"path": "notes/day1.md", "content": "hello lattice"}),
    );
    assert_eq!(wrote["status"], "ok");
    assert_eq!(wrote["result"]["bytes"], 13);
    // It really landed on disk
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/day1.md")).unwrap(),
        "hello lattice"
    );

    let read = call(&mut kernel, "r1", "Read", json!({"path": "notes/day1.md"}));
    assert_eq!(read["status"], "ok");
    assert_eq!(read["result"]["content"], "hello lattice");

    // The call-it-again probe: reading the same file again is unchanged
    let again = call(&mut kernel, "r2", "Read", json!({"path": "notes/day1.md"}));
    assert_eq!(again["result"]["content"], "hello lattice");

    let listed = call(&mut kernel, "l1", "Ls", json!({"path": "notes"}));
    assert_eq!(listed["status"], "ok");
    let entries = listed["result"]["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|e| e["name"] == "day1.md" && e["dir"] == false));

    kernel.shutdown();
}

#[test]
fn escapes_are_refused_before_touching_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    // A secret sitting OUTSIDE the root
    let outside = dir.path().parent().unwrap().join("secret.txt");
    std::fs::write(&outside, "top secret").unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(&root).unwrap();
    let mut kernel = fs_kernel(&root);

    for bad in ["../secret.txt", "../../etc/passwd", "/etc/passwd"] {
        let outcome = call(&mut kernel, bad, "Read", json!({"path": bad}));
        assert_eq!(outcome["status"], "error", "{bad} must be refused");
        assert_eq!(outcome["error"]["code"], "tool.path_refused", "{bad}");
    }
    // A write escape must not create anything outside either
    let outcome = call(
        &mut kernel,
        "we",
        "Write",
        json!({"path": "../escaped.txt", "content": "x"}),
    );
    assert_eq!(outcome["error"]["code"], "tool.path_refused");
    assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    // And the real secret was never read
    let _ = outside;

    kernel.shutdown();
}

#[test]
fn a_symlink_pointing_out_of_the_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "not yours").unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    // A symlink INSIDE the root that points OUT of it
    std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();

    let mut kernel = fs_kernel(&root);
    let outcome = call(&mut kernel, "s1", "Read", json!({"path": "link.txt"}));
    assert_eq!(outcome["status"], "error");
    assert_eq!(outcome["error"]["code"], "tool.path_refused");
    kernel.shutdown();
}

/// loop → policy → fs-tools: the policy judges by the fs tool's declared
/// surface. `net`/`writes` picks which fs tool the scripted model calls.
fn run_under_policy(tool: &str, args: serde_json::Value, allow_writes: bool) -> Vec<EventEnvelope> {
    let dir = tempfile::tempdir().unwrap();
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (effects_policy::NAME.to_string(), effects_policy::manifest()),
        (fs_tools::NAME.to_string(), fs_tools::manifest()),
    ]
    .into();
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "t1", "tool": tool, "arguments": args}]},
        {"status": "ok", "text": "done"},
    ]});
    let root = dir.path().to_path_buf();
    let mut f: HashMap<String, Factory> = HashMap::new();
    f.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
    );
    f.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    f.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    f.insert(
        effects_policy::NAME.to_string(),
        Box::new(|c| Box::new(effects_policy::EffectsPolicy::from_config(c))),
    );
    f.insert(
        fs_tools::NAME.to_string(),
        Box::new(move |_| {
            Box::new(fs_tools::FsTools::from_config(Some(&json!({
                "root": root.to_string_lossy(),
            }))))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "loop".to_string(),
                ComponentInstance {
                    component: minimal_loop::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: scripted_model::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(script),
                },
            ),
            (
                "policy".to_string(),
                ComponentInstance {
                    component: effects_policy::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"allowWrites": allow_writes})),
                },
            ),
            (
                "fs".to_string(),
                ComponentInstance {
                    component: fs_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            // The gate sits on the tool wire: loop → policy → fs
            Wire::new("loop.run", "policy.review"),
            Wire::new("policy.forward", "fs.execute"),
            Wire::new("policy.verdict", "loop.tools"),
            Wire::new("fs.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(&assembly, &registry, &mut f, KernelOptions::default()).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "do a file thing"})),
    );
    kernel.run_until_quiescent().unwrap();
    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    events
}

#[test]
fn the_policy_lets_a_read_through_but_denies_a_write() {
    // read declares only `reads` — allowed even with writes forbidden
    let events = run_under_policy("Read", json!({"path": "anything.txt"}), false);
    let forwarded = events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "policy");
    assert!(forwarded, "a read must pass the gate");

    // write declares `writes` — denied, with a reasoned decision, and
    // the loop still gets an answer (no change needed on its side)
    let events = run_under_policy("Write", json!({"path": "x.txt", "content": "y"}), false);
    let denied_forward = events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "policy");
    assert!(
        !denied_forward,
        "a write must not reach the tool when forbidden"
    );
    let decision = events
        .iter()
        .find(|e| e.source == "policy" && e.reason.as_deref().is_some_and(|r| !r.is_empty()))
        .expect("a denial is a reasoned decision");
    assert!(decision.reason.as_deref().unwrap().contains("writes"));
    // The tool never ran — nothing was written
    assert!(!events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "fs"));

    // With writes allowed, the same write goes through and really runs
    let events = run_under_policy("Write", json!({"path": "ok.txt", "content": "z"}), true);
    assert!(events
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_COMPLETED
            && e.source == "fs"
            && e.payload["status"] == "ok"));
}

/// A file too big to hand over whole used to come back as a flat refusal —
/// the agent got NOTHING. Now it gets a page and the line to continue from,
/// so an enormous file is still readable, just not all at once.
#[test]
fn a_long_file_reads_a_page_at_a_time_and_says_where_to_continue() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=500).map(|i| format!("line{i:03}\n")).collect();
    std::fs::write(dir.path().join("long.txt"), &body).unwrap();
    let mut kernel = fs_kernel(dir.path());

    let first = call(
        &mut kernel,
        "p1",
        "Read",
        json!({"path": "long.txt", "limit": 200}),
    );
    assert_eq!(first["result"]["from"], 1);
    assert_eq!(first["result"]["to"], 200);
    assert_eq!(first["result"]["more"], true);
    assert_eq!(first["result"]["next"], 201);
    let page = first["result"]["content"].as_str().unwrap();
    assert!(page.starts_with("line001\n"));
    assert!(page.ends_with("line200\n"));

    // Continue exactly where it said to, and reach the end
    let second = call(
        &mut kernel,
        "p2",
        "Read",
        json!({"path": "long.txt", "from": 201, "limit": 400}),
    );
    assert_eq!(second["result"]["to"], 500);
    assert_eq!(second["result"]["more"], false);
    assert!(second["result"].get("next").is_none());
    assert!(second["result"]["content"]
        .as_str()
        .unwrap()
        .ends_with("line500\n"));
}

/// The edit tool: an exact replacement, and a refusal to guess. A pattern
/// matching twice means the caller does not yet know which one it meant —
/// editing the wrong one is damage nothing downstream would notice.
#[test]
fn edit_replaces_exactly_once_and_refuses_to_guess() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("c.rs"), "let a = 1;\nlet b = 1;\n").unwrap();
    let mut kernel = fs_kernel(dir.path());

    // Ambiguous: "= 1;" is in both lines — refused, and nothing changes
    let refused = call(
        &mut kernel,
        "e1",
        "Edit",
        json!({"path": "c.rs", "old": "= 1;", "new": "= 2;"}),
    );
    assert_eq!(refused["status"], "error");
    assert_eq!(refused["error"]["code"], "tool.ambiguous");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("c.rs")).unwrap(),
        "let a = 1;\nlet b = 1;\n",
        "a refused edit must leave the file untouched"
    );

    // Unambiguous with more context around it
    let done = call(
        &mut kernel,
        "e2",
        "Edit",
        json!({"path": "c.rs", "old": "let b = 1;", "new": "let b = 2;"}),
    );
    assert_eq!(done["status"], "ok");
    assert_eq!(done["result"]["replaced"], 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("c.rs")).unwrap(),
        "let a = 1;\nlet b = 2;\n"
    );

    // Deliberate mass replace
    let all = call(
        &mut kernel,
        "e3",
        "Edit",
        json!({"path": "c.rs", "old": "let", "new": "const", "all": true}),
    );
    assert_eq!(all["result"]["replaced"], 2);

    let missing = call(
        &mut kernel,
        "e4",
        "Edit",
        json!({"path": "c.rs", "old": "nowhere", "new": "x"}),
    );
    assert_eq!(missing["error"]["code"], "tool.no_match");
}

#[test]
fn edit_context_is_captured_once_and_survives_later_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("context.txt");
    let original: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    let original = original
        .replace("line 8", "target")
        .replace("line 30", "target");
    std::fs::write(&path, &original).unwrap();
    let mut kernel = fs_kernel(dir.path());
    let done = call(
        &mut kernel,
        "context",
        "Edit",
        json!({
            "path":"context.txt", "old":"target", "new":"中文\nextra", "all":true
        }),
    );
    assert_eq!(done["status"], "ok");
    let snapshot: lattice::edit_diff::EditDiff =
        serde_json::from_value(done["result"]["editDiff"].clone()).unwrap();
    assert_eq!(snapshot.hunks.len(), 2);
    assert_eq!(
        (snapshot.hunks[0].old_start, snapshot.hunks[0].new_start),
        (5, 5)
    );
    assert_eq!(
        (snapshot.hunks[1].old_start, snapshot.hunks[1].new_start),
        (27, 28)
    );
    assert_eq!(snapshot.hunks[0].lines.first().unwrap().text, "line 5");
    assert_eq!(snapshot.hunks[0].lines.last().unwrap().text, "line 11");
    std::fs::write(&path, "completely different now").unwrap();
    let mut entries = Vec::new();
    for event in kernel.log().replay(1).unwrap() {
        lattice::ingest(&mut entries, &event);
    }
    let card = entries
        .iter()
        .find_map(|entry| match entry {
            lattice::Entry::Tool(card) => Some(card),
            _ => None,
        })
        .unwrap();
    assert_eq!(card.edit_diff.as_ref(), Some(&snapshot));
    let encoded = serde_json::to_value(card).unwrap();
    let restored: lattice::ToolCard = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(restored.edit_diff, Some(snapshot));
    let mut legacy = encoded;
    legacy.as_object_mut().unwrap().remove("edit_diff");
    assert!(serde_json::from_value::<lattice::ToolCard>(legacy)
        .unwrap()
        .edit_diff
        .is_none());
    let failed = call(
        &mut kernel,
        "missing",
        "Edit",
        json!({"path":"context.txt", "old":"absent", "new":"x"}),
    );
    assert!(failed["result"]["editDiff"].is_null());
}

/// What a write DESTROYED is as much a fact as what it wrote — afterwards
/// nothing on the record can tell "created" from "silently overwrote".
#[test]
fn write_reports_whether_it_created_or_overwrote() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fs_kernel(dir.path());

    let created = call(
        &mut kernel,
        "w1",
        "Write",
        json!({"path": "f.txt", "content": "first"}),
    );
    assert_eq!(created["result"]["created"], true);
    assert!(created["result"]["replacedBytes"].is_null());

    let replaced = call(
        &mut kernel,
        "w2",
        "Write",
        json!({"path": "f.txt", "content": "second"}),
    );
    assert_eq!(replaced["result"]["created"], false);
    assert_eq!(replaced["result"]["replacedBytes"], 5);
}

// ── Unconfined: the default ────────────────────────────────────────────────

#[test]
fn without_a_root_absolute_paths_and_dotdot_both_work() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(dir.path().join("outside.txt"), "reachable\n").unwrap();
    let mut kernel = open_fs_kernel();

    // An absolute path anywhere on the machine
    let by_abs = call(
        &mut kernel,
        "o1",
        "Read",
        json!({"path": dir.path().join("outside.txt").to_string_lossy()}),
    );
    assert_eq!(
        by_abs["status"], "ok",
        "absolute paths are allowed: {by_abs}"
    );
    assert_eq!(by_abs["result"]["content"], "reachable\n");

    // And climbing with .., which a rooted instance refuses outright
    let by_dotdot = call(
        &mut kernel,
        "o2",
        "Read",
        json!({"path": deep.join("../../outside.txt").to_string_lossy()}),
    );
    assert_eq!(by_dotdot["status"], "ok", ".. is allowed: {by_dotdot}");

    // Writing, too — the point of the change is that it is not a read-only door
    let target = dir.path().join("fresh/made.txt");
    let written = call(
        &mut kernel,
        "o3",
        "Write",
        json!({"path": target.to_string_lossy(), "content": "hi"}),
    );
    assert_eq!(written["status"], "ok", "{written}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");
}

#[test]
fn the_declared_surface_widens_with_the_reach() {
    // The whole point: a gate must not be judging a narrower surface than the
    // tool actually has. Unconfined says so.
    for decl in fs_tools::tool_decls(None) {
        let effects = &decl["effects"];
        let named = if effects["writes"].is_array() {
            &effects["writes"]
        } else {
            &effects["reads"]
        };
        assert_eq!(named[0], "*", "{} declares {named}", decl["name"]);
    }
    // Configured, it says the root instead — narrower, and true.
    for decl in fs_tools::tool_decls(Some("/srv/box")) {
        let effects = &decl["effects"];
        let named = if effects["writes"].is_array() {
            &effects["writes"]
        } else {
            &effects["reads"]
        };
        assert_eq!(named[0], "/srv/box", "{} declares {named}", decl["name"]);
    }
    // And the manifest — what the kernel actually offers the model — carries
    // the default, which is the open one.
    let manifest = fs_tools::manifest();
    assert_eq!(manifest.capabilities.as_ref().unwrap().writes, vec!["*"]);
    for decl in &manifest.tools {
        let shown = decl["effects"].to_string();
        assert!(
            !shown.contains("<root>"),
            "no placeholder reaches the model: {shown}"
        );
    }
}

/// ONE line, longer than a whole page. The byte ceiling only ever looked at
/// the second line onward, so a file whose first line was enormous came back
/// whole — and a ledger is exactly that file: one JSON event per line, and an
/// event carrying a command's output is tens of kilobytes on a single line.
/// Reading it to learn what happened would have spent the context on it.
///
/// The other half is that the rest must still be reachable. Cutting without a
/// way to continue would put the tail of a long line permanently out of
/// reach, which is worse than the ceiling not firing.
#[test]
fn a_single_line_longer_than_a_page_is_cut_and_can_be_continued() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = open_fs_kernel();
    let path = dir.path().join("one-huge-line.jsonl");

    // Multi-byte characters throughout: a cut that ignored char boundaries
    // would hand back something that is not text.
    let huge = format!("{{\"payload\":\"{}\"}}\n", "字".repeat(20_000));
    assert!(huge.len() > 50_000, "the fixture is one very long line");
    let written = call(
        &mut kernel,
        "w",
        "Write",
        json!({"path": path.to_string_lossy(), "content": huge}),
    );
    assert_eq!(written["status"], "ok");

    let first = call(
        &mut kernel,
        "r",
        "Read",
        json!({"path": path.to_string_lossy()}),
    );
    assert_eq!(first["status"], "ok");
    let page = first["result"]["content"].as_str().unwrap();
    assert!(
        page.len() < 32_768,
        "one line must not become a whole context: {} bytes",
        page.len()
    );
    assert_eq!(first["result"]["more"], true);
    assert_eq!(
        first["result"]["cut"]["line"], 1,
        "it says WHICH line was cut: {first}"
    );
    assert_eq!(
        first["result"]["cut"]["bytes"],
        huge.len(),
        "and how big that line really is"
    );

    // Continue the same line where it stopped
    let next_byte = first["result"]["nextByte"].as_u64().unwrap();
    let second = call(
        &mut kernel,
        "r2",
        "Read",
        json!({"path": path.to_string_lossy(), "from": 1, "fromByte": next_byte}),
    );
    assert_eq!(second["status"], "ok");
    let more = second["result"]["content"].as_str().unwrap();
    assert!(!more.is_empty(), "the rest of the line is reachable");
    assert!(
        huge[next_byte as usize..].starts_with(more),
        "and it continues exactly where the first page stopped"
    );
    kernel.shutdown();
}

/// How large a file may be WRITTEN and how much of one comes back from a READ
/// were one number. That coupled two unrelated things: lowering it to keep a
/// read from spending a large part of the context window would have started
/// refusing legitimate writes. This pins them apart — a file too big to
/// return in one page is still perfectly writable.
#[test]
fn the_read_page_and_the_write_ceiling_are_separate_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = open_fs_kernel();
    let path = dir.path().join("big.txt");

    // FEW lines, each LONG: far under the 2000-line default page, far over the
    // byte ceiling. A fixture of many short lines cannot tell these apart —
    // the line limit stops the read first and the byte ceiling never fires.
    let content: String = (1..=300)
        .map(|n| format!("line {n} {}\n", "x".repeat(300)))
        .collect();
    assert!(content.len() > 90_000, "the fixture must exceed a page");
    let written = call(
        &mut kernel,
        "w",
        "Write",
        json!({"path": path.to_string_lossy(), "content": content}),
    );
    assert_eq!(
        written["status"], "ok",
        "a file this size must still be writable: {written}"
    );

    let read = call(
        &mut kernel,
        "r",
        "Read",
        json!({"path": path.to_string_lossy()}),
    );
    assert_eq!(read["status"], "ok");
    let page = read["result"]["content"].as_str().unwrap();
    assert!(
        page.len() < content.len(),
        "the read stopped short of the whole file"
    );
    assert!(
        page.len() < 32_768,
        "and stayed inside a page: {} bytes",
        page.len()
    );
    // The byte ceiling stopped it, not the line limit: 300 lines is nowhere
    // near the 2000-line default, so a page shorter than that is only
    // explicable by the budget
    assert!(
        page.lines().count() < 300,
        "the byte ceiling did the stopping, not the line limit: {} lines",
        page.lines().count()
    );
    assert_eq!(
        read["result"]["more"], true,
        "saying plainly that there is more"
    );
    assert!(page.starts_with("line 1 xxx"));
    kernel.shutdown();
}
