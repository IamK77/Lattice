//! The standard Lattice assembly, in one place.
//!
//! Every frontend — the in-process ratatui TUI, the daemon serving remote
//! clients — runs the SAME agent. This module is that agent's recipe
//! (which components, which wires, which config); a binary just picks the
//! transport (in-memory channel vs socket). Keeping the recipe here means
//! "catch the TUI up to the daemon" is not a thing that can drift — they are
//! literally the same assembly.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::components::{
    anthropic_model, context_gate, environment, fs_tools, fs_watch, minimal_loop, net_tools,
    openai_model, project_rules, responses_model, scripted_model, search_tools, shell_tools,
    silent_ui, skill_library, subagent, timer_tools, tool_catalog, trust_policy, web_search,
    workshop_sink,
};
use crate::{AssemblyManifest, ComponentInstance, ComponentManifest, Factory, Wire};

/// The instance name of the main model — the one whose live stream a frontend
/// surfaces. The condense model ("cmodel") is a separate background instance.
pub const MAIN_MODEL: &str = "model";
/// The instance name of the background condenser. It rides the same endpoint
/// and the same key as the main model, so whatever happens to one happens to
/// both — condensing this conversation on a provider the conversation has
/// never been sent to would be a leak, not a setting.
pub const CONDENSE_MODEL: &str = "cmodel";

/// Which brain, where it lives, and where the agent may work.
#[derive(Clone)]
pub struct PresetConfig {
    /// "openai" | "anthropic" | "scripted"
    pub adapter: String,
    pub model: String,
    pub base_url: String,
    pub key_env: String,
    /// Directory to confine the fs/search/shell tools to. `None` — the
    /// default — leaves them unconfined: they work wherever the process was
    /// started, and anywhere else the paths point. Confinement is opt-in
    /// because it was never the boundary it looked like while `Run` sat on
    /// the same wire declaring `executes` and `*`.
    pub workspace: Option<String>,
    /// Fallback context window, for a model that ships no profile. The
    /// profile is authoritative when there is one — the window is a property
    /// of the model, and now that the model can be changed mid-conversation
    /// it has to travel with it rather than sit in the startup config.
    pub context_window: u64,
    /// Fallback native usage field counting input tokens, same terms.
    pub usage_input_field: String,
    /// The chosen model's own profile, when its catalog entry carried one.
    /// Kept here because this struct is what a host passes around, and a
    /// profile dropped on the way in would leave a hand-added model budgeted
    /// against the fallback window above — the exact hole this field closes.
    pub profile: Option<Value>,
    /// Anything wrong with the user's hand-written model catalog. Carried as
    /// data rather than printed here, because a library that prints decides
    /// for its caller where warnings go — the TUI has a terminal to protect,
    /// the daemon has a log.
    pub catalog_problems: Vec<String>,
    /// Base system prompt (component fragments are appended by the gate)
    pub system: String,
    /// The neutral thinking knob for the MAIN model, as the adapters read it:
    /// `false` = off, an effort string = on at that effort, None = say nothing
    /// (the provider's own default; also the only safe value against an
    /// endpoint that rejects an unknown parameter). The condense model never
    /// thinks — condensing is reformatting, not reasoning — so whenever this
    /// is set at all, that instance is explicitly told to stop.
    pub thinking: Option<Value>,
    /// For adapter "scripted": the model's script (keyless, deterministic)
    pub scripted: Option<Value>,
    /// The assembly overlay to merge at startup (the persistence of hot
    /// installs; see src/overlay.rs). None = no overlay — tests and library
    /// consumers stay hermetic unless they opt in.
    pub overlay: Option<PathBuf>,
    /// Complete user-selected baseline. Invalid documents fail startup;
    /// unlike the additive install overlay this is never silently skipped.
    pub assembly: Option<PathBuf>,
}

