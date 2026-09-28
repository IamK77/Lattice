//! The hand-written model catalog, from the file to the running config.
//!
//! Its own test binary because these tests drive `PresetConfig::from_env`
//! through real environment variables, and `LATTICE_SCRIPTED` — which the
//! preset tests set and clear freely — short-circuits the whole path. Env vars
//! are process-global; separate files are separate processes.

use serde_json::json;

use lattice::preset::PresetConfig;

/// Environment variables belong to the PROCESS, and cargo runs the tests in
/// one file on several threads — so two of these staging their own catalog at
/// once would read each other's. Held for the whole body of each test, not
/// just the staging: what is under test reads the environment too.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the environment for the duration of one test. A poisoned lock is
/// stepped over rather than propagated: one test panicking should fail one
/// test, not every test after it.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write a catalog and a preferences file, point the environment at them, and
/// clear everything that would otherwise decide the model for us.
fn staged(catalog: serde_json::Value, chosen: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let models = dir.path().join("models.json");
    std::fs::write(&models, serde_json::to_string_pretty(&catalog).unwrap()).unwrap();
    let prefs = dir.path().join("preferences.json");
    let doc = match chosen {
        Some(id) => json!({ "model": id }),
        None => json!({}),
    };
    std::fs::write(&prefs, serde_json::to_string(&doc).unwrap()).unwrap();

    std::env::set_var("LATTICE_MODELS", &models);
    std::env::set_var("LATTICE_PREFERENCES", &prefs);
    std::env::set_var("LATTICE_OVERLAY", ""); // never touch the real one
    for said in [
        "LATTICE_SCRIPTED",
        "LATTICE_ADAPTER",
        "LATTICE_MODEL",
        "LATTICE_BASE_URL",
        "LATTICE_API_KEY_ENV",
        "LATTICE_THINKING",
    ] {
        std::env::remove_var(said);
    }
    dir
}

fn kimi() -> serde_json::Value {
    json!({"models": {"kimi": {
        "adapter": "openai",
        "model": "kimi-k2",
        "baseUrl": "https://api.moonshot.cn/v1",
        "apiKeyEnv": "MOONSHOT_API_KEY",
        "profile": {
            "contextWindow": 256000,
            "effort": ["low", "high"],
            "usageFields": {"input": "prompt_tokens"},
        },
    }}})
}

/// A model chosen from the catalog must arrive complete — endpoint AND
/// description. Carrying only the endpoint is the failure this closes: the
/// model ran, budgeted against the startup fallback of a million tokens, with
/// an effort ladder that was empty because nothing here had ever heard of it.
#[test]
fn the_chosen_entrys_profile_survives_the_trip_into_the_running_config() {
    let _serial = alone();
    let _dir = staged(kimi(), Some("kimi"));
    // This entry has to be REACHABLE for the saved choice to be honoured —
    // a preference whose key is missing falls through to one that works.
    // Not the subject here, so it is simply satisfied.
    std::env::set_var("MOONSHOT_API_KEY", "x");
    let cfg = PresetConfig::from_env();

    assert_eq!(cfg.model, "kimi-k2", "the saved choice is honoured");
    assert_eq!(cfg.base_url, "https://api.moonshot.cn/v1");
    let profile = cfg.profile.clone().expect("the entry described this model");
    assert_eq!(profile["contextWindow"], 256000);
    assert_eq!(profile["effort"], json!(["low", "high"]));
    std::env::remove_var("MOONSHOT_API_KEY");

    // And it reaches both things that act on it.
    let (_, _, assembly) = lattice::preset::standard(&cfg).expect("preset builds");
    let config = |name: &str| assembly.instances[name].config.clone().unwrap();
    assert_eq!(config("model")["effort"], json!(["low", "high"]));
    assert_eq!(config("ctx")["profile"]["contextWindow"], 256000);
}

/// What was said on THIS launch outranks the saved choice — the same order
/// the thinking setting follows. Any one of the four is enough, because they
/// describe one model between them: honouring a saved name while
/// `LATTICE_BASE_URL` points elsewhere would assemble a model nobody asked for.
#[test]
fn what_was_typed_this_launch_outranks_the_saved_model() {
    let _serial = alone();
    let _dir = staged(kimi(), Some("kimi"));
    std::env::set_var("LATTICE_MODEL", "deepseek-v4-flash");
    let cfg = PresetConfig::from_env();
    std::env::remove_var("LATTICE_MODEL");

    assert_eq!(cfg.model, "deepseek-v4-flash");
    assert!(
        cfg.base_url.contains("deepseek"),
        "the endpoint follows what was said, not the saved entry: {}",
        cfg.base_url
    );
}

/// A saved name the catalog no longer has is ignored rather than fatal: a
/// person who deletes an entry should get the default back, not a program that
/// will not start.
#[test]
fn a_saved_model_the_catalog_lost_falls_back_instead_of_failing() {
    let _serial = alone();
    let _dir = staged(json!({"models": {}}), Some("kimi"));
    let cfg = PresetConfig::from_env();
    assert_eq!(cfg.model, "deepseek-v4-flash");
    assert!(
        cfg.catalog_problems.is_empty(),
        "an empty catalog is not a problem"
    );
}

