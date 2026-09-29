//! A real conversation with a real model, through the real wiring.
//! Default brain is the OpenAI-compatible adapter on DeepSeek's NATIVE API
//! (cost decision; its usage carries the fuller cache statistics):
//!   DEEPSEEK_API_KEY=... cargo run --example chat
//! Swap brains with LATTICE_ADAPTER=anthropic (DeepSeek's Anthropic-compat
//! endpoint, or the official API via LATTICE_BASE_URL=https://api.anthropic.com
//!   LATTICE_MODEL=claude-sonnet-5 LATTICE_API_KEY_ENV=ANTHROPIC_API_KEY).
//! The swap changes ONE component name and its config — no wire moves; that
//! is the model-adapter profile doing its job.
//! Ctrl-C once interrupts the current turn; twice exits. /exit or Ctrl-D quits.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

use lattice::components::{
    anthropic_model, context_gate, fs_tools, minimal_loop, net_tools, openai_model, shell_tools,
    silent_ui, timer_tools, workshop_sink,
};
use lattice::core_events as ce;
use lattice::workshop::{
    build_and_install, fetch_and_install, pending_fetch_installs, pending_installs, BuildOutcome,
};
use lattice::{
    AssemblyManifest, ComponentInstance, EventDraft, Factory, Kernel, KernelOptions, Wire,
};

