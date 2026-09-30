//! The model catalog: which models this installation can reach.
//!
//! An entry names the first two axes of a model call — the DIALECT (which
//! adapter component speaks to it) and the ADDRESS (endpoint, model id, and
//! the environment variable holding the key). The third axis, the model's
//! temperament, is deliberately absent: context window, effort rungs and
//! usage-field names live in [`crate::profile`] and are looked up by `model`.
//! One file says where to reach it, the other says what it is like.
//!
//! Why a catalog exists at all: until now the running model was four
//! environment variables read once at startup, so "the models we have
//! configured" was not a thing that could be read — there was exactly one, and
//! it was whatever the shell said. Choosing between models needs a list, and a
//! list needs somewhere to live.
//!
//! `~/.lattice/models.json` is that place (canon: schemas/model_catalog.json),
//! and it is the WHOLE of it: the binary ships no entries. It used to ship two,
//! which meant a fresh installation was offered models it had no key for and a
//! model could be deleted from the file and come back on the next start. What
//! ships is profiles — what a model is like — never a claim that you can reach
//! one. See [`WELL_KNOWN_KEY_ENVS`] for the part of that list worth keeping.
//!
//! Keys are named, never written: an entry carries the NAME of an environment
//! variable. A key in a configuration file is a key published, which is the
//! same rule the web-search component already follows.

use crate::kernel::log::REDACTED;
use std::path::PathBuf;

use serde_json::Value;

/// One reachable model: which dialect, and where.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    /// The short name a person picks with `/model`, and what the preference
    /// records. Stable across a change of the underlying model id, which is
    /// why the preference stores this rather than the model string.
    pub id: String,
    /// Which adapter component speaks to it ("openai" | "anthropic")
    pub adapter: String,
    /// The model identifier the endpoint expects — also the profile key
    pub model: String,
    pub base_url: String,
    /// The NAME of the environment variable holding the key.
    ///
    /// Still a name even when the catalog carried the key itself: a literal
    /// key is staged into this process's environment under a name of its own
    /// at load (see [`load_reported`]), and this points there. Everything
    /// downstream therefore only ever handles a NAME — which matters, because
    /// an adapter's configuration is recorded on the ledger when a model is
    /// swapped, and the ledger is forever.
    pub key_env: String,
    /// This model's temperament, written into the entry.
    ///
    /// Profiles ship embedded (see [`crate::profile`]) so a fresh binary knows
    /// the models it was built with. That left a hole: a model added by hand
    /// had NO profile and no way to get one, so it ran budgeted against the
    /// startup fallback of a million tokens, with an empty effort ladder and
    /// the wrong usage-field names. A catalog that anyone can extend needs the
    /// describing half to be extensible too.
    ///
    /// Written in the entry rather than in a file beside it because adding a
    /// model is one act and should touch one place. The shape is the same as a
    /// shipped profile (schemas/model_profile.json), minus `model`, which the
    /// entry already names.
    pub profile: Option<Value>,
}

impl Entry {
    /// Is the key for this entry actually in the environment right now?
    ///
    /// Read at listing time rather than remembered, because it is a fact about
    /// this process's environment and not about the catalog. Shown beside the
    /// entry so a model that cannot be reached is visible BEFORE it is chosen,
    /// instead of failing on the next turn.
    pub fn key_present(&self) -> bool {
        std::env::var(&self.key_env).is_ok_and(|v| !v.is_empty())
    }

    /// This entry's profile: what it says itself, over what ships for its
    /// model, FIELD BY FIELD.
    ///
    /// The entry wins where it speaks — someone who wrote a profile into their
    /// catalog is correcting what the binary believes, and being quietly
    /// overruled by a built-in would make the field pointless exactly where it
    /// matters (a model whose provider changed something). But it wins only
    /// where it speaks: correcting one window must not silently discard the
    /// effort rungs nobody meant to touch. Same rule as the entry itself,
    /// where an omitted endpoint is inherited from what it replaces.
    pub fn profile(&self) -> Option<Value> {
        let shipped = crate::profile::lookup(&self.model);
        let Some(mine) = self.profile.as_ref().and_then(Value::as_object) else {
            return shipped;
        };
        let mut merged = shipped.unwrap_or_else(|| Value::Object(Default::default()));
        for (key, value) in mine {
            merged[key] = value.clone();
        }
        Some(merged)
    }

    /// This entry's context window — `None` when nothing describes it, which
    /// is not a failure: the gate keeps whatever it was using rather than
    /// budgeting against a guess.
    pub fn context_window(&self) -> Option<u64> {
        crate::profile::window_of(&self.profile()?)
    }