impl PresetConfig {
    /// Read the standard config from the environment: the same knobs the chat
    /// example uses (LATTICE_ADAPTER / LATTICE_MODEL / LATTICE_BASE_URL /
    /// LATTICE_API_KEY_ENV), plus LATTICE_SCRIPTED for a keyless run.
    pub fn from_env() -> Self {
        if std::env::var("LATTICE_SCRIPTED").is_ok() {
            return Self {
                adapter: "scripted".to_string(),
                model: "scripted".to_string(),
                base_url: String::new(),
                key_env: String::new(),
                profile: None,
                catalog_problems: Vec::new(),
                workspace: workspace_dir(),
                context_window: 64000,
                usage_input_field: "input_tokens".to_string(),
                system: default_system(),
                scripted: Some(json!({"script": [
                    {"status": "ok", "text": "scripted reply 1"},
                    {"status": "ok", "text": "scripted reply 2"},
                    {"status": "ok", "text": "scripted reply 3"},
                ]})),
                thinking: None,
                overlay: overlay_from_env(),
                assembly: std::env::var_os("LATTICE_ASSEMBLY")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from),
            };
        }
        let dialect = std::env::var("LATTICE_ADAPTER").unwrap_or_else(|_| "openai".into());
        let anthropic = dialect == "anthropic";
        let responses = dialect == "responses";
        let base_url = std::env::var("LATTICE_BASE_URL").unwrap_or_else(|_| {
            if responses {
                String::new()
            } else if anthropic {
                "https://api.deepseek.com/anthropic".to_string()
            } else {
                "https://api.deepseek.com".to_string()
            }
        });
        // Only what was actually said. There used to be a guess here — no
        // DeepSeek key in the environment, so assume Anthropic — and it
        // produced a configuration that contradicted itself: the OpenAI
        // dialect, DeepSeek's endpoint, and a variable named for Anthropic.
        // A person following the resulting message set a key that would have
        // been sent to the wrong provider. Nothing is guessed now; when
        // nothing was said, the catalog decides, and a catalog entry names
        // its endpoint and its key together.
        let key_env = std::env::var("LATTICE_API_KEY_ENV").unwrap_or_default();
        let model = std::env::var("LATTICE_MODEL").unwrap_or_else(|_| {
            if responses {
                String::new()
            } else {
                "deepseek-v4-flash".to_string()
            }
        });
        let from_env = crate::models::Entry {
            id: model.clone(),
            adapter: dialect,
            model,
            base_url,
            key_env,
            profile: None,
        };
        // Read once, for both the choice and the complaints: a hand-written
        // catalog is the one file here nothing else checks, so whatever is
        // wrong with it travels with the config to whoever can say it out loud.
        let (catalog, mut catalog_problems) = crate::models::load_reported();
        let saved = crate::preferences::get("model");
        let entry = model_from_env_or_preference(
            said_about_the_model(),
            from_env,
            &catalog,
            saved.as_ref().and_then(Value::as_str),
            &mut catalog_problems,
        );
        Self {
            adapter: entry.adapter,
            system: default_system(),
            model: entry.model,
            base_url: entry.base_url,
            key_env: entry.key_env,
            profile: entry.profile,
            catalog_problems,
            workspace: workspace_dir(),
            context_window: 1_000_000,
            usage_input_field: if anthropic {
                "input_tokens"
            } else {
                "prompt_tokens"
            }
            .to_string(),
            scripted: None,
            thinking: thinking_from_env(),
            overlay: overlay_from_env(),
            assembly: std::env::var_os("LATTICE_ASSEMBLY")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// The main model's thinking setting, strongest source first:
///
/// 1. `LATTICE_THINKING` — typed on THIS launch, so it wins. It is how a
///    person says "not today" without disturbing what they usually want.
/// 2. the `thinking` preference — their standing choice, which is why it
///    lives in the user directory rather than in a conversation's ledger: it
///    should still be in force in a conversation that did not exist yet when
///    they made it.
/// 3. the product default: the main model thinks at "high".
///
/// The empty string means "say nothing at all", for an endpoint that would
/// reject the parameter — which is not the same as "off" and must stay
/// expressible.
/// Did this launch say anything about which model to use?
///
/// Any one of the four is enough. They describe one model between them, so
/// honouring a saved choice while `LATTICE_BASE_URL` points somewhere else
/// would produce a model nobody asked for: one variable's endpoint with
/// another's name.
fn said_about_the_model() -> bool {
    [
        "LATTICE_ADAPTER",
        "LATTICE_MODEL",
        "LATTICE_BASE_URL",
        "LATTICE_API_KEY_ENV",
    ]
    .iter()
    .any(|name| std::env::var(name).is_ok())
}

/// Which model wins: what was said on THIS launch, then the saved choice, then
/// the built-in default — the same order the thinking setting follows, for the
/// same reason. Something typed a second ago outranks something chosen last
/// week, and nothing saved can silently overrule an instruction just given.
///
/// A saved name the catalog no longer has is ignored rather than an error: a
/// person who deletes an entry should get the default back, not a program that
/// will not start.
fn model_from_env_or_preference(
    said_this_launch: bool,
    from_env: crate::models::Entry,
    catalog: &[crate::models::Entry],
    saved: Option<&str>,
    problems: &mut Vec<String>,
) -> crate::models::Entry {
    if said_this_launch {
        return from_env;
    }
    // The saved choice decides — if it can still be reached. Two ways it
    // might not be: the entry is gone from the catalog, or it is still there
    // with its key no longer set. Both are the same situation for the person
    // in front of it, and neither should stop the program: deleting an entry
    // gives the default back, and so should removing its key.
    //
    // Said out loud rather than switched quietly, because which model is
    // running is not a detail — it decides what the answers cost and who
    // sees the conversation.
    if let Some(name) = saved {
        match catalog.iter().find(|entry| entry.id == name) {
            Some(entry) if entry.key_present() => return entry.clone(),
            Some(entry) => {
                let reachable = catalog.iter().find(|e| e.key_present());
                problems.push(match reachable {
                    Some(instead) => format!(
                        "preferred model {name:?} has no key ({} is not set); \
                         starting on {:?} instead",
                        entry.key_env, instead.id
                    ),
                    None => format!(
                        "preferred model {name:?} has no key ({} is not set)",
                        entry.key_env
                    ),
                });
            }
            // A name the catalog no longer has stays quiet. Removing an
            // entry is something the person did on purpose, so the default
            // coming back is what they expect. An entry that is still there
            // with its key gone is the opposite: it looks fine, and they have
            // every reason to think it is the one running.
            None => {}
        }
    }
    // Nothing said, nothing saved: the first model this installation can
    // ACTUALLY reach — configured, with its key where it says it is. A model
    // whose key is absent is one that fails on the first turn, so offering it
    // as the default only moves the discovery later.
    catalog
        .iter()
        .find(|entry| entry.key_present())
        .or_else(|| catalog.first())
        .cloned()
        .unwrap_or(from_env)
}

fn thinking_from_env() -> Option<Value> {
    let said = std::env::var("LATTICE_THINKING").ok();
    crate::preferences::resolve_thinking(said.as_deref(), crate::preferences::get("thinking"))
}

/// LATTICE_OVERLAY: unset = the user-directory default; set to a path = that
/// path; set to the empty string = no overlay at all.
pub(crate) fn overlay_from_env() -> Option<PathBuf> {
    match std::env::var("LATTICE_OVERLAY") {
        Ok(path) if path.is_empty() => None,
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => Some(crate::overlay::default_path()),
    }
}

/// The PRODUCT layer of the system prompt: who this agent is and how it works.
/// Lattice itself is a general runtime, and the facts about the runtime are not
/// here — each component states the one it brings (the gate explains the gate,
/// the adapter explains an interrupted result), so an assembly that replaces
/// this string swaps the character without losing any of them.
pub const FRAGMENTS_HEADING: &str = "# Your setup";

/// The tools kept OUT of the model's schema, reachable by searching the
/// catalogue and calling through the resident dispatcher.
///
/// Which tools are common is a judgement about this product, not about the
/// components that provide them, so the list lives here. Read, write, edit,
/// run, find and grep stay resident: they are what a coding agent reaches for
/// in almost every turn, and making it search for those would trade a real
/// cost for an imagined one. The rest are occasional — and the agent can
/// install more of its own at any time, which is exactly why the list must not
/// be allowed to grow without bound inside the cached prefix.
pub const DEFERRED_TOOLS: &[&str] = &[
    "CancelExpert",
    "Browser",
    "Desktop",
    "Ls",
    "Fetch",
    "WebSearch",
    "Schedule",
    "Unschedule",
    "Watch",
    "Unwatch",
    "LoadSkill",
    "InstallSkill",
    "InstallComponent",
    "InstallComponentFrom",
    "UninstallComponent",
];

/// The product prompt, `{model}` still in it.
///
/// The placeholder is filled in by whoever assembles the prompt — the context
/// gate — rather than here, because the model is no longer decided once at
/// startup. A name substituted at assembly time would still be the old one
/// after `/model`, and an agent that cannot say what it is running on will
/// guess: guessed from a file called CLAUDE.md it answers "Claude" while
/// running on something else entirely, which is the failure this placeholder
/// exists to prevent.
fn default_system() -> String {
    crate::prompts::before_setup(&sections())
}

/// The prompt's sections: what this build ships, under whatever the person put
/// in `~/.lattice/prompts` (see [`crate::prompts::load`]).
fn sections() -> Vec<crate::prompts::Section> {
    crate::prompts::load(std::env::var_os("HOME").map(PathBuf::from).as_deref())
}

/// The sections that come AFTER the components' fragments. Kept a function
/// rather than a constant for the same reason as the opening: a person's own
/// files are read at startup, not at compile time.
pub fn house_rules() -> String {
    crate::prompts::after_setup(&sections())
}

const CONDENSER_SYSTEM: &str =
    "You condense conversation history. Reply with ONLY a compact summary in exactly four \
     sections: Done: (what was completed) State: (where things stand) Open: (unresolved \
     items) Facts: (key paths, decisions, numbers, identifiers). No preamble.";

/// Which search service answers `WebSearch`, from the environment. Absent =
/// Brave, which is what most people will have a key for; the tool says plainly
/// which key is missing when it has none, so an unconfigured search fails with
/// an instruction rather than a shrug.
fn search_config() -> Option<Value> {
    let mut config = json!({});
    if let Ok(backend) = std::env::var("LATTICE_SEARCH") {
        config["backend"] = json!(backend);
    }
    if let Ok(endpoint) = std::env::var("LATTICE_SEARCH_URL") {
        config["endpoint"] = json!(endpoint);
    }
    if let Ok(key_env) = std::env::var("LATTICE_SEARCH_KEY_ENV") {
        config["apiKeyEnv"] = json!(key_env);
    }
    Some(config)
}

fn workspace_dir() -> Option<String> {
    std::env::var("LATTICE_WORKSPACE").ok()
}

/// What the gate needs to know about the model it is budgeting for: how big
/// its window is, and what its provider calls the usage numbers.
///
/// The model's own profile is authoritative; the startup config stands in only
/// where no profile ships. Both fields used to be written here as constants,
/// which was survivable while the model could not change — one wrong number
/// for one model, fixed by editing this file. It stops being survivable the
/// moment `/model` exists.
///
/// It also fixes something that was already wrong: only `input` was ever
/// passed, so the gate fell back to Anthropic's name for the CACHE-READ count
/// while talking to DeepSeek, read a field that endpoint never sends, and
/// concluded on every single call that the prompt cache was cold. Everything
/// waiting for a cold cache — promoting a newly installed tool, adopting a
/// changed system prompt — therefore happened immediately, every time, which
/// is precisely what deferring them was meant to avoid.
pub fn model_profile(cfg: &PresetConfig) -> Value {
    let entry = running_entry(cfg);
    let window = entry.context_window().unwrap_or(cfg.context_window);
    let mut fields = entry.usage_fields();
    if fields.is_empty() {
        fields.insert("input".to_string(), json!(cfg.usage_input_field));
    }
    json!({"contextWindow": window, "usageFields": fields})
}

/// The catalog entry describing what this config actually runs.
///
/// The startup config and a catalog entry say the same four things in
/// different words; this is the translation, and it exists so that the model
/// running now and a model chosen from the list are the same kind of thing and
/// go through the same code.
pub fn running_entry(cfg: &PresetConfig) -> crate::models::Entry {
    crate::models::Entry {
        // A name for the row it will occupy in the list. The model's own
        // identifier is the only short name we have that is certainly about
        // this model rather than about somebody's preferences.
        id: cfg.model.clone(),
        adapter: cfg.adapter.clone(),
        model: cfg.model.clone(),
        base_url: cfg.base_url.clone(),
        key_env: cfg.key_env.clone(),
        profile: cfg.profile.clone(),
    }
}

/// The MAIN model instance's config, from a catalog entry and the thinking
/// setting in force.
///
/// Shared by startup and by `/model`, because the two must agree: a model
/// swapped in mid-conversation that got its effort rungs or its endpoint from
/// a second, slightly different copy of this would misbehave in a way nothing
/// on the ledger would explain.
pub fn main_model_config(entry: &crate::models::Entry, thinking: Option<&Value>) -> Value {
    let mut config = json!({
        "model": entry.model,
        "baseUrl": entry.base_url,
        "apiKeyEnv": entry.key_env,
        // Host metadata: retain the full selected entry, not a profile rebuilt
        // from the subset of capabilities the adapter happens to consume.
        "entryId": entry.id,
        "profile": entry.profile,
    });
    if entry.adapter == "responses" && entry.native_web_search() {
        config["nativeWebSearch"] = json!(true);
    }
    if entry.adapter == "responses" && entry.native_image_generation() {
        config["nativeImageGeneration"] = json!(true);
    }
    if let Some(thinking) = thinking {
        config["thinking"] = thinking.clone();
    }
    // Which effort rungs this model actually has. From its profile — the one
    // thing about a model that cannot be guessed from the dialect, because one
    // dialect serves providers whose ladders differ, and one provider ships
    // models whose ladders differ from each other.
    let rungs = entry.effort_rungs();
    if !rungs.is_empty() {
        config["effort"] = json!(rungs);
    }
    // How much it may write. Left unsaid, every adapter falls back to 4096 —
    // and with thinking on, where the thought is charged to the same budget,
    // that ceiling truncates a long answer mid-sentence.
    if let Some(most) = entry.max_output() {
        config["maxTokens"] = json!(most);
    }
    config
}

/// The background condenser instance's config for the same entry. It rides the
/// same endpoint and the same key: condensing this conversation on a different
/// provider would send the whole conversation somewhere it has never been.
///
/// Thinking is settled by the caller — condensing is reformatting, not
/// reasoning, so it is turned off whenever the main model says anything at all
/// about thinking, and left unsaid otherwise, because an endpoint that rejects
/// the parameter must not meet it here either.
pub fn condenser_config(entry: &crate::models::Entry) -> Value {
    json!({
        "model": entry.model,
        "baseUrl": entry.base_url,
        "apiKeyEnv": entry.key_env,
        "maxTokens": 1024,
        "compactionProtocol": if entry.adapter == "responses" { json!("trigger") } else { Value::Null },
        "system": if entry.adapter == "responses" { Value::Null } else { json!(CONDENSER_SYSTEM) },
        // Same component as the main brain, so it carries the same fragment —
        // and every fragment lands in the MAIN model's system. Null suppresses
        // it, or the interruption passage arrives twice.
        "prompt": null,
    })
}

/// Which adapter short name a model component goes by — the inverse of
/// [`brain_name`], so the running assembly can be read back as a catalog entry.
pub fn adapter_of(component: &str) -> &'static str {
    match component {
        c if c == scripted_model::NAME => "scripted",
        c if c == anthropic_model::NAME => "anthropic",
        c if c == responses_model::NAME => "responses",
        _ => "openai",
    }
}

pub fn brain_name(adapter: &str) -> &'static str {
    match adapter {
        "scripted" => scripted_model::NAME,
        "anthropic" => anthropic_model::NAME,
        "responses" => responses_model::NAME,
        _ => openai_model::NAME,
    }
}

