//! Non-UI process commands: execution, output, and command-specific failures.
//! Argument routing stays in main; terminal and debug-UI lifecycles stay outside.

use crate::startup::{self, home, report_catalog};
use lattice::daemon::Daemon;
use lattice::preset::{self, PresetConfig};
use lattice::{Kernel, KernelOptions, StreamHost, StreamTemplate};
use serde_json::json;
use std::path::PathBuf;

pub(super) fn recover(args: Vec<String>) -> std::io::Result<()> {
    lattice::recovery::run(&args, &mut std::io::stdout().lock())
}

pub(super) fn migrate(path: PathBuf) -> std::io::Result<()> {
    let report = lattice::kernel::migrate::migrate(&path)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub(super) fn verify(path: PathBuf) -> std::io::Result<()> {
    lattice::EventLog::verify_segmented_path(&path)?;
    println!("Ledger verified; no files changed.");
    Ok(())
}

/// Export the selected baseline, without turning installed parts into baseline.
pub(super) fn assembly() -> std::io::Result<()> {
    let cfg = PresetConfig::from_env();
    let document = preset::assembly_document(&cfg).map_err(std::io::Error::other)?;
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}

/// Move large legacy documents beside their ledger. With no paths, inspect all
/// supported conversation locations; one unreadable source must not stop others.
pub(super) fn compact(paths: Vec<String>) -> std::io::Result<()> {
    let ledgers: Vec<std::path::PathBuf> = if paths.is_empty() {
        lattice::ledgers::all(&home())
    } else {
        paths.into_iter().map(std::path::PathBuf::from).collect()
    };
    if ledgers.is_empty() {
        println!("no ledgers to compact");
        return Ok(());
    }

    let types = lattice::core_events::core_event_decls();
    let (mut before, mut after, mut documents) = (0u64, 0u64, 0usize);
    for ledger in &ledgers {
        match lattice::compact(ledger, &types) {
            Ok(report) => {
                before += report.bytes_before;
                after += report.bytes_after;
                documents += report.documents;
                if report.rewritten > 0 {
                    println!(
                        "{}: {} of {} events rewritten, {} documents, {} -> {} bytes",
                        ledger.file_name().unwrap_or_default().to_string_lossy(),
                        report.rewritten,
                        report.events,
                        report.documents,
                        report.bytes_before,
                        report.bytes_after
                    );
                }
            }
            Err(e) => eprintln!(
                "{}: skipped — {e}",
                ledger.file_name().unwrap_or_default().to_string_lossy()
            ),
        }
    }
    println!(
        "{} ledgers, {documents} documents, {before} -> {after} bytes",
        ledgers.len()
    );
    Ok(())
}

/// Fold referenced documents into a self-contained file for handing off.
pub(super) fn export(args: Vec<String>) -> std::io::Result<()> {
    let mut args = args.into_iter();
    let Some(ledger) = args.next().map(std::path::PathBuf::from) else {
        eprintln!("usage: lattice export <ledger.jsonl> [out.jsonl]");
        std::process::exit(2);
    };
    let out = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let stem = ledger.file_stem().unwrap_or_default().to_string_lossy();
            std::path::PathBuf::from(format!("{stem}-inlined.jsonl"))
        });

    // Never truncate an input ledger (including a hard-link alias) or an
    // earlier export before the source has even been read.
    let mut file = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)?,
    );
    let report = lattice::export(
        &ledger,
        &mut file,
        &lattice::core_events::core_event_decls(),
    )?;
    use std::io::Write;
    file.flush()?;

    println!(
        "{}: {} events, {} documents folded back in, {} bytes -> {}",
        ledger.file_name().unwrap_or_default().to_string_lossy(),
        report.events,
        report.inlined,
        report.bytes,
        out.display()
    );
    if !report.missing.is_empty() {
        // An incomplete export must name the references it could not inline.
        eprintln!(
            "warning: {} document(s) could not be read and are still references: {}",
            report.missing.len(),
            report.missing.join(", ")
        );
    }
    Ok(())
}