    /// How this model's provider names the usage numbers.
    ///
    /// Its profile when it has one, and otherwise the conventional names of
    /// its DIALECT — never nothing. "Nothing" was the dangerous answer:
    /// changing to a model with no profile left the previous model's names in
    /// place, and a name that does not appear in the reply reads as zero. Two
    /// already-fixed faults came back that way — the scale reading zero, so
    /// nothing was ever trimmed and the context grew until the provider
    /// refused it; and the cache count reading zero, so every turn was judged
    /// a cache miss.
    pub fn usage_fields(&self) -> serde_json::Map<String, Value> {
        let named = self
            .profile()
            .map(|p| crate::profile::usage_of(&p))
            .unwrap_or_default();
        if !named.is_empty() {
            return named;
        }
        let conventional = match self.adapter.as_str() {
            "anthropic" => serde_json::json!({
                "input": "input_tokens",
                "output": "output_tokens",
                "cacheRead": "cache_read_input_tokens",
                "cacheWrite": "cache_creation_input_tokens",
            }),
            "responses" => serde_json::json!({
                "input": "input_tokens",
                "output": "output_tokens",
                "cacheRead": "input_tokens_details.cached_tokens",
            }),
            // Chat Completions endpoints, DeepSeek among them.
            _ => serde_json::json!({
                "input": "prompt_tokens",
                "output": "completion_tokens",
                "cacheRead": "prompt_cache_hit_tokens",
            }),
        };
        conventional.as_object().cloned().expect("a literal object")
    }

    /// This model's effort rungs, weakest first. Empty when unknown.
    pub fn effort_rungs(&self) -> Vec<String> {
        self.profile()
            .map(|p| crate::profile::rungs_of(&p))
            .unwrap_or_default()
    }

    /// Can this model be sent a picture? Its profile decides — including one
    /// written into the catalog entry, for a model this binary never heard of.
    pub fn accepts_images(&self) -> bool {
        self.profile()
            .is_some_and(|p| crate::profile::accepts_images_of(&p))
    }

    /// Native web search is opt-in, including for catalog-provided profiles.
    pub fn native_web_search(&self) -> bool {
        self.profile()
            .is_some_and(|p| p["nativeWebSearch"].as_bool() == Some(true))
    }

    /// Hosted image generation is opt-in and separate from image input.
    pub fn native_image_generation(&self) -> bool {
        self.profile()
            .is_some_and(|p| p["nativeImageGeneration"].as_bool() == Some(true))
    }

    /// The most this model will write in one reply, when its profile says.
    pub fn max_output(&self) -> Option<u64> {
        self.profile()
            .and_then(|p| crate::profile::max_output_of(&p))
    }
}

/// Variable names an API key conventionally lives in, whether or not this
/// installation configured a model that uses one.
///
/// This is NOT a model list and must not become one. The binary used to ship
/// two catalog entries — a DeepSeek one and an Anthropic one — and they were a
/// mistake in three ways: neither works without a key this binary cannot know
/// you have, so a fresh install listed two models it could not reach instead of
/// saying the catalog was empty; a preference naming one of them refused to
/// start a session whose own five entries were all fine (fixed 2026-07-29, but
/// the reason it could happen was here); and an entry is an ADDRESS, which this
/// architecture puts in configuration on purpose, not in the binary. A profile
/// is a different thing and rightly ships: it says what a model is like, which
/// stays true whether or not you can reach it.
///
/// What was worth keeping is only this list of names. Two jobs depend on it and
/// neither needs a model to exist: keeping these variables out of component
/// subprocesses, and handing their values to the redactor.
const WELL_KNOWN_KEY_ENVS: [&str; 2] = ["DEEPSEEK_API_KEY", "ANTHROPIC_API_KEY"];

/// `~/.lattice/models.json`, or `LATTICE_MODELS` when set.
pub fn path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("LATTICE_MODELS") {
        return (!custom.is_empty()).then(|| PathBuf::from(custom));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".lattice/models.json"))
}

/// The catalog: what ships, with the user's file merged over it by id.
pub fn load() -> Vec<Entry> {
    load_reported().0
}