/// What `standard` hands a host: registry, factories, wiring manifest.
pub type StandardAssembly = (
    HashMap<String, ComponentManifest>,
    HashMap<String, Factory>,
    AssemblyManifest,
);

/// Build the standard assembly: registry, factories, and the wiring manifest.
/// The daemon wraps these in a StreamTemplate; the in-process TUI hands them
/// straight to `Kernel::start`.
///
/// Errors are the ASSEMBLER's rules — contradictions no single component or
/// the kernel can see (the kernel does not know what "trust" means). Today:
/// a trust gate with stance "ask" whose questions nobody wired in can answer.
/// Fail fast beats a turn hanging on a question with no listener.
pub fn standard(cfg: &PresetConfig) -> Result<StandardAssembly, String> {
    standard_at_depth(cfg, 0)
}

/// Export the selected baseline, excluding installation additions. Re-importing
/// it with the same overlay must not turn installs into protected baseline slots.
pub fn assembly_document(cfg: &PresetConfig) -> Result<Value, String> {
    let mut baseline = cfg.clone();
    baseline.overlay = None;
    baseline.assembly = None;
    let (builtins, _, _) = standard(&baseline)?;
    let names = builtins.into_keys().collect();
    baseline.assembly = cfg.assembly.clone();
    let (registry, _, assembly) = standard(&baseline)?;
    Ok(crate::product_assembly::document(
        &registry, &assembly, &names,
    ))
}

