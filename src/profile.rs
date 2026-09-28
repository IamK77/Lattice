//! Model profiles: one model's temperament, as pure data.
//!
//! The third axis of a model call (dialect / endpoint / PROFILE). The files in
//! `profiles/` were canon that nothing read — a schema, a directory, and no
//! consumer. This makes them live for the one field that cannot be guessed
//! from anywhere else: which effort rungs a model actually has.
//!
//! Embedded with `include_str!` rather than read from disk, because a Lattice
//! binary is delivered as one file and a profile that only exists next to the
//! source tree would be missing exactly where it is needed. The files stay the
//! canon — the schema validates them, and this module is one line per file.
//!
//! Deliberately partial: `contextWindow` and `usageFields` still come from
//! `PresetConfig`, which is a wart. Moving them here is a behaviour change to
//! context budgeting and belongs in its own step, not smuggled into this one.

use serde_json::Value;

/// Every profile shipped with the binary, as (model name, document).
const EMBEDDED: [(&str, &str); 2] = [
    (
        "deepseek-v4-flash",
        include_str!("../profiles/deepseek-v4-flash.json"),
    ),
    (
        "claude-sonnet-5",
        include_str!("../profiles/claude-sonnet-5.json"),
    ),
];

/// The profile for a model, if one ships with this binary.
pub fn lookup(model: &str) -> Option<Value> {
    EMBEDDED
        .iter()
        .find(|(name, _)| *name == model)
        .and_then(|(_, text)| serde_json::from_str(text).ok())
}

/// A model's total context window in tokens — `None` when no profile ships
/// for it.
///
/// None is not a failure and must not become a default: a budget computed
/// against a guessed window is worse than no budgeting, because it fires
/// confidently at the wrong moment. A caller with nothing to go on keeps what
/// it had.
pub fn context_window(model: &str) -> Option<u64> {
    window_of(&lookup(model)?)
}

/// How this model's provider NAMES the usage numbers, as canonical role →
/// native field. Empty when unknown, and each consumer keeps its own default.
pub fn usage_fields(model: &str) -> serde_json::Map<String, Value> {
    lookup(model).map(|p| usage_of(&p)).unwrap_or_default()
}

/// A model's effort rungs, weakest first — empty when unknown.
///
/// Empty is not a failure: it means "nothing declared", and each dialect falls
/// back to its own defaults. Guessing a ladder for an unknown model would aim
/// every request on it wrongly and silently.
pub fn effort_rungs(model: &str) -> Vec<String> {
    lookup(model).map(|p| rungs_of(&p)).unwrap_or_default()
}

// The same three questions asked of a profile DOCUMENT rather than of a model
// name, because a profile no longer only comes from this binary: a catalog
// entry may carry its own, for a model nothing here has ever heard of. Same
// reading either way — one shape, one set of readers.

pub fn window_of(profile: &Value) -> Option<u64> {
    profile.get("contextWindow")?.as_u64()
}

/// The most this model will produce in one reply.
///
/// Shipped in every profile and read by nobody, so every call went out with
/// the adapter's fallback of 4096 — and with thinking on, where the budget
/// counts the thought as well as the answer, a long turn hit that ceiling and
/// came back truncated. A model that says it can write 64k should be allowed
/// to.
pub fn max_output_of(profile: &Value) -> Option<u64> {
    profile.get("maxOutputTokens")?.as_u64()
}