fn main() {
    // Which wire format the brain speaks; both adapters implement the same
    // model-adapter profile, so this only ever changes name + config below
    let adapter = std::env::var("LATTICE_ADAPTER").unwrap_or_else(|_| "openai".to_string());
    let anthropic = adapter == "anthropic";
    let base_url = std::env::var("LATTICE_BASE_URL").unwrap_or_else(|_| {
        if anthropic {
            "https://api.deepseek.com/anthropic".to_string()
        } else {
            "https://api.deepseek.com".to_string()
        }
    });
    let model_name =
        std::env::var("LATTICE_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".to_string());
    let key_env = std::env::var("LATTICE_API_KEY_ENV").unwrap_or_else(|_| {
        if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            "DEEPSEEK_API_KEY".to_string()
        } else {
            "ANTHROPIC_API_KEY".to_string()
        }
    });
    let (brain_name, brain_manifest) = if anthropic {
        (anthropic_model::NAME, anthropic_model::manifest())
    } else {
        (openai_model::NAME, openai_model::manifest())
    };
    if std::env::var(&key_env).is_err() {
        eprintln!("set {key_env} (or point LATTICE_API_KEY_ENV at another variable)");
        std::process::exit(1);
    }

    let registry: HashMap<String, lattice::ComponentManifest> = [
        (silent_ui::NAME.to_string(), silent_ui::manifest()),
        (minimal_loop::NAME.to_string(), minimal_loop::manifest()),
        (context_gate::NAME.to_string(), context_gate::manifest()),
        (brain_name.to_string(), brain_manifest),
        (fs_tools::READER.to_string(), fs_tools::reader_manifest()),
        (fs_tools::WRITER.to_string(), fs_tools::writer_manifest()),
        (shell_tools::NAME.to_string(), shell_tools::manifest()),
        (net_tools::NAME.to_string(), net_tools::manifest()),
        (timer_tools::NAME.to_string(), timer_tools::manifest()),
        (workshop_sink::NAME.to_string(), workshop_sink::manifest()),
    ]
    .into();

    let displayed = Arc::new(Mutex::new(Vec::new()));
    let ui_buffer = Arc::clone(&displayed);
    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(move |_| Box::new(silent_ui::SilentUi::new(Arc::clone(&ui_buffer)))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|config| Box::new(minimal_loop::MinimalLoop::from_config(config))),
    );
    let brain_factory: Factory = if anthropic {
        Box::new(|config| Box::new(anthropic_model::AnthropicModel::from_config(config)))
    } else {
        Box::new(|config| Box::new(openai_model::OpenAiModel::from_config(config)))
    };
    factories.insert(brain_name.to_string(), brain_factory);
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|config| Box::new(context_gate::ContextGate::from_config(config))),
    );
    factories.insert(
        fs_tools::READER.to_string(),
        Box::new(|config| Box::new(fs_tools::FsReader::from_config(config))),
    );
    factories.insert(
        fs_tools::WRITER.to_string(),
        Box::new(|config| Box::new(fs_tools::FsWriter::from_config(config))),
    );
    factories.insert(
        shell_tools::NAME.to_string(),
        Box::new(|config| Box::new(shell_tools::ShellTools::from_config(config))),
    );
    factories.insert(
        net_tools::NAME.to_string(),
        Box::new(|config| Box::new(net_tools::NetTools::from_config(config))),
    );
    factories.insert(
        timer_tools::NAME.to_string(),
        Box::new(|config| Box::new(timer_tools::TimerTools::from_config(config))),
    );
    factories.insert(
        workshop_sink::NAME.to_string(),
        Box::new(|_| Box::new(workshop_sink::WorkshopSink)),
    );

    // Swapping the scripted brain for the real one: the component name and
    // its config change; every wire stays exactly as in the heartbeat
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
                    // Tools are NOT listed here: every wired provider
                    // declares its own in its manifest; the kernel collects
                    config: Some(json!({"model": model_name})),
                },
            ),
            (
                "model".to_string(),
                ComponentInstance {
                    component: brain_name.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({
                        "model": model_name,
                        "baseUrl": base_url,
                        "apiKeyEnv": key_env,
                        // The main brain thinks; the condenser below does not
                        "thinking": "high",
                    })),
                },
            ),
            (
                "ctx".to_string(),
                ComponentInstance {
                    component: context_gate::NAME.to_string(),
                    requires: Vec::new(),
                    // The model profile feeds the window scale; the usage
                    // field name follows the dialect (DeepSeek native counts
                    // prompt_tokens, the Anthropic wire counts input_tokens)
                    // The gate assembles the system prompt: this base text
                    // plus every component's own fragment (the workshop and
                    // the recall tool explain themselves via their manifests)
                    config: Some(json!({"profile": {
                        "contextWindow": 1_000_000,
                        "usageFields": {"input":
                            if anthropic { "input_tokens" } else { "prompt_tokens" }},
                    }, "condense": true,
                    "system": "You are a helpful assistant running inside Lattice, an event-sourced agent runtime. Be concise."})),
                },
            ),
            (
                "cmodel".to_string(),
                ComponentInstance {
                    component: brain_name.to_string(),
                    requires: Vec::new(),
                    // The gate's own condenser: same brain component, second
                    // instance, assembly-isolated from the main loop
                    config: Some(json!({
                        "model": model_name,
                        "baseUrl": base_url,
                        "apiKeyEnv": key_env,
                        "maxTokens": 1024,
                        "thinking": false,
                        "system": "You condense conversation history. Reply with ONLY a compact summary in exactly four sections: Done: (what was completed) State: (where things stand) Open: (unresolved items) Facts: (key paths, decisions, numbers, identifiers). No preamble.",
                    })),
                },
            ),
            ("fs-write".into(), ComponentInstance {
                component: fs_tools::WRITER.into(), requires: vec![],
                config: Some(json!({"root":"./workspace"})),
            }),
            (
                "fs".to_string(),
                ComponentInstance {
                    component: fs_tools::READER.to_string(),
                    requires: Vec::new(),
                    // A working directory the agent may read and write
                    config: Some(json!({"root": "./workspace"})),
                },
            ),
            (
                "shell".to_string(),
                ComponentInstance {
                    component: shell_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: Some(json!({"cwd": "./workspace"})),
                },
            ),
            (
                "net".to_string(),
                ComponentInstance {
                    component: net_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "timer".to_string(),
                ComponentInstance {
                    component: timer_tools::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
            (
                "workshop".to_string(),
                ComponentInstance {
                    component: workshop_sink::NAME.to_string(),
                    requires: Vec::new(),
                    config: None,
                },
            ),
        ]
        .into(),
        wires: vec![
            Wire::new("ui.user", "loop.input"),
            Wire::new("loop.ask", "ctx.ask"),
            Wire::new("ctx.forward", "model.request"),
            Wire::new("ctx.condense", "cmodel.request"),
            Wire::new("cmodel.result", "ctx.condensed"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "fs.execute"),
            Wire::new("fs.outcome", "loop.tools"),
            Wire::new("loop.run", "fs-write.execute"),
            Wire::new("fs-write.outcome", "loop.tools"),
            Wire::new("loop.run", "shell.execute"),
            Wire::new("shell.outcome", "loop.tools"),
            // A finished background command wakes the loop as fresh input
            Wire::new("shell.wake", "loop.input"),
            Wire::new("loop.run", "net.execute"),
            Wire::new("net.outcome", "loop.tools"),
            Wire::new("loop.run", "timer.execute"),
            Wire::new("timer.outcome", "loop.tools"),
            Wire::new("timer.wake", "loop.input"),
            Wire::new("loop.run", "workshop.execute"),
            Wire::new("workshop.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
            Wire::new("ui.interrupt", "model.control"),
        ],
    };

    // The ledger goes to disk: this session is replayable and debuggable
    let ledger_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".lattice")
        .join("chat");
    std::fs::create_dir_all(&ledger_dir).expect("cannot create the ledger directory");
    let session = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let ledger_path = ledger_dir.join(format!("{session}.jsonl"));
    let mut kernel = Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions {
            stream: Some(format!("chat-{session}")),
            log_file: Some(ledger_path.clone()),
            ..KernelOptions::default()
        },
    )
    .expect("assembly must pass inspection");

    // Streaming fragments print live; tool activity and usage print from the
    // log — what you see is what was recorded
    kernel.set_notice_handler(|_, payload| {
        if let Some(chunk) = payload["chunk"].as_str() {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        }
    });
    kernel.subscribe_log(|event| match event.event_type.as_str() {
        ce::TOOL_EXEC_STARTED => eprintln!(
            "\x1b[2m⚙ {} {}\x1b[0m",
            event.payload["tool"].as_str().unwrap_or("?"),
            event.payload["arguments"]
        ),
        ce::TOOL_EXEC_COMPLETED => {
            eprintln!("\x1b[2m  → {}\x1b[0m", event.payload["result"].clone())
        }
        ce::MODEL_CALL_COMPLETED => {
            // Providers name usage fields differently; print what exists
            if let Some(usage) = event.payload["usage"].as_object() {
                let line: Vec<String> = usage
                    .iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect();
                if !line.is_empty() {
                    eprintln!("\x1b[2m[{}]\x1b[0m", line.join(" · "));
                }
            }
        }
        _ => {}
    });

    // Ctrl-C once = interrupt event; twice = exit
    let presses = Arc::new(AtomicUsize::new(0));
    let presses_in_handler = Arc::clone(&presses);
    let interrupter = kernel.injector("ui");
    ctrlc::set_handler(move || {
        if presses_in_handler.fetch_add(1, Ordering::SeqCst) == 0 {
            eprintln!("\n⏹ interrupting (Ctrl-C again to quit)");
            interrupter.emit(
                "interrupt",
                EventDraft::new(ce::INTERRUPTED, &[], json!({"by": "user"})),
            );
        } else {
            std::process::exit(130);
        }
    })
    .expect("failed to install the Ctrl-C handler");

    let injector = kernel.injector("ui");
    let workshop_dir = std::env::temp_dir().join("lattice-workshop");
    std::fs::create_dir_all(&workshop_dir).ok();
    let stdin = std::io::stdin();
    println!("lattice chat · {model_name} @ {base_url} · /exit to quit");
    println!("\x1b[2mledger: {}\x1b[0m", ledger_path.display());
    loop {
        presses.store(0, Ordering::SeqCst);
        print!("\x1b[1myou ›\x1b[0m ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/exit" {
            break;
        }
        injector.emit(
            "user",
            EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": line})),
        );
        // Run to quiescence, then service any tool the agent asked to build,
        // then run again — the workshop loop
        loop {
            kernel
                .run_until_quiescent()
                .expect("the run must not fail on IO");
            let pending = pending_installs(&kernel).expect("read pending installs");
            let fetches = pending_fetch_installs(&kernel).expect("read pending fetch installs");
            if pending.is_empty() && fetches.is_empty() {
                break;
            }
            for req in fetches {
                let components_dir = lattice::overlay::default_path()
                    .parent()
                    .expect("the overlay path has a parent")
                    .join("components");
                let outcome = fetch_and_install(
                    &mut kernel,
                    &req,
                    &components_dir,
                    "loop",
                    Some(&lattice::overlay::default_path()),
                    |manifest, tool| {
                        eprintln!(
                            "\n  ┌─ install request ──────────────────\n  │ component: {} (from a source, own process)\n  │ offers tool: {}\n  │ effects: {:?}\n  │ canon + conformance exam: passed ✓\n  └─ allow into this session? [y/N]",
                            manifest.name, tool["name"], manifest.capabilities
                        );
                        let mut answer = String::new();
                        std::io::stdin().lock().read_line(&mut answer).ok();
                        answer.trim().eq_ignore_ascii_case("y")
                    },
                )
                .expect("workshop IO");
                let payload = match outcome {
                    BuildOutcome::Installed(msg) => {
                        eprintln!("\x1b[2m  → {msg}\x1b[0m");
                        json!({"call": req.call, "status": "ok", "result": msg})
                    }
                    BuildOutcome::Rejected(why) => {
                        eprintln!("\x1b[2m  → rejected: {why}\x1b[0m");
                        json!({"call": req.call, "status": "error",
                            "error": {"code": "workshop.rejected", "message": why, "blame": "request"}})
                    }
                };
                kernel.injector("workshop").emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
                );
            }
            for req in pending {
                let outcome = build_and_install(
                    &mut kernel,
                    &req,
                    &workshop_dir,
                    "loop",
                    Some(&lattice::overlay::default_path()),
                    |manifest, tool| {
                        eprintln!(
                            "\n  ┌─ install request ──────────────────\n  │ component: {} (Python, own process)\n  │ offers tool: {}\n  │ effects: {:?}\n  │ inspection + conformance exam: passed ✓\n  └─ allow into this session? [y/N]",
                            manifest.name, tool["name"], manifest.capabilities
                        );
                        let mut answer = String::new();
                        std::io::stdin().lock().read_line(&mut answer).ok();
                        answer.trim().eq_ignore_ascii_case("y")
                    },
                )
                .expect("workshop IO");
                let payload = match outcome {
                    BuildOutcome::Installed(msg) => {
                        eprintln!("\x1b[2m  → {msg}\x1b[0m");
                        json!({"call": req.call, "status": "ok", "result": msg})
                    }
                    BuildOutcome::Rejected(why) => {
                        eprintln!("\x1b[2m  → rejected: {why}\x1b[0m");
                        json!({"call": req.call, "status": "error",
                            "error": {"code": "workshop.rejected", "message": why, "blame": "request"}})
                    }
                };
                kernel.injector("workshop").emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&req.cause], payload),
                );
            }
        }
        println!();
    }
    println!(
        "bye — {} events on the ledger: {}",
        kernel.log().len(),
        ledger_path.display()
    );
}