/// Rewriting, archiving and deleting are opt-in through the ledger policy.
pub(super) fn tidy() -> std::io::Result<()> {
    let Some(policy) = lattice::ledgers::Policy::from_preferences(&lattice::preferences::load())
    else {
        println!(
            "no \"ledger\" policy in preferences.json — nothing to do.\n\
             Add one to have old conversations tidied, e.g.\n  \
             \"ledger\": {{ \"compactAfterDays\": 7, \"archiveAfterDays\": 30 }}"
        );
        return Ok(());
    };
    let done = lattice::ledgers::sweep(
        &home(),
        policy,
        std::time::SystemTime::now(),
        &lattice::core_events::core_event_decls(),
    )?;
    println!(
        "{} compacted, {} archived, {} deleted, {} bytes freed",
        done.compacted, done.archived, done.deleted, done.bytes_freed
    );
    // Moving and deleting change what the index says.
    if done.archived + done.deleted > 0 {
        let counted = lattice::ledgers::rebuild(&home())?;
        println!("{counted} conversations reindexed");
    }
    Ok(())
}

/// Rebuild the derived conversation index from source records.
pub(super) fn index() -> std::io::Result<()> {
    let at = lattice::ledgers::dir(&home());
    let counted = lattice::ledgers::rebuild(&home())?;
    println!("{counted} conversations indexed in {}", at.display());
    Ok(())
}

/// Inspect the configured prompt using the same assembler that sends it. This
/// is deliberately not a launch: report configuration problems, but do not
/// require a model key or prepare a workspace merely to inspect the prompt.
pub(super) fn prompt() -> std::io::Result<()> {
    let cfg = PresetConfig::from_env();
    report_catalog(&cfg);
    let (registry, mut factories, assembly) = match lattice::preset::standard(&cfg) {
        Ok(built) => built,
        Err(e) => {
            eprintln!("the standard assembly did not build: {e}");
            std::process::exit(1);
        }
    };
    let kernel = match Kernel::start(
        &assembly,
        &registry,
        &mut factories,
        KernelOptions::default(),
    ) {
        Ok(kernel) => kernel,
        Err(e) => {
            eprintln!("the standard assembly did not start: {e}");
            std::process::exit(1);
        }
    };

    let system = lattice::components::context_gate::assemble_system(
        Some(&cfg.system),
        Some(lattice::preset::FRAGMENTS_HEADING),
        &kernel.prompt_fragments(),
        Some(&lattice::preset::house_rules()),
        Some(&cfg.model),
    )
    .unwrap_or_default();
    println!("{system}");

    // Show the effective wire setting and the model's declared effort rungs,
    // not only the text: an absent parameter and unknown rungs are meaningful.
    println!("─── settings ───");
    println!(
        "thinking  {}",
        match &cfg.thinking {
            Some(value) => value.to_string(),
            None => "(no parameter sent)".to_string(),
        }
    );
    let rungs = lattice::profile::effort_rungs(&cfg.model);
    println!(
        "rungs     {}",
        if rungs.is_empty() {
            format!("(none declared for {})", cfg.model)
        } else {
            rungs.join(" ")
        }
    );

    // Names, not full schemas, answer which tools the model can reach.
    let (mut resident, mut deferred): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for decl in kernel.tool_decls() {
        let Some(name) = decl["name"].as_str() else {
            continue;
        };
        if lattice::preset::DEFERRED_TOOLS.contains(&name) {
            deferred.push(name.to_string());
        } else {
            resident.push(name.to_string());
        }
    }
    resident.push(lattice::DEFERRED_DISPATCHER.to_string());
    resident.sort();
    println!(
        "\n\n─── in the schema ({}) ───\n{}",
        resident.len(),
        resident.join(" ")
    );
    println!(
        "─── found by {} ({}) ───\n{}",
        lattice::components::tool_catalog::FIND_TOOLS,
        deferred.len(),
        deferred.join(" ")
    );
    eprintln!("\n({} characters of system prompt)", system.chars().count());
    kernel.shutdown();
    Ok(())
}

