//! The edges of what a tool can reach, and what it hands on.
//!
//! Each of these is a boundary the declaration already claimed. A skill
//! library that reads only its own folders, a search confined to a root, a
//! component subprocess that is somebody else's program: all three were
//! described that way and none of them held, because the argument or the
//! environment carried further than the description did.

use std::collections::HashMap;
use std::io::Read;

use serde_json::{json, Value};

use lattice::components::skill_library;
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

/// Emits tool requests and nothing else, so a tool can be exercised through
/// the kernel exactly as one really is.
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

/// One kernel, one tool component behind a driver.
fn one_tool(component: &str, manifest: ComponentManifest, factory: Factory) -> Kernel {
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (component.to_string(), manifest),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    factories.insert(component.to_string(), factory);
    let assembly = AssemblyManifest {
        instances: [
            ("driver".to_string(), ComponentInstance::new("driver", None)),
            ("tool".to_string(), ComponentInstance::new(component, None)),
        ]
        .into(),
        wires: vec![Wire::new("driver.out", "tool.execute")],
    };
    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .unwrap()
}

fn call(kernel: &mut Kernel, id: &str, tool: &str, args: Value) -> Value {
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

/// A name is a name, not a path.
///
/// `Path::join` REPLACES the whole path when the thing joined is absolute, so
/// `load_skill("/etc/anything")` read from wherever it was pointed, and `..`
/// walked out the same way. The skill name is the one argument a model
/// chooses freely, and "skills are read only from the configured folders" was
/// not true of it.
#[test]
fn a_skill_name_cannot_be_a_path_to_somewhere_else() {
    for escape in [
        "/etc/passwd",
        "../../../etc",
        "..",
        ".",
        "sub/dir",
        "Upper",
        "has space",
        "trailing-",
        "-leading",
        "double--hyphen",
        "",
    ] {
        assert!(
            !skill_library::is_skill_name(escape),
            "{escape:?} must not be accepted as a skill name"
        );
    }
    for real in ["writing", "pdf-forms", "a1", "one-two-three"] {
        assert!(
            skill_library::is_skill_name(real),
            "{real:?} is a perfectly ordinary skill name"
        );
    }
}

/// A confined search stays inside its root even when the way out is a link.
///
/// `follow_links(false)` governs links met while walking; the walk's own
/// STARTING point is followed regardless. So a link inside the root — one the
/// agent can make with a single shell command — was a legal `path` argument
/// that searched wherever it pointed, while the declared surface said "the
/// root". The file tools had checked this from the beginning.
#[cfg(unix)]
#[test]
fn a_confined_search_cannot_start_on_a_link_out_of_the_root() {
    use lattice::components::search_tools;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "not yours").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    let confined = json!({"root": root.path().display().to_string()});

    let mut kernel = one_tool(
        search_tools::NAME,
        search_tools::manifest(),
        Box::new(move |_| Box::new(search_tools::SearchTools::from_config(Some(&confined)))),
    );
    let found = call(
        &mut kernel,
        "g1",
        "Find",
        json!({"glob": "*.txt", "path": "escape"}),
    );
    kernel.shutdown();

    assert_eq!(
        found["status"], "error",
        "a start outside the root must be refused, not searched: {found}"
    );
    let message = found["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("outside"), "and say why: {message}");
}

/// A command that leaves something running in the background returns anyway.
///
/// bash exiting does not close the pipe: whatever it started with `&` still
/// holds the same stdout. Reading to the end therefore waited for the
/// BACKGROUND process — past the watchman's deadline, deaf to the
/// cancellation it raised (a blocking read cannot be interrupted), until the
/// whole shell instance was declared unresponsive and the session had no
/// `Run` left. One ampersand.
#[cfg(unix)]
#[test]
fn a_command_that_backgrounds_something_does_not_hold_the_call_open() {
    use lattice::components::shell_tools;

    let dir = tempfile::tempdir().unwrap();
    let here = json!({"cwd": dir.path().display().to_string()});
    let mut kernel = one_tool(
        shell_tools::NAME,
        shell_tools::manifest(),
        Box::new(move |_| Box::new(shell_tools::ShellTools::from_config(Some(&here)))),
    );

    let started = std::time::Instant::now();
    let result = call(
        &mut kernel,
        "r1",
        "Run",
        json!({"command": "echo before; sleep 30 &"}),
    );
    let took = started.elapsed();
    kernel.shutdown();

    assert_eq!(result["status"], "ok", "the call finished: {result}");
    assert!(
        took < std::time::Duration::from_secs(5),
        "it must not wait for the background process (took {took:?})"
    );
    let out = result["result"]["stdout"].as_str().unwrap_or_default();
    assert!(
        out.contains("before"),
        "and what the command did print is still there: {out:?}"
    );
    assert!(
        result["result"]["note"].is_string(),
        "and it says the output is not the whole story: {result}"
    );
}

/// Somebody else's program does not get this session's keys.
///
/// A child inherits the whole environment unless told otherwise, so a
/// process-form component — foreign code, running next door, installed on one
/// approval — could read every API key the session was started with in a
/// single line. The kernel is told which names to withhold, never what they
/// mean.
#[cfg(unix)]
#[test]
fn a_component_subprocess_is_not_handed_the_withheld_variables() {
    use std::collections::HashMap;

    use lattice::components::silent_ui;
    use lattice::{
        AssemblyManifest, ComponentInstance, ComponentManifest, Factory, Kernel, KernelOptions,
        PortDecl, RuntimeKind, Wire,
    };

    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("seen.txt");
    let script = dir.path().join("peek.py");
    std::fs::write(
        &script,
        format!(
            "import json, os, sys\n\
             open({:?}, 'w').write(\
                 (os.environ.get('LATTICE_TEST_SECRET') or '<absent>') + '|' + \
                 (os.environ.get('PATH') and 'PATH-ok' or 'PATH-missing'))\n\
             for line in sys.stdin:\n    pass\n",
            report.display().to_string()
        ),
    )
    .unwrap();

    std::env::set_var("LATTICE_TEST_SECRET", "sk-do-not-share");

    let peeker = ComponentManifest {
        name: "peeker".to_string(),
        version: "0".to_string(),
        runtime: RuntimeKind::Process,
        entry: format!("python3 {}", script.display()),
        inputs: vec![PortDecl::new(
            "input",
            &[lattice::core_events::USER_MESSAGE],
        )],
        outputs: Vec::new(),
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: Vec::new(),
        tools: Vec::new(),
        prompt: None,
        handle_timeout_ms: None,
        concurrency: None,
    };
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        ("peeker".to_string(), peeker),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| {
            Box::new(silent_ui::SilentUi::new(std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            )))
        }),
    );
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                ComponentInstance {
                    component: silent_ui::NAME.to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
            (
                "peeker".to_string(),
                ComponentInstance {
                    component: "peeker".to_string(),
                    config: None,
                    requires: Vec::new(),
                },
            ),
        ]
        .into(),
        wires: vec![Wire::new("ui.user", "peeker.input")],
    };
    let options = KernelOptions {
        child_env_deny: vec!["LATTICE_TEST_SECRET".to_string()],
        ..KernelOptions::default()
    };
    let kernel = Kernel::start(&assembly, &registry, &mut factories, options).unwrap();
    // The child writes its report at startup; give it a moment to exist.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !report.exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    kernel.shutdown();
    std::env::remove_var("LATTICE_TEST_SECRET");

    let mut seen = String::new();
    std::fs::File::open(&report)
        .expect("the child ran and reported")
        .read_to_string(&mut seen)
        .unwrap();
    assert!(
        seen.starts_with("<absent>"),
        "the withheld variable must not reach the child: {seen}"
    );
    assert!(
        seen.contains("PATH-ok"),
        "and the rest of the environment must still be there, or nothing runs: {seen}"
    );
}