/// A hand-written file is the one piece of configuration nothing else checks,
/// so what is wrong with it has to travel to whoever can say it out loud.
/// Silence here shows up as "the model I added is not in the list".
#[test]
fn what_is_wrong_with_the_file_travels_with_the_config() {
    let _serial = alone();
    let _dir = staged(
        json!({"models": {"oops": {"adapter": "antropic", "model": "m"}}}),
        None,
    );
    let cfg = PresetConfig::from_env();
    let said = cfg.catalog_problems.join(" | ");
    assert!(
        said.contains("antropic") && said.contains("oops"),
        "the config must carry the complaint, not swallow it: {said}"
    );
}

/// A key can live in the catalog itself — and what travels onward is still
/// only a NAME.
///
/// The value never reaches an adapter's configuration, because a model swap
/// records that configuration on the ledger and the ledger is permanent. A
/// literal key is staged into this process's environment at load, and
/// everything downstream handles the variable that holds it.
#[test]
fn a_key_written_into_the_catalog_is_usable_without_ever_leaving_this_process() {
    let _serial = alone();
    let _dir = staged(
        json!({"models": {"inline": {
            "adapter": "openai",
            "model": "inline-1",
            "baseUrl": "https://example.invalid",
            "apiKey": "sk-written-in-the-file",
        }}}),
        Some("inline"),
    );
    let cfg = PresetConfig::from_env();

    assert_eq!(cfg.model, "inline-1", "the saved choice is honoured");
    assert_ne!(
        cfg.key_env, "sk-written-in-the-file",
        "the config carries a variable NAME, never the key"
    );
    assert!(
        !cfg.key_env.is_empty(),
        "and it does carry one: {:?}",
        cfg.key_env
    );
    assert_eq!(
        std::env::var(&cfg.key_env).ok().as_deref(),
        Some("sk-written-in-the-file"),
        "the key is where the name says it is"
    );

    // The whole point of "configured plus a key present" — this one counts
    // as reachable, and nothing about it is guessed.
    let entry = lattice::preset::running_entry(&cfg);
    assert!(entry.key_present());
    std::env::remove_var(&cfg.key_env);
}

/// With nothing said and nothing saved, the catalog decides — and it picks
/// one that can actually be reached.
///
/// There used to be a guess here: no DeepSeek key in the environment, so
/// assume Anthropic. It produced a configuration contradicting itself — the
/// OpenAI dialect, DeepSeek's endpoint, and a variable named for Anthropic —
/// so a person following the resulting message set a key that would have
/// gone to the wrong provider.
#[test]
fn with_no_preference_the_default_is_a_model_that_can_be_reached() {
    let _serial = alone();
    let _dir = staged(
        json!({"models": {
            // First in the file, and unreachable: no key anywhere.
            "unreachable": {
                "adapter": "openai", "model": "nope-1",
                "baseUrl": "https://nope.invalid", "apiKeyEnv": "NOT_SET_ANYWHERE",
            },
            // Second, and usable.
            "usable": {
                "adapter": "openai", "model": "yes-1",
                "baseUrl": "https://yes.invalid", "apiKey": "sk-here",
            },
        }}),
        None,
    );
    std::env::remove_var("NOT_SET_ANYWHERE");
    let cfg = PresetConfig::from_env();

    assert_eq!(cfg.model, "yes-1", "the reachable one is chosen");
    assert_eq!(
        std::env::var(&cfg.key_env).ok().as_deref(),
        Some("sk-here"),
        "and its key really is where the config says"
    );
    // Never a name belonging to a provider nobody configured.
    assert_ne!(cfg.key_env, "ANTHROPIC_API_KEY");
    std::env::remove_var(&cfg.key_env);
}

/// The redaction placeholder is not a key, and must not be sent as one.
///
/// It gets into this file by a route worth naming: the agent reads the
/// catalog, the ledger's redactor replaces the key with `[redacted]` on the
/// way in, and a later write-back of what was read puts that string where the
/// key was. Seen for real — two entries in a working catalog ended up holding
/// it, and the endpoint answered `401 Authentication Fails, Your api key:
/// ****ted]`.
#[test]
fn an_entry_whose_key_was_overwritten_by_the_placeholder_is_skipped_and_named() {
    let _serial = alone();
    let _dir = staged(
        json!({"models": {
            "lost": {
                "adapter": "openai", "model": "m",
                "baseUrl": "https://example", "apiKey": "[redacted]",
            },
            "kept": {
                "adapter": "openai", "model": "n",
                "baseUrl": "https://example", "apiKey": "sk-real-looking-value",
            },
        }}),
        None,
    );
    let (catalog, problems) = lattice::models::load_reported();
    assert!(
        catalog.iter().all(|e| e.id != "lost"),
        "the entry is skipped rather than used: {:?}",
        catalog.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert!(
        catalog.iter().any(|e| e.id == "kept"),
        "one bad entry does not cost the others"
    );
    let said = problems.join("\n");
    assert!(
        said.contains("lost") && said.contains("apiKey"),
        "it names the entry and the field: {said}"
    );
    assert!(
        said.contains("writing it back"),
        "and says how the key was lost, which is the only way to avoid it again: {said}"
    );
}
