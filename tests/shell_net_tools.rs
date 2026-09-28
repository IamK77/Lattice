//! The dangerous toolset: `run` (bash, executes) and `fetch` (HTTP, network).
//! Behavior against a real bash and a local canned server, the watchman
//! killing a runaway command, and an effects-policy gate denying both by
//! their declared surface.
#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use lattice::components::{
    effects_policy, minimal_loop, net_tools, scripted_model, shell_tools, silent_ui,
};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, Component, ComponentInstance, ComponentManifest, Ctx, EventDraft,
    EventEnvelope, Factory, Kernel, KernelOptions, PortDecl, RuntimeKind, Wire,
};

struct Driver;
impl Component for Driver {
    fn handle(&mut self, _p: &str, _e: &EventEnvelope, _c: &mut Ctx) {}
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

/// driver → one tool provider (with an optional manifest override, e.g. a
/// short deadline for the cancellation test).
fn tool_kernel(provider: ComponentManifest, factory: Factory) -> Kernel {
    let name = provider.name.clone();
    let registry: HashMap<String, ComponentManifest> = [
        ("driver".to_string(), driver_manifest()),
        (name.clone(), provider),
    ]
    .into();
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert("driver".to_string(), Box::new(|_| Box::new(Driver)));
    factories.insert(name.clone(), factory);
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
                "tool".to_string(),
                ComponentInstance {
                    component: name,
                    requires: Vec::new(),
                    config: None,
                },
            ),
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

/// shell-tools pointed at a specific directory.
fn shell_kernel(cwd: &std::path::Path) -> Kernel {
    let cwd = cwd.to_string_lossy().into_owned();
    tool_kernel(
        shell_tools::manifest(),
        Box::new(move |_| {
            Box::new(shell_tools::ShellTools::from_config(Some(&json!({
                "cwd": cwd,
            }))))
        }),
    )
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
        .expect("an outcome")
        .payload
}

#[test]
fn run_reports_stdout_stderr_and_exit_code() {
    let mut kernel = tool_kernel(
        shell_tools::manifest(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    );
    let ok = call(&mut kernel, "r1", "Run", json!({"command": "echo hello"}));
    assert_eq!(ok["status"], "ok");
    assert_eq!(ok["result"]["exit_code"], 0);
    assert_eq!(ok["result"]["stdout"], "hello\n");
    let log = &ok["result"]["logs"]["stdout"];
    assert_eq!(log["complete"], true);
    assert_eq!(log["sealed"], true);
    assert_eq!(
        std::fs::read_to_string(log["path"].as_str().unwrap()).unwrap(),
        "hello\n"
    );

    // A non-zero exit is data, not a tool error
    let failed = call(
        &mut kernel,
        "r2",
        "Run",
        json!({"command": "echo oops >&2; exit 3"}),
    );
    assert_eq!(failed["status"], "ok");
    assert_eq!(failed["result"]["exit_code"], 3);
    assert_eq!(failed["result"]["stderr"], "oops\n");
    kernel.shutdown();
}

/// Long output keeps BOTH ends. Cutting only the tail throws away the part
/// that usually matters most — a build prints its errors last, a test run its
/// summary — and the reader cannot even tell how much went missing.
#[test]
fn long_output_keeps_its_head_and_tail_and_counts_what_it_dropped() {
    let mut kernel = tool_kernel(
        shell_tools::manifest(),
        // A small ceiling so a short command overflows it
        Box::new(|_| {
            Box::new(shell_tools::ShellTools::from_config(Some(
                &json!({"maxBytes": 400}),
            )))
        }),
    );
    let out = call(&mut kernel, "r1", "Run", json!({"command": "seq 1 500"}));
    let stdout = out["result"]["stdout"].as_str().unwrap();
    assert!(stdout.starts_with("1\n2\n"), "the beginning survives");
    assert!(stdout.trim_end().ends_with("500"), "the END survives too");
    assert!(
        stdout.contains("lines omitted"),
        "and it says how much went missing: {stdout}"
    );
    // The ceiling is the whole point: keeping two ends must not quietly cost
    // twice the budget
    assert!(
        stdout.len() <= 400,
        "the clip must respect its ceiling, got {} bytes",
        stdout.len()
    );
    kernel.shutdown();
}

#[test]
fn the_watchman_kills_a_runaway_command() {
    // A short deadline: the kernel cancels the delivery, the tool kills the child
    let mut manifest = shell_tools::manifest();
    manifest.handle_timeout_ms = Some(300);
    let mut kernel = tool_kernel(
        manifest,
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    );
    let started = Instant::now();
    let outcome = call(&mut kernel, "slow", "Run", json!({"command": "sleep 30"}));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the command must be killed, not waited out"
    );
    assert_eq!(outcome["status"], "cancelled");
    kernel.shutdown();
}

/// Serve exactly one canned HTTP response on an ephemeral local port.
fn canned_server(status_line: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf); // consume the request line/headers
            let _ = write!(
                stream,
                "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn fetch_stops_at_the_download_bound_and_identifies_its_client() {
    use std::io::BufRead;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (headers_tx, headers_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        headers_tx.send(headers).unwrap();
        let mut stream = reader.into_inner();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n123456789"
        )
        .unwrap();
        stream.flush().unwrap();
        // The rest of the advertised body never arrives before Fetch returns.
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    let mut manifest = net_tools::manifest();
    manifest.handle_timeout_ms = Some(3000);
    let mut kernel = tool_kernel(
        manifest,
        Box::new(|_| {
            Box::new(net_tools::NetTools::from_config(Some(
                &json!({"maxDownloadBytes":8}),
            )))
        }),
    );
    let outcome = call(&mut kernel, "bounded", "Fetch", json!({"url":url}));
    let _ = release_tx.send(());
    kernel.shutdown();
    server.join().unwrap();
    assert_eq!(outcome["status"], "ok", "{outcome}");
    assert_eq!(outcome["result"]["body"], "12345678");
    assert_eq!(outcome["result"]["sourceTruncated"], true);
    assert_eq!(outcome["result"]["raw"]["complete"], false);
    assert_eq!(
        std::fs::read(outcome["result"]["raw"]["path"].as_str().unwrap()).unwrap(),
        b"12345678"
    );
    assert!(headers_rx
        .recv()
        .unwrap()
        .to_ascii_lowercase()
        .contains("user-agent: lattice"));
}

#[test]
fn fetch_returns_status_and_body() {
    let url = canned_server("200 OK", "hello from the server");
    let mut kernel = tool_kernel(
        net_tools::manifest(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
    );
    let outcome = call(&mut kernel, "f1", "Fetch", json!({"url": url}));
    assert_eq!(outcome["status"], "ok", "{outcome}");
    assert_eq!(outcome["result"]["http_status"], 200);
    assert_eq!(outcome["result"]["body"], "hello from the server");
    kernel.shutdown();
}

#[test]
fn fetch_refuses_a_non_http_url() {
    let mut kernel = tool_kernel(
        net_tools::manifest(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
    );
    let outcome = call(
        &mut kernel,
        "f2",
        "Fetch",
        json!({"url": "file:///etc/passwd"}),
    );
    assert_eq!(outcome["status"], "error");
    assert_eq!(outcome["error"]["code"], "tool.bad_arguments");
    kernel.shutdown();
}

/// loop → policy → provider: the policy judges by the tool's declared
/// surface. Returns whether the tool was forwarded (allowed).
fn allowed_under_policy(
    provider: ComponentManifest,
    factory: Factory,
    tool: &str,
    args: serde_json::Value,
    policy_config: serde_json::Value,
) -> bool {
    let provider_name = provider.name.clone();
    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (scripted_model::NAME.to_string(), scripted_model::manifest()),
        (effects_policy::NAME.to_string(), effects_policy::manifest()),
        (provider_name.clone(), provider),
    ]
    .into();
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "t1", "tool": tool, "arguments": args}]},
        {"status": "ok", "text": "done"},
    ]});
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
    f.insert(provider_name.clone(), factory);
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
                    config: Some(policy_config),
                },
            ),
            (
                "tool".to_string(),
                ComponentInstance {
                    component: provider_name,
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
            Wire::new("loop.run", "policy.review"),
            Wire::new("policy.forward", "tool.execute"),
            Wire::new("policy.verdict", "loop.tools"),
            Wire::new("tool.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };
    let mut kernel = Kernel::start(&assembly, &registry, &mut f, KernelOptions::default()).unwrap();
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": "go"})),
    );
    kernel.run_until_quiescent().unwrap();
    let forwarded = kernel
        .log()
        .replay(1)
        .unwrap()
        .iter()
        .any(|e| e.event_type == ce::TOOL_EXEC_STARTED && e.source == "policy");
    kernel.shutdown();
    forwarded
}

#[test]
fn the_policy_denies_run_and_fetch_by_default_and_allows_when_permitted() {
    // executes is denied by default, allowed when the policy permits it
    assert!(!allowed_under_policy(
        shell_tools::manifest(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
        "Run",
        json!({"command": "echo hi"}),
        json!({}),
    ));
    // Permitting EXECUTION alone is not enough, and this is the point: a shell
    // writes and reaches the network through whatever it starts. While `run`
    // declared only `executes`, a policy that forbade writing forwarded it
    // anyway — the rule never fired because the declaration never mentioned
    // writing. A surface must name everything the tool can reach.
    assert!(!allowed_under_policy(
        shell_tools::manifest(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
        "Run",
        json!({"command": "echo hi"}),
        json!({"allowExecutes": true}),
    ));
    assert!(allowed_under_policy(
        shell_tools::manifest(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
        "Run",
        json!({"command": "echo hi"}),
        json!({"allowExecutes": true, "allowWrites": true, "allowNetwork": true}),
    ));
    // network likewise
    assert!(!allowed_under_policy(
        net_tools::manifest(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
        "Fetch",
        json!({"url": "http://example.com"}),
        json!({}),
    ));
    // Saving retrievable evidence is a write, not a network-only operation.
    assert!(!allowed_under_policy(
        net_tools::manifest(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
        "Fetch",
        json!({"url": "http://example.com"}),
        json!({"allowNetwork": true}),
    ));
    assert!(allowed_under_policy(
        net_tools::manifest(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
        "Fetch",
        json!({"url": "http://example.com"}),
        json!({"allowNetwork": true, "allowWrites": true}),
    ));
}

// ── Where a command runs is answerable, not asserted ───────────────────────

/// A model cannot check a claim in its prompt, and a `cd <repo> &&` prefix
/// costs it nothing — so it prefixed one onto every command in a real session,
/// 16 out of 16, with the prompt saying plainly that it need not. The fix is
/// structural: every result carries the directory the command actually ran in,
/// so after one call the model has SEEN it rather than been told. It is also
/// the audit answer — the ledger could not say where a command executed.
#[test]
fn every_result_reports_the_directory_it_ran_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = shell_kernel(dir.path());

    let out = call(&mut kernel, "c1", "Run", json!({"command": "pwd"}));
    assert_eq!(out["status"], "ok", "{out}");
    let reported = out["result"]["cwd"]
        .as_str()
        .expect("the directory is reported");
    let printed = out["result"]["stdout"].as_str().unwrap().trim().to_string();
    assert!(
        std::fs::canonicalize(reported).unwrap() == std::fs::canonicalize(&printed).unwrap(),
        "reported {reported:?} but the command itself printed {printed:?}"
    );
    kernel.shutdown();
}

/// And a directory is a PARAMETER, so getting there needs no string surgery.
/// `cd x && ...` buries the destination inside an opaque command; a field
/// lands on the ledger as its own value, which is what makes it auditable.
#[test]
fn a_command_can_be_pointed_somewhere_without_splicing_cd_into_it() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut kernel = shell_kernel(dir.path());

    let out = call(
        &mut kernel,
        "c1",
        "Run",
        json!({"command": "pwd", "cwd": elsewhere.to_string_lossy()}),
    );
    let printed = out["result"]["stdout"].as_str().unwrap().trim();
    assert_eq!(
        std::fs::canonicalize(printed).unwrap(),
        std::fs::canonicalize(&elsewhere).unwrap(),
        "the command ran where it was pointed: {out}"
    );
    assert_eq!(
        std::fs::canonicalize(out["result"]["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&elsewhere).unwrap(),
        "and says so"
    );

    // The default is untouched by one call that went elsewhere.
    let back = call(&mut kernel, "c2", "Run", json!({"command": "pwd"}));
    assert_eq!(
        std::fs::canonicalize(back["result"]["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap(),
        "{back}"
    );
    kernel.shutdown();
}

/// The default ceiling is a CONTEXT budget, and the number that shipped was
/// 1 MiB — roughly 350k tokens, larger than the window the result has to fit
/// into. One `tail -20` of a ledger returned 131 KB and the next model call's
/// cache hit rate fell from 99.5% to 26.4%. Nothing downstream defends
/// against this: the context gate trims only once the WHOLE context crosses
/// its threshold and has no opinion about a single oversized result.
#[test]
fn the_default_output_ceiling_fits_in_a_context_window() {
    let mut kernel = tool_kernel(
        shell_tools::manifest(),
        // No config: exactly what the preset builds
        Box::new(|_| Box::new(shell_tools::ShellTools::from_config(None))),
    );
    let out = call(&mut kernel, "r1", "Run", json!({"command": "seq 1 200000"}));
    let stdout = out["result"]["stdout"].as_str().unwrap();
    assert!(
        stdout.len() <= 16_384,
        "the default let {} bytes through — a single command must not be able \
         to spend a large part of the window",
        stdout.len()
    );
    // And it is still the clip that keeps both ends, not a bare truncation
    assert!(stdout.starts_with("1\n2\n"), "the beginning survives");
    assert!(stdout.trim_end().ends_with("200000"), "so does the end");
    assert!(
        stdout.contains("lines omitted"),
        "and it says what it dropped"
    );
    kernel.shutdown();
}
