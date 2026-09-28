//! A live walk through the skill story — install, menu, invoke — over the
//! REAL wiring, driven by a scripted (keyless) brain so it runs anywhere.
//!
//!   cargo run --example skill_demo
//!
//! It builds the standard assembly's skill path (the expansion station on the
//! input line, the library on the tool line), then:
//!   1. shows the menu is empty (no skills yet)
//!   2. has the model install a skill from a local folder
//!   3. shows the skill.listing event the frontends fold into their palette
//!   4. types "/pirate Ana" — the expansion station swaps the body in with NO
//!      model round-trip, and the ledger keeps both the typed line and the
//!      expansion, causally linked

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use lattice::components::{minimal_loop, scripted_model, silent_ui, skill_library};
use lattice::core_events as ce;
use lattice::{
    AssemblyManifest, ComponentInstance, EventDraft, EventEnvelope, Factory, Kernel, KernelOptions,
    Wire,
};

fn main() {
    // A scratch home: the library scans (and installs into) `skills/`, and a
    // separate `donor/` holds the skill we will install FROM.
    let home = tempdir();
    let skills_dir = home.join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    let donor = home.join("donor").join("pirate");
    std::fs::create_dir_all(&donor).unwrap();
    std::fs::write(
        donor.join("SKILL.md"),
        "---\nname: pirate\ndescription: rewrite a greeting in pirate voice\n---\n\
         Rewrite the following greeting as a pirate would say it: $ARGUMENTS. \
         Keep it to one sentence.",
    )
    .unwrap();

    // The model's script: turn one installs the skill, turn two (after the
    // /pirate expansion reaches it) just acknowledges — a real brain would act
    // on the expanded instructions; scripted, we only prove the plumbing.
    let script = json!({"script": [
        {"status": "ok", "toolCalls": [{"id": "c1", "tool": "install_skill", "arguments": {
            "source": donor.display().to_string(),
            "reason": "the user wants pirate greetings",
        }}]},
        {"status": "ok", "text": "installed — try /pirate <name>"},
        {"status": "ok", "text": "(the brain would now speak like a pirate)"},
    ]});

    let mut kernel = start(&skills_dir, script);

    // ── 1. The menu starts empty ──────────────────────────────────────────
    kernel.run_until_quiescent().unwrap();
    banner("1. before any skill — the palette menu is empty");
    print_menu(&kernel);

    // ── 2. The user asks; the model installs the skill ────────────────────
    banner("2. user: \"give me a pirate greeting skill\" → model calls install_skill");
    send(&mut kernel, "give me a pirate greeting skill");
    print_installs(&kernel);

    // ── 3. The menu now carries the skill (this is the skill.listing event
    //       a frontend folds into its / palette) ────────────────────────────
    banner("3. the menu the frontends now show (from the skill.listing event)");
    print_menu(&kernel);

    // ── 4. Invoke it: "/pirate Ana" — expanded on the input line, NO model
    //       round-trip; the ledger keeps the typed line AND the expansion ────
    banner("4. user types \"/pirate Ana\" — the expansion station swaps the body in");
    send(&mut kernel, "/pirate Ana");
    print_expansion(&kernel);

    kernel.shutdown();
}