/// Run the built-in implementation over the existing bridge protocol. Human
/// diagnostics belong on stderr; stdout belongs entirely to the bridge child.
pub(super) fn component(name: Option<String>) -> std::io::Result<()> {
    use lattice::components::{
        fs_tools, fs_watch, net_tools, shell_tools, skill_library, timer_tools,
    };
    let known = "skill-consumer, skill-installer, fs-reader, fs-writer, shell-tools, net-tools, timer-tools, fs-watch";
    let Some(name) = name else {
        eprintln!("lattice component <name> — run a builtin as a bridge child\navailable: {known}");
        std::process::exit(2);
    };
    match name.as_str() {
        "skill-consumer" => {
            lattice::run_bridge_child(|c| Box::new(skill_library::SkillConsumer::from_config(c)))
        }
        "skill-installer" => {
            lattice::run_bridge_child(|c| Box::new(skill_library::SkillInstaller::from_config(c)))
        }
        "fs-reader" => lattice::run_bridge_child(|c| Box::new(fs_tools::FsReader::from_config(c))),
        "fs-writer" => lattice::run_bridge_child(|c| Box::new(fs_tools::FsWriter::from_config(c))),
        "shell-tools" => {
            lattice::run_bridge_child(|c| Box::new(shell_tools::ShellTools::from_config(c)))
        }
        "net-tools" => lattice::run_bridge_child(|c| Box::new(net_tools::NetTools::from_config(c))),
        "timer-tools" => {
            lattice::run_bridge_child(|c| Box::new(timer_tools::TimerTools::from_config(c)))
        }
        "fs-watch" => lattice::run_bridge_child(|c| Box::new(fs_watch::FsWatch::from_config(c))),
        other => {
            eprintln!("lattice: unknown builtin component '{other}'\navailable: {known}");
            std::process::exit(2);
        }
    }
}

/// The daemon owns its separate stream-host construction and stop protocol;
/// do not substitute a terminal session builder or move factories across threads.
pub(super) fn daemon() -> std::io::Result<()> {
    let cfg = startup::config()?;

    let socket = std::env::var("LATTICE_SOCKET")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home().join(".lattice").join("daemon.sock"));
    std::fs::create_dir_all(socket.parent().unwrap())?;

    // Assembly contradictions must fail at boot, not at each attachment.
    if let Err(problem) = preset::standard(&cfg) {
        return Err(std::io::Error::other(problem));
    }

    let title = format!("{} @ {}", cfg.model, cfg.base_url);
    let key_env = cfg.key_env.clone();
    let note = json!({"host": "daemon", "model": cfg.model, "adapter": cfg.adapter});
    let daemon = Daemon::serve(&socket, move || {
        let (registry, factories, assembly) =
            preset::standard(&cfg).expect("validated at daemon startup");
        let template = StreamTemplate {
            registry,
            factories,
            assembly,
        };
        StreamHost::new([("chat".to_string(), template)].into())
            .withholding(lattice::models::key_env_names(&key_env))
            .redacting(lattice::models::key_values(&key_env))
            .noting(note.clone())
            .with_ledger_path(|stream| {
                // A client-chosen stream identifier is not necessarily a safe
                // filename. Invalid names stay memory-only, not repaired names.
                let named = !stream.is_empty()
                    && stream.len() <= 128
                    && stream
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                if !named {
                    eprintln!("warning: stream id {stream:?} is not a name — kept in memory only");
                    return None;
                }
                std::env::var("HOME").ok().map(|h| {
                    let dir = std::path::PathBuf::from(h).join(".lattice").join("streams");
                    std::fs::create_dir_all(&dir).ok();
                    lattice::ledgers::named_path(&dir, stream)
                })
            })
    })?;
    println!("lattice daemon · {title}");
    println!("socket: {}", daemon.socket_path().display());
    println!("connect a client (clients/ink, or any language); Ctrl-C to stop");

    let (tx, rx) = std::sync::mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })
    .ok();
    let _ = rx.recv();
    daemon.stop();
    println!("\nstopped");
    Ok(())
}