/// Write the catalog file, replacing it only once the new text is completely
/// on disk.
///
/// Not a nicety. This file holds keys written in full, and a key here may be
/// the only copy of itself; a half-written file is an unrecoverable one. Write
/// beside it, then rename — a rename within a directory either happened or did
/// not, so an interruption leaves the old file exactly as it was.
fn write_catalog(path: &std::path::Path, document: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(document)
        .map_err(|problem| format!("cannot write the catalog: {problem}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|problem| format!("{} cannot be created: {problem}", parent.display()))?;
    }
    let staged = path.with_extension("json.writing");
    std::fs::write(&staged, format!("{text}\n"))
        .map_err(|problem| format!("{} cannot be written: {problem}", staged.display()))?;
    // Keys live in here; keep it to the owner, as it was before this touched it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let owner_only = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&staged, owner_only);
    }
    std::fs::rename(&staged, path)
        .map_err(|problem| format!("{} cannot be replaced: {problem}", path.display()))
}

/// The catalog file as a document, for editing it. A missing file reads as an
/// empty catalog so that the first model can be added to nothing.
fn open_catalog(path: &std::path::Path) -> Result<Value, String> {
    let document = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|problem| {
            format!(
                "{} is not valid JSON ({problem}). Fix it by hand first — \
                 rewriting a file this program cannot read would throw away \
                 whatever else is in it.",
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({"models": {}})
        }
        Err(error) => {
            return Err(format!(
                "cannot read {}: {error}; refusing to rewrite it",
                path.display()
            ))
        }
    };
    Ok(document)
}

/// Where the catalog is, or why there is nowhere to put one.
fn catalog_path() -> Result<PathBuf, String> {
    path().ok_or_else(|| "there is no home directory to keep a catalog in".to_string())
}

/// Take one model out of the catalog for good.
///
/// The catalog is the user's file and nothing else, so this removes it and it
/// stays removed. What goes with it is whatever key was written in the entry —
/// there is no copy kept, by decision.
pub fn remove(id: &str) -> Result<(), String> {
    remove_from(&catalog_path()?, id)
}

/// [`remove`], told which file.
pub fn remove_from(path: &std::path::Path, id: &str) -> Result<(), String> {
    let mut document = open_catalog(path)?;
    let Some(models) = document.get_mut("models").and_then(Value::as_object_mut) else {
        return Err(format!("{} has no \"models\" object", path.display()));
    };
    if models.remove(id).is_none() {
        return Err(format!(
            "{} does not have a model called {id:?}",
            path.display()
        ));
    }
    write_catalog(path, &document)
}

/// Put one model into the catalog. `spec` is the entry exactly as the file
/// spells it (adapter, model, baseUrl, apiKey or apiKeyEnv).
///
/// Refuses to overwrite: a name already in use is a different act — that is
/// editing an entry, and doing it silently under the word "add" would discard
/// the key the old one held.
pub fn add(id: &str, spec: Value) -> Result<(), String> {
    add_to(&catalog_path()?, id, spec)
}