fn validate_model_endpoint(adapter: &str, model: &str, base_url: &str) -> Result<(), String> {
    if !["openai", "anthropic", "responses", "scripted"].contains(&adapter) {
        return Err(format!("unknown model adapter: {adapter}"));
    }
    if adapter == "responses" && (model.trim().is_empty() || base_url.trim().is_empty()) {
        return Err("Responses requires an explicit model and base URL".into());
    }
    Ok(())
}

/// The same assembly, built for a stream that is already `depth` subagents
/// deep. The only thing depth changes is whether `Task` is offered onward —
/// everything else about a subagent's stream is an ordinary stream, which is
/// why there is one assembly and not two.
pub fn standard_at_depth(cfg: &PresetConfig, depth: u64) -> Result<StandardAssembly, String> {
    validate_model_endpoint(&cfg.adapter, &cfg.model, &cfg.base_url)?;
    let brain = brain_name(&cfg.adapter);

    let registry: HashMap<String, ComponentManifest> = [
        (silent_ui::NAME, silent_ui::manifest()),
        (environment::NAME, environment::manifest()),
        (project_rules::NAME, project_rules::manifest()),
        (tool_catalog::NAME, tool_catalog::manifest()),
        (minimal_loop::NAME, minimal_loop::manifest()),
        (context_gate::NAME, context_gate::manifest()),
        // EVERY model adapter this build carries, not only the one starting
        // today. The registry is what this binary CAN assemble; the assembly
        // below is what it does assemble. Keeping the others here is what
        // lets `/model` swap dialects without a restart — and it costs
        // nothing, because a component with no instance is a manifest sitting
        // in a map.
        (openai_model::NAME, openai_model::manifest()),
        (responses_model::NAME, responses_model::manifest()),
        (anthropic_model::NAME, anthropic_model::manifest()),
        (scripted_model::NAME, scripted_model::manifest()),
        (fs_tools::READER, fs_tools::reader_manifest()),
        (fs_tools::WRITER, fs_tools::writer_manifest()),
        (
            crate::components::browser_tools::NAME,
            crate::components::browser_tools::manifest(),
        ),
        (
            crate::components::desktop_tools::NAME,
            crate::components::desktop_tools::manifest(),
        ),
        (search_tools::NAME, search_tools::manifest()),
        (
            crate::components::code_tools::NAME,
            crate::components::code_tools::manifest(),
        ),
        (shell_tools::NAME, shell_tools::manifest()),
        (net_tools::NAME, net_tools::manifest()),
        (web_search::NAME, web_search::manifest()),
        (timer_tools::NAME, timer_tools::manifest()),
        (subagent::NAME, subagent::manifest()),
        (fs_watch::NAME, fs_watch::manifest()),
        (workshop_sink::NAME, workshop_sink::manifest()),
        (skill_library::CONSUMER, skill_library::consumer_manifest()),
        (
            skill_library::INSTALLER,
            skill_library::installer_manifest(),
        ),
        (trust_policy::NAME, trust_policy::manifest()),
    ]
    .into_iter()
    .map(|(name, manifest)| (name.to_string(), manifest))
    .collect();

    let mut factories: HashMap<String, Factory> = HashMap::new();
    factories.insert(
        silent_ui::NAME.to_string(),
        Box::new(|_| Box::new(silent_ui::SilentUi::new(std::sync::Arc::default()))),
    );
    factories.insert(
        minimal_loop::NAME.to_string(),
        Box::new(|c| Box::new(minimal_loop::MinimalLoop::from_config(c))),
    );
    factories.insert(
        context_gate::NAME.to_string(),
        Box::new(|c| Box::new(context_gate::ContextGate::from_config(c))),
    );
    // One factory per adapter, for the same reason the registry holds all
    // three: the kernel builds a replacement from these, so an adapter with no
    // factory could be named but never started.
    factories.insert(
        scripted_model::NAME.to_string(),
        Box::new(|c| Box::new(scripted_model::ScriptedModel::from_config(c))),
    );
    factories.insert(
        anthropic_model::NAME.to_string(),
        Box::new(|c| Box::new(anthropic_model::AnthropicModel::from_config(c))),
    );
    factories.insert(
        openai_model::NAME.to_string(),
        Box::new(|c| Box::new(openai_model::OpenAiModel::from_config(c))),
    );
    factories.insert(
        responses_model::NAME.to_string(),
        Box::new(|c| Box::new(responses_model::ResponsesModel::from_config(c))),
    );
    factories.insert(
        environment::NAME.to_string(),
        Box::new(|c| Box::new(environment::Environment::from_config(c))),
    );
    factories.insert(
        project_rules::NAME.to_string(),
        Box::new(|c| Box::new(project_rules::ProjectRules::from_config(c))),
    );
    factories.insert(
        tool_catalog::NAME.to_string(),
        Box::new(|c| Box::new(tool_catalog::ToolCatalog::from_config(c))),
    );
    factories.insert(
        crate::components::browser_tools::NAME.to_string(),
        Box::new(|c| {
            Box::new(crate::components::browser_tools::BrowserTools::from_config(
                c,
            ))
        }),
    );
    factories.insert(
        crate::components::desktop_tools::NAME.to_string(),
        Box::new(|c| {
            Box::new(crate::components::desktop_tools::DesktopTools::from_config(
                c,
            ))
        }),
    );
    factories.insert(
        fs_tools::READER.to_string(),
        Box::new(|c| Box::new(fs_tools::FsReader::from_config(c))),
    );
    factories.insert(
        fs_tools::WRITER.to_string(),
        Box::new(|c| Box::new(fs_tools::FsWriter::from_config(c))),
    );
    factories.insert(
        search_tools::NAME.to_string(),
        Box::new(|c| Box::new(search_tools::SearchTools::from_config(c))),
    );
    factories.insert(
        crate::components::code_tools::NAME.to_string(),
        Box::new(|c| Box::new(crate::components::code_tools::CodeTools::from_config(c))),
    );
    factories.insert(
        shell_tools::NAME.to_string(),
        Box::new(|c| Box::new(shell_tools::ShellTools::from_config(c))),
    );
    factories.insert(
        net_tools::NAME.to_string(),
        Box::new(|c| Box::new(net_tools::NetTools::from_config(c))),
    );
    factories.insert(
        web_search::NAME.to_string(),
        Box::new(|c| Box::new(web_search::WebSearch::from_config(c))),
    );
    factories.insert(
        timer_tools::NAME.to_string(),
        Box::new(|c| Box::new(timer_tools::TimerTools::from_config(c))),
    );
    factories.insert(
        subagent::NAME.to_string(),
        Box::new(|c| Box::new(subagent::Subagent::from_config(c))),
    );
    factories.insert(
        skill_library::CONSUMER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillConsumer::from_config(c))),
    );
    factories.insert(
        skill_library::INSTALLER.to_string(),
        Box::new(|c| Box::new(skill_library::SkillInstaller::from_config(c))),
    );
    factories.insert(
        fs_watch::NAME.to_string(),
        Box::new(|c| Box::new(fs_watch::FsWatch::from_config(c))),
    );
    factories.insert(
        trust_policy::NAME.to_string(),
        Box::new(|c| Box::new(trust_policy::TrustPolicy::from_config(c))),
    );
    factories.insert(
        workshop_sink::NAME.to_string(),
        Box::new(|_| Box::new(workshop_sink::WorkshopSink)),
    );

    let scripted = cfg.adapter == "scripted";
    let entry = running_entry(cfg);
    let model_config = if scripted {
        cfg.scripted
            .clone()
            .unwrap_or_else(|| json!({"script": []}))
    } else {
        main_model_config(&entry, cfg.thinking.as_ref())
    };
    let cmodel_config = if scripted {
        json!({"script": []})
    } else {
        let mut config = condenser_config(&entry);
        if cfg.thinking.is_some() {
            config["thinking"] = json!(false);
        }
        config
    };
    // No profile in scripted mode = the gate stays inert for compaction (a
    // keyless smoke run has no real usage to weigh) — but it still carries the
    // system prompt, so a scripted run assembles the SAME prompt the product
    // does, and the smoke test covers it. Real mode adds the window + condense.
    let gate_config = if scripted {
        Some(json!({
            "system": cfg.system,
            "modelName": cfg.model,
            "fragmentsHeading": FRAGMENTS_HEADING,
            "systemTail": house_rules(),
            "deferTools": DEFERRED_TOOLS,
        }))
    } else {
        Some(json!({
            "profile": model_profile(cfg),
            "modelName": cfg.model,
            "condense": true,
            "nativeCompaction": cfg.adapter == "responses",
            "nativeTarget": {"model": cfg.model, "baseUrl": cfg.base_url},
            "system": cfg.system,
            "fragmentsHeading": FRAGMENTS_HEADING,
            "systemTail": house_rules(),
            "deferTools": DEFERRED_TOOLS,
        }))
    };

    // The resident skill listing is computed here, at assembly time, and
    // carried as the instance's prompt override; a restart re-scans. No
    // skills = no fragment = zero resident cost.
    let skill_dirs = skill_library::default_dirs();
    let mut skills_config = json!({"dirs": skill_dirs});
    if let Some(listing) = skill_library::listing_prompt(&skill_dirs) {
        skills_config["prompt"] = json!(listing);
    }

    // Each slot carries its bound: swapping in a component that does not
    // claim the required profile fails inspection at boot, plainly, instead
    // of misbehaving at runtime. `loop` and `workshop` have no profile yet.
    // The confinement config, under whichever key that component names it —
    // absent when there is no workspace, which is what leaves the tools open.
    let confine = |key: &str| cfg.workspace.as_ref().map(|dir| json!({ key: dir }));
    // `Run` needs one thing more than a directory: its fragment has to NAME
    // that directory, because a confined shell starts somewhere the
    // environment fragment never mentions.
    let shell_config = cfg
        .workspace
        .as_ref()
        .map(|dir| json!({"cwd": dir, "prompt": shell_tools::prompt_for(dir)}));
    let instance = |component: &str, config: Option<Value>, requires: &[&str]| ComponentInstance {
        component: component.to_string(),
        config,
        requires: requires.iter().map(|p| p.to_string()).collect(),
    };
    const TOOLS: &[&str] = &["tool-provider"];
    let assembly = AssemblyManifest {
        instances: [
            (
                "ui".to_string(),
                instance(silent_ui::NAME, None, &["frontend", "frontend-authorize"]),
            ),
            (
                "loop".to_string(),
                instance(minimal_loop::NAME, Some(json!({"model": cfg.model})), &[]),
            ),
            (
                MAIN_MODEL.to_string(),
                instance(brain, Some(model_config), &["model-adapter"]),
            ),
            (
                "ctx".to_string(),
                instance(context_gate::NAME, gate_config, &["context-manager"]),
            ),
            (
                "cmodel".to_string(),
                instance(brain, Some(cmodel_config), &["model-adapter"]),
            ),
            (
                // Named so its fragment sorts among the other tool notes,
                // after "where you are" and before the project's own rules
                "tool-catalog".to_string(),
                instance(
                    tool_catalog::NAME,
                    Some(json!({"deferred": DEFERRED_TOOLS})),
                    TOOLS,
                ),
            ),
            (
                // Sorts first among the fragments, so the model reads where it
                // is standing before it reads what it can do
                "env".to_string(),
                instance(environment::NAME, None, &[]),
            ),
            (
                // Named to sort LAST among the fragments (they are ordered by
                // instance name). The project's rules are a document, with its
                // own headings, and anything printed after it would read as
                // belonging to its final section. Last also puts it beside the
                // house rules, which is what it is.
                "zz-project-rules".to_string(),
                instance(project_rules::NAME, None, &[]),
            ),
            (
                "web-browser".to_string(),
                instance(crate::components::browser_tools::NAME, None, TOOLS),
            ),
            (
                "local-desktop".to_string(),
                instance(crate::components::desktop_tools::NAME, Some(json!({
                    "hostProtection": {"applications": [
                        "com.apple.Terminal", "com.googlecode.iterm2", "com.mitchellh.ghostty",
                        "dev.warp.Warp-Stable", "com.apple.ActivityMonitor", "com.apple.systempreferences",
                        "com.follow.clash", "io.github.clash-verge-rev.clash-verge-rev",
                        "com.apple.controlcenter", "com.apple.systemuiserver", "com.trycua.driver"
                    ]}
                })), TOOLS),
            ),
            (
                "fs".to_string(),
                instance(fs_tools::READER, confine("root"), TOOLS),
            ),
            (
                "fs-write".to_string(),
                instance(fs_tools::WRITER, confine("root"), TOOLS),
            ),
            (
                // Looking costs only reading here — the same questions asked
                // through `Run` would cost the right to execute programs
                "search".to_string(),
                instance(search_tools::NAME, confine("root"), TOOLS),
            ),
            (
                "code".to_string(),
                instance(crate::components::code_tools::NAME, None, TOOLS),
            ),
            (
                "shell".to_string(),
                instance(shell_tools::NAME, shell_config, TOOLS),
            ),
            ("net".to_string(), instance(net_tools::NAME, None, TOOLS)),
            (
                // The search service is configuration, not code: LATTICE_SEARCH
                // picks one, and the key is read from the environment because a
                // key in an assembly file is a key published.
                "search-web".to_string(),
                instance(web_search::NAME, search_config(), TOOLS),
            ),
            (
                "timer".to_string(),
                instance(timer_tools::NAME, None, TOOLS),
            ),
            ("watch".to_string(), instance(fs_watch::NAME, None, TOOLS)),
            // The in-stream half of handing work to an expert. `depth` is what a
            // subagent's own assembly carries to refuse starting another: one
            // sentence could otherwise open a tree, and neither the bill nor
            // the interrupt path is ready to catch one.
            (
                "subagent".to_string(),
                instance(
                    subagent::NAME,
                    Some(json!({"depth": depth, "experts": expert_roster()})),
                    TOOLS,
                ),
            ),
            // The frontends can answer the authorization pair (ui.answer →
            // trust.answer below), so the gate ASKS: an ungranted admission
            // puts a card in front of the human and the turn waits. Headless
            // runs that want refusal instead: config {"stance": "deny"}.
            (
                "trust".to_string(),
                instance(
                    trust_policy::NAME,
                    Some(json!({"stance": "ask"})),
                    &["policy"],
                ),
            ),
            (
                "workshop".to_string(),
                instance(workshop_sink::NAME, None, &[]),
            ),
            (
                "skills".to_string(),
                instance(skill_library::CONSUMER, Some(skills_config), TOOLS),
            ),
            (
                "skill-installer".to_string(),
                instance(skill_library::INSTALLER, Some(json!({"dirs": skill_dirs})), TOOLS),
            ),
        ]
        .into(),
        wires: standard_wires(),
    };

    let mut registry = registry;
    let mut assembly = assembly;
    if let Some(path) = &cfg.assembly {
        if cfg.overlay.as_ref().is_some_and(|overlay| {
            path == overlay
                || std::fs::canonicalize(path)
                    .ok()
                    .zip(std::fs::canonicalize(overlay).ok())
                    .is_some_and(|(a, b)| a == b)
        }) {
            return Err("complete assembly and installation overlay must be separate files".into());
        }
        assembly = crate::product_assembly::load(&mut registry, path, &assembly)?;
    }
    // The persistence half of hot installation: merge what the user had
    // installed. A problematic overlay is skipped whole — a startup without
    // its installs is degraded, not broken — and the skip is said out loud.
    // Capture provenance from the successful merge, never from a later read
    // of an editable overlay. This is service configuration, not kernel policy.
    let baseline: std::collections::HashSet<_> = assembly.instances.keys().cloned().collect();
    if let Some(path) = &cfg.overlay {
        if let Err(problem) = crate::overlay::apply(&mut registry, &mut assembly, path) {
            eprintln!(
                "warning: assembly overlay {} skipped: {problem}",
                path.display()
            );
        }
    }

    let installed: Vec<_> = assembly
        .instances
        .keys()
        .filter(|name| !baseline.contains(*name))
        .cloned()
        .collect();
    if let Some(sink) = assembly.instances.get_mut("workshop") {
        sink.config = Some(json!({"installedInstances": installed}));
    }

    check_ask_has_an_answerer(&registry, &assembly)?;

    Ok((registry, factories, assembly))
}