/// Can this model be sent images?
///
/// A property of the MODEL, so it is asked of the profile and not of the
/// dialect or the endpoint: one dialect speaks to models that can and models
/// that cannot, and it is swapping the model that changes the answer.
///
/// Unknown reads as NO. The two failures are not symmetric: refusing to attach
/// an image the model could have read costs a person one retry, while sending
/// one it cannot read costs a rejected call in the middle of a turn.
pub fn accepts_images_of(profile: &Value) -> bool {
    profile
        .get("acceptsImages")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// …asked of a model name, for the profiles this binary ships.
pub fn accepts_images(model: &str) -> bool {
    lookup(model).is_some_and(|p| accepts_images_of(&p))
}

pub fn usage_of(profile: &Value) -> serde_json::Map<String, Value> {
    profile
        .get("usageFields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub fn rungs_of(profile: &Value) -> Vec<String> {
    profile
        .get("effort")
        .and_then(Value::as_array)
        .map(|words| {
            words
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped profile must parse and satisfy the shape its consumers
    /// assume. A profile is data, so this IS its inspection — there is no
    /// exam to fail at boot.
    #[test]
    fn every_shipped_profile_parses_and_names_its_model() {
        for (name, text) in EMBEDDED {
            let profile: Value = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("{name} is not valid JSON: {e}"));
            assert_eq!(
                profile["model"], name,
                "a profile's file name and its `model` field must agree"
            );
        }
    }

    /// The rungs must be words the ruler knows, or nearest-matching silently
    /// skips them and the model looks like it has fewer settings than it does.
    #[test]
    fn declared_rungs_are_words_the_ruler_knows() {
        use crate::components::model_common::RUNGS;
        for (name, _) in EMBEDDED {
            for word in effort_rungs(name) {
                assert!(
                    RUNGS.contains(&word.as_str()) || word == "none",
                    "{name} declares {word:?}, which is not on the ruler"
                );
            }
        }
    }

    #[test]
    fn deepseek_declares_the_two_rungs_its_documentation_lists() {
        // api-docs.deepseek.com/guides/thinking_mode: "high" and "max", where
        // low and medium are aliases for high and xhigh for max.
        assert_eq!(effort_rungs("deepseek-v4-flash"), vec!["high", "max"]);
    }

    /// Both shipped models must state their window and how their provider
    /// names the input count. Without the first the gate cannot budget; without
    /// the second it reads the wrong number and budgets against zero, which
    /// looks exactly like a conversation that never grows.
    #[test]
    fn every_shipped_profile_states_its_window_and_input_field() {
        for (name, _) in EMBEDDED {
            assert!(
                context_window(name).is_some_and(|w| w > 0),
                "{name} states no context window"
            );
            assert!(
                usage_fields(name).contains_key("input"),
                "{name} does not say what its provider calls the input count"
            );
        }
        assert_eq!(context_window("claude-sonnet-5"), Some(200_000));
        assert_eq!(
            usage_fields("deepseek-v4-flash")["input"],
            "prompt_tokens",
            "the two dialects name it differently, which is the whole reason \
             this mapping exists"
        );
    }

    /// Which models can read an image is a fact about the MODEL, so a shipped
    /// profile has to state it — and the two we ship disagree, which is what
    /// makes the field worth having.
    #[test]
    fn a_profile_says_whether_its_model_can_read_an_image() {
        assert!(accepts_images("claude-sonnet-5"));
        assert!(!accepts_images("deepseek-v4-flash"));
        for (name, _) in EMBEDDED {
            assert!(
                lookup(name).is_some_and(|p| p.get("acceptsImages").is_some()),
                "{name} does not say whether it can read an image"
            );
        }
    }

    /// Silence is NO. Offering to attach an image to a model that cannot read
    /// one trades a person's retry for a call rejected mid-turn.
    #[test]
    fn a_model_that_does_not_say_is_taken_as_unable() {
        assert!(!accepts_images("some-model-nobody-shipped"));
        assert!(!accepts_images_of(&serde_json::json!({"model": "x"})));
    }

    #[test]
    fn an_unknown_model_declares_nothing_rather_than_a_guess() {
        assert!(effort_rungs("some-model-nobody-shipped").is_empty());
        assert!(lookup("some-model-nobody-shipped").is_none());
        // A guessed window budgets confidently at the wrong moment, which is
        // worse than not budgeting at all
        assert!(context_window("some-model-nobody-shipped").is_none());
        assert!(usage_fields("some-model-nobody-shipped").is_empty());
    }
}