/// Build the skill path of the standard assembly: the expansion station on the
/// input line (ui.user → skills.input → skills.expanded → loop.input), the
/// library on the tool line. Exactly the preset's shape, minus the brains we
/// do not need for a keyless demo.
fn start(skills_dir: &std::path::Path, script: Value) -> Kernel {
    let registry = [
        (silent_ui::NAME, silent_ui::manifest()),
        (minimal_loop::NAME, minimal_loop::manifest()),
        (scripted_model::NAME, scripted_model::manifest()),
        (skill_library::NAME, skill_library::manifest()),
    ]
    .into_iter()
    .map(|(n, m)| (n.to_string(), m))
    .collect();

    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(Arc::new(Mutex::new(Vec::new()))))),
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
        skill_library::NAME.to_string(),
        Box::new(|c| Box::new(skill_library::SkillLibrary::from_config(c))),
    );

    let instance = |component: &str, config: Option<Value>| ComponentInstance {
        component: component.to_string(),
        config,
        requires: Vec::new(),
    };
    let assembly = AssemblyManifest {
        instances: [
            ("ui".to_string(), instance(silent_ui::NAME, None)),
            ("loop".to_string(), instance(minimal_loop::NAME, None)),
            (
                "model".to_string(),
                instance(scripted_model::NAME, Some(script)),
            ),
            (
                "skills".to_string(),
                instance(
                    skill_library::NAME,
                    Some(json!({"dirs": [skills_dir.display().to_string()]})),
                ),
            ),
        ]
        .into(),
        wires: vec![
            // The expansion station: everything the user types passes through
            Wire::new("ui.user", "skills.input"),
            Wire::new("skills.expanded", "loop.input"),
            Wire::new("loop.ask", "model.request"),
            Wire::new("model.result", "loop.model"),
            Wire::new("loop.run", "skills.execute"),
            Wire::new("skills.outcome", "loop.tools"),
            Wire::new("loop.out", "ui.display"),
        ],
    };

    Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    )
    .expect("the skill assembly boots")
}

fn send(kernel: &mut Kernel, text: &str) {
    kernel.injector("ui").emit(
        "user",
        EventDraft::new(ce::USER_MESSAGE, &[], json!({"text": text})),
    );
    kernel.run_until_quiescent().unwrap();
}

fn ledger(kernel: &Kernel) -> Vec<EventEnvelope> {
    kernel.log().replay(1).expect("read the completed ledger")
}

/// The latest skill.listing event, rendered as the menu a frontend shows.
fn print_menu(kernel: &Kernel) {
    let latest = ledger(kernel)
        .into_iter()
        .rev()
        .find(|e| e.event_type == skill_library::SKILL_LISTING);
    match latest {
        None => println!("   (no skill.listing event yet — nothing to show)"),
        Some(e) => {
            let skills = e.payload["skills"].as_array().cloned().unwrap_or_default();
            if skills.is_empty() {
                println!("   (menu is empty)");
            }
            for s in skills {
                println!(
                    "   /{}  —  {}",
                    s["name"].as_str().unwrap_or("?"),
                    s["description"].as_str().unwrap_or("")
                );
            }
        }
    }
}

/// The install's outcome and its reasoned decision event.
fn print_installs(kernel: &Kernel) {
    for e in ledger(kernel) {
        if e.event_type == ce::TOOL_EXEC_COMPLETED && e.source == "skills" {
            if let Some(r) = e.payload["result"]["installed"].as_str() {
                println!("   ✓ installed \"{r}\"  ({})", e.payload["result"]["dir"]);
            }
        }
        if e.event_type == skill_library::SKILL_INSTALLED {
            println!(
                "   ↳ decision recorded: {} — reason: {}",
                e.payload["name"].as_str().unwrap_or("?"),
                e.reason.as_deref().unwrap_or("")
            );
        }
    }
}

/// The typed original and the expansion, side by side — both on the ledger,
/// the expansion caused by the original.
fn print_expansion(kernel: &Kernel) {
    let events = ledger(kernel);
    // The typed original: a causeless user message beginning with '/'
    let original = events.iter().find(|e| {
        e.event_type == ce::USER_MESSAGE
            && e.causes.is_empty()
            && e.payload["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("/pirate"))
    });
    let Some(original) = original else {
        println!("   (no /pirate message found)");
        return;
    };
    println!("   typed by the user : {}", original.payload["text"]);
    // Its expansion: the caused re-emission the station produced
    let expanded = events
        .iter()
        .find(|e| e.event_type == ce::USER_MESSAGE && e.causes.contains(&original.id));
    match expanded {
        Some(e) => {
            println!("   the model receives: {}", e.payload["text"]);
            println!(
                "   (expansion event {} ← caused by {}, no model round-trip)",
                e.id, original.id
            );
        }
        None => println!("   (no expansion produced)"),
    }
}

fn banner(title: &str) {
    println!("\n\x1b[1m── {title}\x1b[0m");
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("lattice-skill-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}