/// The stance-ask contradiction check: a trust gate configured to ASK needs
/// a wire into its `answer` port from a component claiming the
/// frontend-authorize profile — otherwise every ungranted admission is a
/// question nobody can answer and the turn waits forever. Checked after the
/// overlay merge, so a user-swapped frontend is held to it too.
fn check_ask_has_an_answerer(
    registry: &HashMap<String, ComponentManifest>,
    assembly: &AssemblyManifest,
) -> Result<(), String> {
    for (name, inst) in &assembly.instances {
        let asks = inst.component == trust_policy::NAME
            && inst.config.as_ref().and_then(|c| c["stance"].as_str()) == Some("ask");
        if !asks {
            continue;
        }
        let answer_port = format!("{name}.answer");
        let answered = assembly.wires.iter().any(|wire| {
            wire.to == answer_port
                && crate::contracts::assembly::parse_endpoint(&wire.from)
                    .and_then(|(src, _)| assembly.instances.get(src))
                    .and_then(|i| registry.get(&i.component))
                    .is_some_and(|m| m.implements.iter().any(|p| p == "frontend-authorize"))
        });
        if !answered {
            return Err(format!(
                "assembly contradiction: trust instance \"{name}\" has stance \"ask\", but no \
                 wire into {answer_port} comes from a component implementing the \
                 frontend-authorize profile — its questions would hang forever. Wire in a \
                 frontend that can answer, or set the stance to \"deny\"."
            ));
        }
    }
    Ok(())
}

