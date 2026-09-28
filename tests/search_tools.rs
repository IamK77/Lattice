//! Search tools: finding files by name and by content WITHOUT the price of a
//! shell. The point of these tools is the declaration they carry — `reads` and
//! nothing else — so the tests below check both halves: that they really find
//! things, and that nothing about them can reach outside the root.
#![cfg(unix)]

use std::collections::HashMap;

use serde_json::json;

use std::sync::Arc;

use lattice::components::{effects_policy, minimal_loop, scripted_model, search_tools, silent_ui};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

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

/// Unconfined search — no root config, which is now the default.
fn open_search_kernel() -> Kernel {
    search_kernel_with(json!(null))
}

/// The same, with a configured search instance.
fn search_kernel_with(config: serde_json::Value) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (search_tools::NAME.to_string(), search_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    factories.insert(
        search_tools::NAME.to_string(),
        Box::new(move |c| Box::new(search_tools::SearchTools::from_config(c))),
    );
    let instance_config = (!config.is_null()).then(|| config.clone());
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            (
                "search".to_string(),
                ComponentInstance::new(search_tools::NAME, instance_config),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "search.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

fn search_kernel(root: &std::path::Path) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (search_tools::NAME.to_string(), search_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    let root = root.to_path_buf();
    factories.insert(
        search_tools::NAME.to_string(),
        Box::new(move |_| {
            Box::new(search_tools::SearchTools::from_config(Some(&json!({
                "root": root.to_string_lossy(),
            }))))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            (
                "search".to_string(),
                ComponentInstance::new(search_tools::NAME, None),
            ),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "search.execute")],
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

/// A small tree with something to find in it.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    greet();\n}\n").unwrap();
    std::fs::write(
        root.join("src/deep/util.rs"),
        "pub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("docs/notes.md"), "greet is in util.rs\n").unwrap();
    dir
}

#[test]
fn find_matches_paths_as_the_caller_sees_them() {
    let dir = fixture();
    let mut kernel = search_kernel(dir.path());

    let rust = call(&mut kernel, "f1", "Find", json!({"glob": "**/*.rs"}));
    let paths: Vec<&str> = rust["result"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    // Root-relative, so nothing leaks about where the workspace really sits
    assert_eq!(paths, vec!["src/deep/util.rs", "src/main.rs"]);

    // A pattern anchored at a subdirectory means what it looks like it means
    let deep = call(&mut kernel, "f2", "Find", json!({"glob": "src/deep/*.rs"}));
    assert_eq!(deep["result"]["paths"], json!(["src/deep/util.rs"]));

    let none = call(&mut kernel, "f3", "Find", json!({"glob": "**/*.py"}));
    assert_eq!(none["result"]["paths"], json!([]));
    kernel.shutdown();
}

#[test]
fn grep_returns_path_line_and_text() {
    let dir = fixture();
    let mut kernel = search_kernel(dir.path());

    let hits = call(
        &mut kernel,
        "g1",
        "Grep",
        json!({"pattern": "fn greet", "glob": "**/*.rs"}),
    );
    let matches = hits["result"]["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "src/deep/util.rs");
    assert_eq!(matches[0]["line"], 1);
    assert!(matches[0]["text"]
        .as_str()
        .unwrap()
        .contains("pub fn greet"));

    // The glob really narrows: the same word lives in the markdown too
    let everywhere = call(&mut kernel, "g2", "Grep", json!({"pattern": "greet"}));
    let paths: Vec<&str> = everywhere["result"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"docs/notes.md"), "{paths:?}");
    assert!(paths.contains(&"src/main.rs"), "{paths:?}");
    kernel.shutdown();
}

/// A search that could be pointed anywhere would not be a read of "the root",
/// and the root is what the declaration promises.
#[test]
fn neither_tool_can_be_pointed_outside_the_root() {
    let dir = fixture();
    let mut kernel = search_kernel(dir.path());

    for (id, tool, args) in [
        ("e1", "Find", json!({"glob": "*", "path": "../.."})),
        ("e2", "Grep", json!({"pattern": "x", "path": "../.."})),
        ("e3", "Find", json!({"glob": "*", "path": "/etc"})),
    ] {
        let refused = call(&mut kernel, id, tool, args);
        assert_eq!(refused["status"], "error", "{id}");
        assert_eq!(refused["error"]["code"], "tool.path_refused", "{id}");
    }
    kernel.shutdown();
}

/// A symlink pointing out of the root must not carry the search out with it —
/// no argument check can undo that once it has happened, so the walk simply
/// does not follow links.
#[test]
fn a_symlink_out_of_the_root_is_not_followed() {
    let dir = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.rs"), "fn greet() {}\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

    let mut kernel = search_kernel(dir.path());
    let found = call(&mut kernel, "s1", "Find", json!({"glob": "**/*.rs"}));
    let paths = found["result"]["paths"].to_string();
    assert!(
        !paths.contains("secret.rs"),
        "the walk must not follow a link out of the root: {paths}"
    );
    kernel.shutdown();
}

#[test]
fn results_are_capped_and_say_so() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..50 {
        std::fs::write(dir.path().join(format!("f{i:02}.txt")), "needle\n").unwrap();
    }
    let mut kernel = search_kernel(dir.path());

    let capped = call(
        &mut kernel,
        "c1",
        "Find",
        json!({"glob": "*.txt", "limit": 10}),
    );
    assert_eq!(capped["result"]["paths"].as_array().unwrap().len(), 10);
    assert_eq!(capped["result"]["more"], true);

    let hits = call(
        &mut kernel,
        "c2",
        "Grep",
        json!({"pattern": "needle", "limit": 5}),
    );
    assert_eq!(hits["result"]["matches"].as_array().unwrap().len(), 5);
    assert_eq!(hits["result"]["more"], true);
    kernel.shutdown();
}

#[test]
fn a_bad_pattern_is_reported_not_a_crash() {
    let dir = fixture();
    let mut kernel = search_kernel(dir.path());
    let bad = call(&mut kernel, "b1", "Grep", json!({"pattern": "[unclosed"}));
    assert_eq!(bad["status"], "error");
    assert_eq!(bad["error"]["code"], "tool.bad_pattern");
    kernel.shutdown();
}

/// The whole reason these tools exist: a policy that forbids running programs
/// and writing files still lets them through, because all they declare is
/// reading. `run("grep …")` would be denied by the very same policy.
///
/// Driven through a REAL loop, because that is what puts the declaration where
/// the gate looks for it: the kernel collects each provider's declared tools,
/// the loop offers them on the model call, and the gate reads the surface back
/// off the ledger. A hand-made event could not prove that chain holds.
#[test]
fn a_read_only_policy_lets_search_through() {
    let dir = fixture();
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (effects_policy::NAME.to_string(), effects_policy::manifest()),
        (search_tools::NAME.to_string(), search_tools::manifest()),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        effects_policy::NAME.to_string(),
        Box::new(|c| Box::new(effects_policy::EffectsPolicy::from_config(c))),
    );
    let root = dir.path().to_path_buf();
    factories.insert(
        search_tools::NAME.to_string(),
        Box::new(move |_| {
            Box::new(search_tools::SearchTools::from_config(Some(&json!({
                "root": root.to_string_lossy(),
            }))))
        }),
    );

    let script = json!({"script": [
        {"status": "ok", "toolCalls": [
            {"id": "c1", "tool": "Find", "arguments": {"glob": "**/*.rs"}}]},
        {"status": "ok", "text": "found them"},
    ]});
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance::new(silent_ui::NAME, None),
            ),
            (
                "loop".to_string(),
                ComponentInstance::new(minimal_loop::NAME, None),
            ),
            (
                "model".to_string(),
                ComponentInstance::new(scripted_model::NAME, Some(script)),
            ),
            (
                // Everything dangerous forbidden: no execution, no writes, no
                // network. Only reading is left.
                "policy".to_string(),
                ComponentInstance::new(effects_policy::NAME, Some(json!({}))),
            ),
            (
                "search".to_string(),
                ComponentInstance::new(search_tools::NAME, None),
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "policy.review"),
            Wire::new("policy.forward", "search.execute"),
            Wire::new("policy.verdict", "loop.tools"),
            Wire::new("search.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(
            ce::USER_MESSAGE,
            &[],
            json!({"text": "find the rust files"}),
        ),
    );
    kernel.run_until_quiescent().unwrap();

    let events = kernel.log().replay(1).unwrap();
    kernel.shutdown();
    let outcome = events
        .iter()
        .rev()
        .find(|e| e.event_type == ce::TOOL_EXEC_COMPLETED && e.payload["call"] == "c1")
        .expect("the search answered");
    assert_eq!(
        outcome.payload["status"], "ok",
        "a read-only policy must let a read-only tool through: {}",
        outcome.payload
    );
    assert_eq!(
        outcome.payload["result"]["paths"].as_array().unwrap().len(),
        2
    );
}

// ── Unconfined: the default ────────────────────────────────────────────────

#[test]
fn without_a_root_a_search_starts_where_it_is_pointed() {
    let dir = fixture();
    let mut kernel = open_search_kernel();

    // An absolute start, and a glob written the way it looks: anchored on
    // where the search STARTS, not on where that directory sits on this
    // machine. The alternative would make the pattern depend on the tempdir.
    let found = call(
        &mut kernel,
        "o1",
        "Find",
        json!({"glob": "src/**/*.rs", "path": dir.path().to_string_lossy()}),
    );
    let paths: Vec<String> = found["result"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    assert!(!paths.is_empty(), "the glob matched nothing: {found}");

    // What comes back is usable as-is: absolute in, absolute out, and every
    // one of them a file that really exists at that path.
    for path in &paths {
        assert!(
            std::path::Path::new(path).is_file(),
            "a returned path must be one you can hand straight to `read`: {path}"
        );
        assert!(path.starts_with(dir.path().to_str().unwrap()), "{path}");
    }

    // grep the same way
    let hits = call(
        &mut kernel,
        "o2",
        "Grep",
        json!({"pattern": "fn greet", "path": dir.path().to_string_lossy()}),
    );
    let first = hits["result"]["matches"][0]["path"].as_str().unwrap();
    assert!(
        std::path::Path::new(first).is_file(),
        "grep's paths are usable too: {first}"
    );
    kernel.shutdown();
}

#[test]
fn without_a_root_nothing_is_refused_for_leaving_it() {
    let dir = fixture();
    let mut kernel = open_search_kernel();
    // Both of these a rooted instance refuses outright — an absolute start,
    // and a path with `..` in it. Kept inside the fixture so the walk stays
    // small; what is being tested is the refusal, not the reach.
    let absolute = dir.path().join("src").to_string_lossy().into_owned();
    let dotdot = dir.path().join("src/..").to_string_lossy().into_owned();
    for (id, path) in [("r1", absolute), ("r2", dotdot)] {
        let out = call(
            &mut kernel,
            id,
            "Find",
            json!({"glob": "**/*.rs", "path": path}),
        );
        assert_eq!(out["status"], "ok", "{path} was refused: {out}");
        assert!(
            !out["result"]["paths"].as_array().unwrap().is_empty(),
            "and it really searched: {out}"
        );
    }
    kernel.shutdown();
}

#[test]
fn the_declared_surface_widens_with_the_reach() {
    for decl in search_tools::tool_decls(None) {
        assert_eq!(decl["effects"]["reads"][0], "*", "{}", decl["name"]);
    }
    for decl in search_tools::tool_decls(Some("/srv/box")) {
        assert_eq!(decl["effects"]["reads"][0], "/srv/box", "{}", decl["name"]);
    }
    let manifest = search_tools::manifest();
    assert_eq!(manifest.capabilities.as_ref().unwrap().reads, vec!["*"]);
    // and nothing this component ever declares lets it write or execute
    let surface = manifest.capabilities.unwrap();
    assert!(surface.writes.is_empty() && !surface.executes);
}

/// Asking to see dotfiles must not also switch off every .gitignore.
///
/// One flag used to answer both questions, and the cheap-sounding half
/// carried the expensive one. A real session asked for `hidden: true` over a
/// home directory: 530k entries became 2.2M — measured — the search ran past
/// its deadline, and because nothing here looked at the cancellation token
/// the whole search component was declared unresponsive, taking `Find` and
/// `Grep` away for the rest of that conversation.
#[test]
fn showing_dotfiles_does_not_also_stop_honouring_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("plain.txt"), "needle").unwrap();
    std::fs::write(dir.path().join(".dotfile.txt"), "needle").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "needle").unwrap();
    // A walker only honours .gitignore inside a repository.
    std::fs::create_dir(dir.path().join(".git")).unwrap();

    let mut kernel = open_search_kernel();
    let names = |found: &serde_json::Value| -> Vec<String> {
        found["result"]["matches"]
            .as_array()
            .or_else(|| found["result"]["paths"].as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|m| m["path"].as_str().or_else(|| m.as_str()))
                    .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let at = dir.path().display().to_string();

    let plain = call(
        &mut kernel,
        "g1",
        "Grep",
        json!({"pattern": "needle", "path": at}),
    );
    assert_eq!(
        names(&plain),
        vec!["plain.txt"],
        "by default, neither: {plain}"
    );

    let dots = call(
        &mut kernel,
        "g2",
        "Grep",
        json!({"pattern": "needle", "path": at, "hidden": true}),
    );
    let seen = names(&dots);
    assert!(
        seen.contains(&"\u{2e}dotfile.txt".to_string()) || seen.iter().any(|n| n.starts_with('.')),
        "asking for dotfiles shows them: {seen:?}"
    );
    assert!(
        !seen.contains(&"ignored.txt".to_string()),
        "but it must NOT switch off .gitignore: {seen:?}"
    );

    let everything = call(
        &mut kernel,
        "g3",
        "Grep",
        json!({"pattern": "needle", "path": at, "hidden": true, "ignored": true}),
    );
    assert!(
        names(&everything).contains(&"ignored.txt".to_string()),
        "asking for ignored files is its own, separate request: {:?}",
        names(&everything)
    );
    kernel.shutdown();
}