/// [`add`], told which file.
pub fn add_to(path: &std::path::Path, id: &str, spec: Value) -> Result<(), String> {
    if id.is_empty() {
        return Err("a model needs a short name to be chosen by".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        || !id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(format!(
            "{id:?} cannot be a short name — lower-case letters, digits, dot, \
             dash and underscore, starting with a letter or digit"
        ));
    }
    let mut document = open_catalog(path)?;
    if !document.get("models").is_some_and(Value::is_object) {
        document["models"] = serde_json::json!({});
    }
    let models = document["models"]
        .as_object_mut()
        .expect("just ensured it is an object");
    if models.contains_key(id) {
        return Err(format!("{id:?} is already in the catalog"));
    }
    models.insert(id.to_string(), spec);
    write_catalog(path, &document)
}

/// The catalog, and everything wrong with the user's file that they should
/// hear about.
///
/// The complaints exist because this file is HAND-WRITTEN and nothing else
/// checks it. Swallowing a mistake here shows up as "the model I added is not
/// in the list", with no hint that a comma was missing — the one failure mode
/// a hand-edited file must not have. The assembly overlay next door already
/// says its problems out loud at startup; this now does the same.
pub fn load_reported() -> (Vec<Entry>, Vec<String>) {
    match path() {
        Some(path) => load_from(&path),
        None => (Vec::new(), Vec::new()),
    }
}

/// Every environment variable this installation might hold a model key in:
/// the catalog's, the well-known ones, plus whatever the running config named.
///
/// Used to keep those variables away from component subprocesses. A process
/// -form component is somebody else's code running next door, and it inherits
/// the whole environment unless told otherwise — so without this, installing
/// one is handing over every API key the session was started with. Names, not
/// values: the list says which variables to withhold, and nothing here ever
/// reads one.
pub fn key_env_names(running: &str) -> Vec<String> {
    let (catalog, _) = load_reported();
    let mut names: Vec<String> = catalog
        .iter()
        .map(|entry| entry.key_env.clone())
        .chain(WELL_KNOWN_KEY_ENVS.iter().map(|name| name.to_string()))
        .chain(std::iter::once(running.to_string()))
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The VALUES of every key this installation might use, for keeping them off
/// the ledger.
///
/// The names go to the subprocess denylist ([`key_env_names`]); the values go
/// to the redactor, because a key does not only appear under a key-shaped
/// name — it appears in a config file the agent read, in the output of `env`,
/// quoted inside an error. Only what a secret IS can catch those.
///
/// Read from the environment, which is where they all are by the time this is
/// called: a key written into the catalog was staged into a variable at load.
pub fn key_values(running: &str) -> Vec<String> {
    key_env_names(running)
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
        .collect()
}

/// [`load`], told where to look. A missing file leaves the built-ins; so does
/// an unreadable or malformed one — a catalog that cannot be parsed must not
/// stop the program starting, exactly as with preferences.
pub fn load_from(path: &std::path::Path) -> (Vec<Entry>, Vec<String>) {
    let here = path.display();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // No file is the ordinary state, not a problem to report
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let document: Value = match serde_json::from_str(&text) {
        Ok(document) => document,
        Err(problem) => {
            return (
                Vec::new(),
                vec![format!(
                    "{here} is not valid JSON ({problem}) — leaving no models configured"
                )],
            )
        }
    };
    merge(&document, &here.to_string())
}

/// The merge rule, separated from reading the file so it can be tested
/// without one: an entry whose id matches a built-in REPLACES it in place
/// (keeping its position, so a familiar list does not reshuffle), and a new id
/// is appended in the file's own order.
///
/// A bad ENTRY is skipped and named; the rest of the file still applies. That
/// is deliberately unlike the assembly overlay, which is skipped whole — an
/// assembly is one interlocking thing and half of it is dangerous, while a
/// catalog is a list and one bad line costs one model.
pub(crate) fn merge(document: &Value, here: &str) -> (Vec<Entry>, Vec<String>) {
    let mut said = Vec::new();
    let Some(models) = document.get("models").and_then(Value::as_object) else {
        if !document.is_null() {
            said.push(format!(
                "{here} has no \"models\" object — leaving no models configured"
            ));
        }
        return (Vec::new(), said);
    };
    let mut merged: Vec<Entry> = Vec::new();
    for (id, spec) in models {
        let field = |key: &str| spec.get(key).and_then(Value::as_str);
        // adapter and model are the two things nothing can stand in for
        let (Some(adapter), Some(model)) = (field("adapter"), field("model")) else {
            said.push(format!(
                "{here}: model \"{id}\" skipped — it must name both \"adapter\" and \"model\""
            ));
            continue;
        };
        // A misspelled adapter would otherwise fall through to the OpenAI
        // dialect silently and talk the wrong wire format at the endpoint.
        if !["openai", "anthropic", "responses", "scripted"].contains(&adapter) {
            said.push(format!(
                "{here}: model \"{id}\" skipped — no adapter called \"{adapter}\" \
                 (known: openai, anthropic, responses, scripted)"
            ));
            continue;
        }
        if let Some(unknown) = spec.as_object().map(|fields| {
            fields
                .keys()
                .filter(|key| {
                    ![
                        "adapter",
                        "model",
                        "baseUrl",
                        "apiKey",
                        "apiKeyEnv",
                        "profile",
                        "note",
                    ]
                    .contains(&key.as_str())
                })
                .cloned()
                .collect::<Vec<_>>()
        }) {
            for key in unknown {
                said.push(format!(
                    "{here}: model \"{id}\" has an unknown field \"{key}\", ignored \
                     (known: adapter, model, baseUrl, apiKey, apiKeyEnv, profile)"
                ));
            }
        }
        // A key written into the file itself. Staged into this process's
        // environment under a name derived from the id, so that from here on
        // it is indistinguishable from a key that was always in a variable —
        // and so that nothing but this process ever sees the value. It is
        // also, by that same step, covered by the list of variables withheld
        // from component subprocesses.
        // The one bad key this program can recognise: its own redaction
        // placeholder. It gets here by a route worth naming — the agent reads
        // this file, the ledger's redactor replaces the key with `[redacted]`
        // on the way in, and a later write-back of what was read puts that
        // string where the key was. The key is gone at that point; the least
        // this can do is refuse to send it to a provider as if it were one.
        if field("apiKey") == Some(REDACTED) {
            said.push(format!(
                "{here}: model \"{id}\" has \"{REDACTED}\" where its apiKey should be — \
                 the real key was overwritten (most likely by something reading this \
                 file and writing it back). Put the key in again; this entry is skipped."
            ));
            continue;
        }
        let key_env = match field("apiKey").filter(|k| !k.is_empty()) {
            Some(literal) => {
                // Encode every byte, including punctuation, without folding
                // distinct catalog names into the same credential slot.
                let encoded: String = id.bytes().map(|byte| format!("{byte:02X}")).collect();
                let name = format!("LATTICE_KEY_V2_{encoded}");
                std::env::set_var(&name, literal);
                Some(name)
            }
            None => field("apiKeyEnv").map(str::to_string),
        };
        let profile = spec.get("profile").filter(|p| !p.is_null()).cloned();
        if let Some(profile) = &profile {
            said.extend(profile_complaints(here, id, profile));
        }
        let existing = merged.iter().position(|entry| &entry.id == id);
        // An omitted endpoint or key name falls back to the entry this one
        // replaces, so overriding just the model id of a shipped entry does
        // not require restating where it lives.
        let inherited = existing.map(|at| &merged[at]);
        let entry = Entry {
            id: id.clone(),
            adapter: adapter.to_string(),
            model: model.to_string(),
            base_url: field("baseUrl")
                .map(str::to_string)
                .or_else(|| inherited.map(|e| e.base_url.clone()))
                .unwrap_or_default(),
            key_env: key_env
                .or_else(|| inherited.map(|e| e.key_env.clone()))
                .unwrap_or_default(),
            profile,
        };
        // A brand-new entry that names neither an endpoint nor a key variable
        // has nothing to inherit, and an empty string is not the same as
        // absent: it OVERRIDES the adapter's own default, so the request goes
        // to "/chat/completions" and the key lookup asks the environment for
        // a variable with no name. Both fail on the first turn, in a way that
        // reads like the endpoint's fault.
        if entry.base_url.is_empty() {
            said.push(format!(
                "{here}: model \"{id}\" names no baseUrl and replaces nothing that did — \
                 calls to it will have nowhere to go"
            ));
        }
        if entry.key_env.is_empty() {
            said.push(format!(
                "{here}: model \"{id}\" names neither apiKey nor apiKeyEnv, and replaces \
                 nothing that did — every call to it will fail for want of a key"
            ));
        }
        match existing {
            Some(at) => merged[at] = entry,
            None => merged.push(entry),
        }
    }
    (merged, said)
}

/// What is wrong with a hand-written profile. None of these skip the entry —
/// the model still works, it is just described worse than the author meant.
///
/// The rung check earns its place: a word off the ruler is silently ignored
/// when a request is placed, so `"effort": ["turbo"]` reads as "this model has
/// no effort settings at all" and nothing anywhere would say why.
fn profile_complaints(here: &str, id: &str, profile: &Value) -> Vec<String> {
    let mut said = Vec::new();
    if !profile.is_object() {
        said.push(format!(
            "{here}: model \"{id}\" has a \"profile\" that is not an object, ignored"
        ));
        return said;
    }
    for word in crate::profile::rungs_of(profile) {
        let known =
            crate::components::model_common::RUNGS.contains(&word.as_str()) || word == "none";
        if !known {
            said.push(format!(
                "{here}: model \"{id}\" declares the effort rung \"{word}\", which is not on \
                 the ruler ({}) — it will never be chosen",
                crate::components::model_common::RUNGS.join(", ")
            ));
        }
    }
    if profile.get("contextWindow").is_some() && crate::profile::window_of(profile).is_none() {
        said.push(format!(
            "{here}: model \"{id}\" has a \"contextWindow\" that is not a whole number, ignored"
        ));
    }
    said
}

/// The catalog as `/model` should show it: everything configured, plus
/// whatever is ACTUALLY RUNNING if the catalog does not already describe it.
///
/// That last part matters. The running model comes from the environment, and
/// someone who set `LATTICE_MODEL` to something not in their file would
/// otherwise open a list that does not contain the model they are using — and
/// then have no way back to it after picking another.
pub fn listing(running: &Entry) -> Vec<Entry> {
    listing_of(load(), running)
}

/// [`listing`], told what the catalog holds. Split out so the RULE can be
/// tested without a user directory: a test that called `load` would pass or
/// fail depending on whose machine it ran on.
fn listing_of(mut entries: Vec<Entry>, running: &Entry) -> Vec<Entry> {
    if entries.iter().any(|entry| entry.same_target(running)) {
        return entries;
    }
    let mut running = running.clone();
    // Its id must not collide with a configured one, or picking that id would
    // reach a different model than the row it was chosen from.
    if entries.iter().any(|entry| entry.id == running.id) {
        running.id = format!("{}-env", running.id);
    }
    entries.push(running);
    entries
}

impl Entry {
    /// Do these two reach the same model the same way? The id is not part of
    /// it: two names for one endpoint are one entry as far as the listing is
    /// concerned.
    pub fn same_target(&self, other: &Entry) -> bool {
        self.adapter == other.adapter
            && self.model == other.model
            && self.base_url == other.base_url
            && self.key_env == other.key_env
    }
}

/// Find an entry by its short name, in the catalog as `/model` shows it.
pub fn find(id: &str, running: &Entry) -> Option<Entry> {
    listing(running).into_iter().find(|entry| entry.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn literal_keys_for_dash_and_underscore_names_remain_distinct() {
        let entries = merged(json!({"models": {
            "audit-key": {"adapter": "openai", "model": "a", "baseUrl": "https://a.invalid", "apiKey": "fake-first-key"},
            "audit_key": {"adapter": "openai", "model": "b", "baseUrl": "https://b.invalid", "apiKey": "fake-second-key"}
        }}));
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0].key_env, entries[1].key_env);
        for e in entries {
            let expected = if e.id == "audit-key" {
                "fake-first-key"
            } else {
                "fake-second-key"
            };
            assert_eq!(std::env::var(&e.key_env).unwrap(), expected);
        }
    }

    #[test]
    fn adding_to_a_non_utf8_catalog_preserves_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        let original = b"{\"models\":\xff}";
        std::fs::write(&path, original).unwrap();
        assert!(add_to(&path, "new", json!({"apiKeyEnv": "TEST_KEY"})).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    fn entry(id: &str, model: &str) -> Entry {
        Entry {
            id: id.to_string(),
            adapter: "openai".to_string(),
            model: model.to_string(),
            base_url: "https://example.com".to_string(),
            key_env: "SOME_KEY".to_string(),
            profile: None,
        }
    }

    /// `merge`, for a test that only cares about the entries.
    fn merged(document: Value) -> Vec<Entry> {
        merge(&document, "test.json").0
    }

    /// `merge`, for a test that only cares about what it complained about.
    fn complaints(document: Value) -> Vec<String> {
        merge(&document, "test.json").1
    }

    /// Deleting is what makes the catalog a place you can keep tidy, and it is
    /// only honest because nothing ships: what goes out of the file is gone,
    /// rather than coming back at the next start.
    #[test]
    fn a_deleted_model_is_gone_and_the_others_are_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({"models": {
                "keep": {"adapter": "openai", "model": "m1",
                         "baseUrl": "https://a.invalid", "apiKey": "sk-keep-this-one"},
                "drop": {"adapter": "openai", "model": "m2",
                         "baseUrl": "https://b.invalid", "apiKeyEnv": "K2"},
            }}))
            .unwrap(),
        )
        .unwrap();

        remove_from(&path, "drop").unwrap();
        let (entries, said) = load_from(&path);
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "keep");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("sk-keep-this-one"),
            "the key of an entry nobody deleted must survive being rewritten: {text}"
        );
        assert!(!text.contains("\"drop\""));

        // Deleting what is not there says so rather than silently succeeding —
        // "it is gone" and "it was never here" send you to different places
        assert!(remove_from(&path, "drop").unwrap_err().contains("drop"));
    }

    /// A file this program cannot parse is not a file it may rewrite. Whatever
    /// else is in there — other entries, other keys — would go with the rewrite,
    /// and the one thing certain about an unparseable catalog is that nobody
    /// knows what it says.
    #[test]
    fn a_catalog_that_will_not_parse_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(&path, "{\"models\": {\"a\": ").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        assert!(remove_from(&path, "a").is_err());
        assert!(add_to(&path, "b", json!({"adapter": "openai", "model": "m"})).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "the broken file is left exactly as it was"
        );
    }

    #[test]
    fn adding_writes_an_entry_that_reads_back_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");

        // No file yet: the first model is added to nothing
        add_to(
            &path,
            "mine",
            json!({"adapter": "openai", "model": "m",
                   "baseUrl": "https://x.invalid", "apiKeyEnv": "MINE_KEY"}),
        )
        .unwrap();
        let (entries, said) = load_from(&path);
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "m");
        assert_eq!(entries[0].key_env, "MINE_KEY");

        // A name already taken is refused rather than overwritten: doing that
        // under the word "add" would throw away the key the old entry held
        let clash = add_to(
            &path,
            "mine",
            json!({"adapter": "openai", "model": "other"}),
        );
        assert!(clash.unwrap_err().contains("already"));
        assert_eq!(load_from(&path).0[0].model, "m");

        // Short names go in the listing and the preference file, so the shape
        // the schema asks for is checked here rather than at the next start
        for bad in ["", "Mine", "has space", "-leading"] {
            assert!(
                add_to(&path, bad, json!({"adapter": "openai", "model": "m"})).is_err(),
                "{bad:?} should not be accepted as a short name"
            );
        }
    }

    /// The binary ships no models. Whatever is offered came out of the user's
    /// file, which is what lets a model be deleted and stay deleted.
    #[test]
    fn nothing_is_offered_that_the_user_did_not_configure() {
        assert!(
            merged(json!({"models": {}})).is_empty(),
            "an empty catalog is empty — a shipped entry would be a model this \
             binary claims you have, and it cannot know that"
        );
    }

    #[test]
    fn entries_are_offered_in_the_file_s_own_order() {
        let merged = merged(json!({"models": {
            "deepseek": {"adapter": "openai", "model": "deepseek-v4"},
            "local": {"adapter": "openai", "model": "qwen",
                      "baseUrl": "http://localhost:11434", "apiKeyEnv": "NONE"},
        }}));
        assert_eq!(merged[0].id, "deepseek");
        assert_eq!(merged[0].model, "deepseek-v4");
        assert_eq!(merged.last().unwrap().id, "local");
        assert_eq!(merged.len(), 2);
    }

    /// Nothing here is load-bearing: a corrupt or absent catalog leaves an
    /// empty list rather than stopping the program. Startup then says the
    /// catalog is empty and prints what to write, which is the true statement —
    /// where it used to name two models nobody had configured.
    #[test]
    fn a_broken_catalog_leaves_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(&path, "{not json").unwrap();
        let (entries, said) = load_from(&path);
        assert!(entries.is_empty());
        assert!(
            said.iter().any(|s| s.contains("not valid JSON")),
            "a hand-written file that will not parse must SAY so — silence here \
             reads as \"the model I added did not appear\": {said:?}"
        );

        // No file at all is the ordinary state, not a problem to report
        let (entries, said) = load_from(&dir.path().join("absent.json"));
        assert!(entries.is_empty());
        assert!(said.is_empty(), "{said:?}");

        // An entry missing the two things nothing can stand in for is skipped,
        // not defaulted into something that would fail at call time
        let document = json!({"models": {"half": {"adapter": "openai"}}});
        assert!(merged(document.clone()).is_empty());
        assert!(
            complaints(document)[0].contains("half"),
            "it names which one"
        );
    }

    /// Everything a hand-written catalog can get wrong, said out loud. Each of
    /// these is silent otherwise, and each looks like something else: a
    /// misspelled adapter looks like the endpoint misbehaving, an invented
    /// effort rung looks like a model with no effort settings.
    #[test]
    fn a_hand_written_catalog_is_told_what_is_wrong_with_it() {
        let said = |document| complaints(document).join(" | ");

        let typo = said(json!({"models": {
            "x": {"adapter": "antropic", "model": "m"},
        }}));
        assert!(
            typo.contains("antropic") && typo.contains("openai, anthropic"),
            "a misspelled adapter must not fall through to the OpenAI wire \
             format silently: {typo}"
        );

        let stray = said(json!({"models": {
            "x": {"adapter": "openai", "model": "m", "baseurl": "http://h"},
        }}));
        assert!(
            stray.contains("baseurl") && stray.contains("baseUrl"),
            "{stray}"
        );

        let rung = said(json!({"models": {
            "x": {"adapter": "openai", "model": "m", "profile": {"effort": ["turbo"]}},
        }}));
        assert!(
            rung.contains("turbo") && rung.contains("never be chosen"),
            "an invented rung is silently skipped when a request is placed: {rung}"
        );

        let window = said(json!({"models": {
            "x": {"adapter": "openai", "model": "m", "profile": {"contextWindow": "200k"}},
        }}));
        assert!(window.contains("contextWindow"), "{window}");

        // A new entry that names no endpoint and no key variable: both are
        // empty strings rather than absent, and an empty string OVERRIDES the
        // adapter's default rather than falling back to it, so the model
        // fails on its first turn in a way that looks like the endpoint's
        // fault.
        let bare = said(json!({"models": {
            "x": {"adapter": "openai", "model": "m"},
        }}));
        assert!(
            bare.contains("baseUrl") && bare.contains("apiKeyEnv"),
            "{bare}"
        );
        // There is nothing to inherit from any more: every entry names its own
        // endpoint and its own key, because the binary ships no entry to fall
        // back on. So the same complaint stands whatever the id is called.
        let familiar = said(json!({"models": {
            "deepseek": {"adapter": "openai", "model": "deepseek-v5"},
        }}));
        assert!(
            familiar.contains("baseUrl") && familiar.contains("apiKeyEnv"),
            "{familiar}"
        );

        // And a file that is simply fine says nothing at all
        assert!(complaints(json!({"models": {
            "x": {"adapter": "openai", "model": "m", "apiKeyEnv": "K",
                  "baseUrl": "https://example.invalid",
                  "profile": {"contextWindow": 200000, "effort": ["low", "high"]}},
        }}))
        .is_empty());
    }

    /// The hole this closes: before inline profiles, a model added by hand had
    /// no window, no rungs and no usage-field names — so it ran budgeted
    /// against the startup fallback of a million tokens with an empty ladder.
    #[test]
    fn an_entry_can_describe_a_model_this_binary_never_heard_of() {
        let listed = merged(json!({"models": {"kimi": {
            "adapter": "openai",
            "model": "kimi-k2",
            "baseUrl": "https://api.moonshot.cn/v1",
            "apiKeyEnv": "MOONSHOT_API_KEY",
            "profile": {
                "contextWindow": 256000,
                "effort": ["low", "high"],
                "usageFields": {"input": "prompt_tokens"},
            },
        }}}));
        let kimi = listed.iter().find(|e| e.id == "kimi").expect("added");
        assert_eq!(kimi.context_window(), Some(256_000));
        assert_eq!(kimi.effort_rungs(), vec!["low", "high"]);
        assert_eq!(kimi.usage_fields()["input"], "prompt_tokens");
    }

    /// A profile written into an entry wins FIELD BY FIELD, not wholesale.
    /// Correcting one window must not silently discard the effort rungs
    /// nobody meant to touch.
    #[test]
    fn an_inline_profile_corrects_what_it_names_and_inherits_the_rest() {
        let listed = merged(json!({"models": {"deepseek": {
            "adapter": "openai",
            "model": "deepseek-v4-flash",
            "profile": {"contextWindow": 128000},
        }}}));
        let mine = listed.iter().find(|e| e.id == "deepseek").unwrap();
        assert_eq!(
            mine.context_window(),
            Some(128_000),
            "mine wins where I spoke"
        );
        assert_eq!(
            mine.effort_rungs(),
            crate::profile::effort_rungs("deepseek-v4-flash"),
            "and the shipped profile still supplies what I did not"
        );
        assert_eq!(mine.usage_fields()["input"], "prompt_tokens");
    }

    /// The running model always appears. Someone who set LATTICE_MODEL to
    /// something not in their file would otherwise open a list without the
    /// model they are using, and have no way back after picking another.
    #[test]
    fn what_is_running_is_always_in_the_list() {
        let configured = || vec![entry("deepseek", "deepseek-v4"), entry("local", "qwen")];
        let running = entry("mystery", "some-model");
        let listed = listing_of(configured(), &running);
        assert!(
            listed.iter().any(|e| e.same_target(&running)),
            "the running model must be offered: {listed:?}"
        );

        // An empty catalog is the ordinary state of a fresh installation, and
        // the model actually running still has to appear in it
        let listed = listing_of(Vec::new(), &running);
        assert!(
            listed.iter().any(|e| e.same_target(&running)),
            "with nothing configured the running model is the whole list: {listed:?}"
        );

        // Same target under a different short name: one row, not two
        let mut renamed = configured()[0].clone();
        renamed.id = "whatever".to_string();
        assert_eq!(
            listing_of(configured(), &renamed).len(),
            configured().len(),
            "two names for one endpoint are one row"
        );

        // A DIFFERENT target that happens to share a configured name gets a
        // distinct id, or picking that name would reach the other model
        let impostor = entry("deepseek", "not-deepseek-at-all");
        let listed = listing_of(configured(), &impostor);
        assert_eq!(
            listed.iter().filter(|e| e.id == "deepseek").count(),
            1,
            "one row per id: {listed:?}"
        );
        assert!(listed.iter().any(|e| e.same_target(&impostor)));
    }
}