/// Every wire of the standard assembly, in black and white.
fn standard_wires() -> Vec<Wire> {
    vec![
        // The expansion station on the input line: a /skill-name message is
        // expanded by the library (body in, arguments substituted, causal
        // link back to the typed original), anything else passes untouched.
        // Same audit-visible hop the trust gate takes on the tool line.
        Wire::new("ui.user", "skills.input"),
        Wire::new("skills.expanded", "loop.input"),
        Wire::new("loop.ask", "ctx.ask"),
        Wire::new("ctx.forward", "model.request"),
        Wire::new("ctx.condense", "cmodel.request"),
        Wire::new("cmodel.result", "ctx.condensed"),
        Wire::new("model.result", "loop.model"),
        // Every tool request passes the trust gate: non-admissions are
        // forwarded untouched (the audit-visible hop), install-class
        // calls only when granted
        Wire::new("loop.run", "trust.review"),
        Wire::new("trust.verdict", "loop.tools"),
        // The human's authorization answers reach the gate
        Wire::new("ui.answer", "trust.answer"),
        Wire::new("ui.answer", "web-browser.answer"),
        Wire::new("trust.forward", "web-browser.execute"),
        Wire::new("web-browser.outcome", "loop.tools"),
        Wire::new("web-browser.interrupted", "loop.faults"),
        Wire::new("ui.interrupt", "web-browser.control"),
        Wire::new("trust.forward", "local-desktop.execute"),
        Wire::new("local-desktop.outcome", "loop.tools"),
        Wire::new("local-desktop.interrupted", "loop.faults"),
        Wire::new("ui.interrupt", "local-desktop.control"),
        // The same wire carries settings the person turns mid-conversation.
        // Both ends filter by `channel`, so a trust answer and an effort
        // change ride the same event type without either mistaking the other
        // — the convention the trust gate already established.
        Wire::new("ui.answer", "ctx.dial"),
        Wire::new("trust.forward", "tool-catalog.execute"),
        Wire::new("tool-catalog.outcome", "loop.tools"),
        Wire::new("trust.forward", "fs.execute"),
        Wire::new("fs.outcome", "loop.tools"),
        Wire::new("trust.forward", "fs-write.execute"),
        Wire::new("fs-write.outcome", "loop.tools"),
        Wire::new("trust.forward", "search.execute"),
        Wire::new("search.outcome", "loop.tools"),
        Wire::new("trust.forward", "code.execute"),
        Wire::new("code.outcome", "loop.tools"),
        Wire::new("trust.forward", "shell.execute"),
        Wire::new("shell.outcome", "loop.tools"),
        // Background command finishes → wakes the loop as fresh input
        Wire::new("shell.wake", "loop.input"),
        Wire::new("trust.forward", "net.execute"),
        Wire::new("trust.forward", "search-web.execute"),
        Wire::new("search-web.outcome", "loop.tools"),
        Wire::new("net.outcome", "loop.tools"),
        Wire::new("trust.forward", "timer.execute"),
        Wire::new("timer.outcome", "loop.tools"),
        // Timer fires → wakes the loop as fresh input
        Wire::new("timer.wake", "loop.input"),
        Wire::new("trust.forward", "subagent.execute"),
        Wire::new("subagent.outcome", "loop.tools"),
        // A subagent's answer arrives the way a finished background command's
        // does; the host emits it through this instance's injector
        Wire::new("subagent.wake", "loop.input"),
        Wire::new("trust.forward", "watch.execute"),
        Wire::new("watch.outcome", "loop.tools"),
        // A watched path changes → wakes the loop as fresh input
        Wire::new("watch.wake", "loop.input"),
        Wire::new("trust.forward", "workshop.execute"),
        Wire::new("workshop.outcome", "loop.tools"),
        Wire::new("trust.forward", "skills.execute"),
        Wire::new("skills.outcome", "loop.tools"),
        Wire::new("trust.forward", "skill-installer.execute"),
        Wire::new("skill-installer.outcome", "loop.tools"),
        Wire::new("skill-installer.changed", "skills.refresh"),
        // The skill folders' standing watch, wired back to the library:
        // a folder dropped in by hand refreshes the listing, no model turn
        Wire::new("skills.changed", "skills.refresh"),
        Wire::new("loop.out", "ui.display"),
        Wire::new("ui.interrupt", "model.control"),
    ]
}