/// No argument may make one search walk the machine.
///
/// An oversized traversal must return an error rather than a short result:
/// a caller must not mistake partial coverage for a complete set of matches.
///
/// Bounded by ENTRIES rather than by a clock, deliberately: this component
/// refuses to answer the same call differently on different machines, and a
/// time limit does exactly that.
#[test]
fn a_search_pointed_at_too_much_ground_says_so_rather_than_running_on() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..40u32 {
        std::fs::write(
            dir.path().join(format!("f{n}.txt")),
            if n == 39 { "needle" } else { "" },
        )
        .unwrap();
    }

    // A ceiling of 10 rather than a filesystem of 200,000: the cap is the
    // behaviour under test, not the number.
    let mut kernel = search_kernel_with(json!({"maxVisited": 10}));
    let out = call(
        &mut kernel,
        "wide",
        "Grep",
        json!({"pattern": "needle", "path": dir.path().display().to_string()}),
    );
    kernel.shutdown();

    assert_eq!(
        out["status"], "error",
        "it must refuse, not half-answer: {out}"
    );
    assert_eq!(out["error"]["code"], "tool.too_wide");
    let said = out["error"]["message"].as_str().unwrap_or_default();
    assert!(
        said.contains("Narrow it") && said.contains("path"),
        "and say what to do about it: {said}"
    );
}