/// A kind of subagent: which of the standard tools it keeps, and what it is
/// told it is for.
///
/// Shipped mutation tools are separate assembly choices. This bounds the
/// provided operations, not arbitrary processes or provider-side behavior;
/// model networking and runtime-owned audit writes remain.
pub struct Expert {
    pub name: &'static str,
    /// Shown to the model when it asks who is available
    pub description: &'static str,
    /// Tool instances kept, on top of the ones every stream needs
    pub tools: &'static [&'static str],
    pub prompt: &'static str,
}

/// Instances every stream needs whatever it is for: the loop, the model, the
/// context gate and its condenser, the prompt fragments, the trust gate.
const EXPERT_CORE: &[&str] = &[
    "ui",
    "loop",
    "ctx",
    "cmodel",
    "model",
    "env",
    "zz-project-rules",
    "trust",
    "tool-catalog",
    // Structural, not a tool: what the user says reaches the loop THROUGH it
    // (ui.user → skills.input → skills.expanded → loop.input). Dropping it as
    // if it were just another toolset cut both wires, and an expert then sat
    // there having been told what to do with nothing listening — which is
    // exactly what the first real run did.
    "skills",
];

pub const EXPERTS: &[Expert] = &[
    Expert {
        name: "explorer",
        description: "Reads and searches to answer a question about what is there. \
                      Has no user-file mutation, installation, or command-execution tools.",
        tools: &["fs", "search"],
        prompt: "You are an explorer. You were sent one question and you answer it. \
                 Read and search as widely as you need, then reply with the ANSWER and \
                 the file paths and line numbers it rests on — not a description of how \
                 you looked. Whoever sent you cannot see anything you did, only your \
                 reply, so a reply that says \"I found it\" without saying what is no \
                 reply at all. Say plainly what you could not determine.",
    },
    Expert {
        name: "researcher",
        description: "Reads the web to answer a question. Has no user-file mutation, \
                      installation, or command-execution tools.",
        tools: &["net", "search-web"],
        prompt: "You are a researcher. You were sent one question and you answer it from \
                 what you can read on the web. Reply with the ANSWER and the URLs it rests \
                 on. Whoever sent you cannot see any page you opened, only your reply. Say \
                 which parts you could not confirm, and never present what one source \
                 claims as settled fact.",
    },
    Expert {
        name: "worker",
        description: "Reads, searches, and runs commands. Use when the work needs doing, \
                      not just finding out — but you will not see the steps.",
        tools: &["fs", "fs-write", "search", "shell", "skill-installer"],
        prompt: "You are a worker. You were sent one job and you do it. Whoever sent you \
                 will see only your final reply, so it must say what you actually changed \
                 — which files, what happened — and say plainly what you did not finish.",
    },
];

/// What the `ask` tool shows the model when no expert is named.
pub fn expert_roster() -> Value {
    Value::Array(
        EXPERTS
            .iter()
            .map(|e| json!({"name": e.name, "description": e.description}))
            .collect(),
    )
}

/// The product's expert host, with the same startup credential boundaries as
/// its main stream. Hosts choose storage separately from this assembly recipe.
pub fn expert_host(cfg: &PresetConfig) -> crate::StreamHost {
    let templates = EXPERTS
        .iter()
        .filter_map(|expert| {
            let (registry, factories, assembly) = expert_assembly(cfg, expert).ok()?;
            Some((
                expert.name.to_string(),
                crate::StreamTemplate {
                    registry,
                    factories,
                    assembly,
                },
            ))
        })
        .collect();
    crate::StreamHost::new(templates)
        .withholding(crate::models::key_env_names(&cfg.key_env))
        .redacting(crate::models::key_values(&cfg.key_env))
}

/// One expert's assembly: the standard one at depth 1, with every tool it does
/// not get removed — and `subagent` removed always, because an expert that
/// cannot hand work on had better not be offered the tool for it.
pub fn expert_assembly(cfg: &PresetConfig, expert: &Expert) -> Result<StandardAssembly, String> {
    // Experts are independent templates, not filtered copies of a user's
    // custom conversation baseline: filtering a custom gate could bypass it.
    let mut expert_cfg = cfg.clone();
    expert_cfg.assembly = None;
    let (registry, factories, mut assembly) = standard_at_depth(&expert_cfg, 1)?;
    let keep: std::collections::HashSet<&str> = EXPERT_CORE
        .iter()
        .chain(expert.tools.iter())
        .copied()
        .collect();
    assembly
        .instances
        .retain(|name, _| keep.contains(name.as_str()));
    // A wire naming an instance that is gone would fail inspection, which is
    // the assembler's contradiction to resolve, not the kernel's to tolerate.
    assembly.wires.retain(|w| {
        let ends = [&w.from, &w.to];
        ends.iter().all(|end| {
            end.split('.')
                .next()
                .is_some_and(|instance| keep.contains(instance))
        })
    });
    // Only advertise deferred tools owned by the final expert assembly, not
    // unused implementations in the registry or the main stream's tool list.
    let available: std::collections::HashSet<&str> = assembly
        .instances
        .values()
        .flat_map(|instance| registry[&instance.component].tools.iter())
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for (instance, key) in [("tool-catalog", "deferred"), ("ctx", "deferTools")] {
        if let Some(names) = assembly
            .instances
            .get_mut(instance)
            .and_then(|i| i.config.as_mut())
            .and_then(|c| c.get_mut(key))
            .and_then(Value::as_array_mut)
        {
            names.retain(|name| name.as_str().is_some_and(|name| available.contains(name)));
        }
    }
    // What this expert is for, said to the expert itself. The base system text
    // lives in the context gate's config — it is the gate that assembles the
    // prompt (base text plus each component's fragment), so replacing it there
    // is replacing the whole of what this stream is told it is.
    let gate = assembly
        .instances
        .get_mut("ctx")
        .ok_or_else(|| "the standard assembly has no context gate to tell".to_string())?;
    let mut config = gate.config.clone().unwrap_or_else(|| json!({}));
    config["system"] = json!(expert.prompt);
    gate.config = Some(config);
    Ok((registry, factories, assembly))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_endpoint_validation_never_guesses_responses_configuration() {
        assert!(validate_model_endpoint("typo", "m", "https://example.com").is_err());
        assert!(validate_model_endpoint("responses", "", "https://example.com").is_err());
        assert!(validate_model_endpoint("responses", "m", " ").is_err());
        assert!(validate_model_endpoint("responses", "m", "https://example.com").is_ok());
        assert!(validate_model_endpoint("scripted", "", "").is_ok());
    }

    #[test]
    fn native_search_is_profile_opt_in_for_the_responses_main_model_only() {
        let mut model = entry("exam", "TEST_KEY");
        model.adapter = "responses".into();
        assert!(main_model_config(&model, None)
            .get("nativeWebSearch")
            .is_none());
        model.profile = Some(json!({"nativeWebSearch":true}));
        assert_eq!(main_model_config(&model, None)["nativeWebSearch"], true);
        assert!(condenser_config(&model).get("nativeWebSearch").is_none());
        model.adapter = "openai".into();
        assert!(main_model_config(&model, None)
            .get("nativeWebSearch")
            .is_none());
        model.adapter = "responses".into();
        model.profile = Some(json!({"nativeWebSearch":false}));
        assert!(main_model_config(&model, None)
            .get("nativeWebSearch")
            .is_none());
    }

    #[test]
    fn native_images_are_profile_opt_in_and_never_enabled_for_condensation() {
        let mut model = entry("exam", "TEST_KEY");
        model.adapter = "responses".into();
        assert!(main_model_config(&model, None)
            .get("nativeImageGeneration")
            .is_none());
        model.profile = Some(json!({"nativeImageGeneration":true}));
        assert_eq!(
            main_model_config(&model, None)["nativeImageGeneration"],
            true
        );
        assert!(condenser_config(&model)
            .get("nativeImageGeneration")
            .is_none());
        model.adapter = "openai".into();
        assert!(main_model_config(&model, None)
            .get("nativeImageGeneration")
            .is_none());
    }

    #[test]
    fn responses_condenser_explicitly_selects_native_trigger() {
        let mut model = entry("exam", "TEST_KEY");
        model.adapter = "responses".into();
        let config = condenser_config(&model);
        assert_eq!(config["compactionProtocol"], "trigger");
        assert!(config["system"].is_null());
        assert_eq!(config["baseUrl"], model.base_url);
        model.adapter = "openai".into();
        let config = condenser_config(&model);
        assert!(config["compactionProtocol"].is_null());
        assert_eq!(config["system"], CONDENSER_SYSTEM);
    }

    fn entry(id: &str, key_env: &str) -> crate::models::Entry {
        crate::models::Entry {
            id: id.to_string(),
            adapter: "openai".to_string(),
            model: id.to_string(),
            base_url: "https://example".to_string(),
            key_env: key_env.to_string(),
            profile: None,
        }
    }

    /// A saved choice whose key is gone must not stop the program.
    ///
    /// Seen for real: the preference named a built-in entry whose variable was
    /// never set, while five hand-written entries carried their keys. Starting
    /// refused, and said "no model this installation can reach" directly above
    /// a list of five it could. Both halves were wrong — it could reach five,
    /// and one unreachable preference is not a reason to refuse.
    ///
    /// The same reasoning the code already applied to a DELETED entry: removing
    /// an entry gives the default back rather than a program that will not
    /// start, and removing its key is the same situation for the person.
    #[test]
    fn a_preferred_model_with_no_key_falls_through_and_says_so() {
        let present = "LATTICE_TEST_KEY_PRESENT";
        std::env::set_var(present, "x");
        let catalog = vec![
            entry("deepseek", "LATTICE_TEST_KEY_ABSENT"),
            entry("flash", present),
        ];
        let mut problems = Vec::new();
        let chosen = model_from_env_or_preference(
            false,
            entry("fallback", present),
            &catalog,
            Some("deepseek"),
            &mut problems,
        );
        assert_eq!(chosen.id, "flash", "the first one it can actually reach");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("deepseek") && problems[0].contains("flash"),
            "it names what was wanted and what it started on instead: {}",
            problems[0]
        );
        std::env::remove_var(present);
    }

    /// A saved choice that can be reached is simply used, quietly.
    #[test]
    fn a_preferred_model_that_works_is_used_without_comment() {
        let present = "LATTICE_TEST_KEY_WORKS";
        std::env::set_var(present, "x");
        let catalog = vec![entry("flash", present), entry("pro", present)];
        let mut problems = Vec::new();
        let chosen = model_from_env_or_preference(
            false,
            entry("fallback", present),
            &catalog,
            Some("pro"),
            &mut problems,
        );
        assert_eq!(chosen.id, "pro");
        assert!(problems.is_empty(), "{problems:?}");
        std::env::remove_var(present);
    }

    /// What was typed on this launch outranks everything, key or no key —
    /// nothing here may quietly overrule an instruction just given.
    #[test]
    fn what_was_said_this_launch_is_not_second_guessed() {
        let present = "LATTICE_TEST_KEY_SAID";
        std::env::set_var(present, "x");
        let catalog = vec![entry("flash", present)];
        let mut problems = Vec::new();
        let chosen = model_from_env_or_preference(
            true,
            entry("typed", "LATTICE_TEST_KEY_MISSING"),
            &catalog,
            Some("flash"),
            &mut problems,
        );
        assert_eq!(chosen.id, "typed");
        assert!(problems.is_empty());
        std::env::remove_var(present);
    }

    /// Nothing reachable anywhere: the preference is still reported, and the
    /// caller is left to refuse — this function does not decide that.
    #[test]
    fn with_nothing_reachable_it_still_says_what_was_wanted() {
        let catalog = vec![entry("deepseek", "LATTICE_TEST_KEY_NONE")];
        let mut problems = Vec::new();
        let chosen = model_from_env_or_preference(
            false,
            entry("fallback", "LATTICE_TEST_KEY_NONE"),
            &catalog,
            Some("deepseek"),
            &mut problems,
        );
        assert_eq!(chosen.id, "deepseek", "the only entry there is");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("no key"), "{}", problems[0]);
    }

    /// A saved name the catalog no longer has stays quiet. Removing an entry
    /// is something the person did on purpose, so getting the default back is
    /// what they expect — this was decided before and is not changed here.
    #[test]
    fn a_preferred_model_that_was_deleted_falls_back_without_a_word() {
        let present = "LATTICE_TEST_KEY_DELETED";
        std::env::set_var(present, "x");
        let catalog = vec![entry("flash", present)];
        let mut problems = Vec::new();
        let chosen = model_from_env_or_preference(
            false,
            entry("fallback", present),
            &catalog,
            Some("gone"),
            &mut problems,
        );
        assert_eq!(chosen.id, "flash");
        assert!(problems.is_empty(), "{problems:?}");
        std::env::remove_var(present);
    }
}
