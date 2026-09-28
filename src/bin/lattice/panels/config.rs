//! Effective model facts beside their current on-disk configuration sources.

use super::thousands;
use crate::terminal_host::panel_sources::ConfigSources;
use lattice::view::View;

pub(crate) fn rows(view: &dyn View, sources: &ConfigSources) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut say =
        |k: &str, v: String, from: &str| rows.push((k.to_string(), format!("{v:<28}{from}")));
    let from_prefs = |key: &str| {
        if sources.preferences.get(key).is_some() {
            "preferences.json"
        } else {
            "this launch"
        }
    };
    if let Some(now) = view.models().current() {
        say("model", now.id.clone(), from_prefs("model"));
        say("talks", now.model.clone(), "models.json");
        say("dialect", now.dialect.clone(), "models.json");
        say("endpoint", now.endpoint.clone(), "models.json");
        // Only the variable name and presence, never the secret value.
        say(
            "api key",
            format!(
                "{} ({})",
                if now.key_present { "set" } else { "MISSING" },
                now.key_env
            ),
            "models.json",
        );
        match now.window {
            Some(w) => say("context window", thousands(w), "profile"),
            None => say("context window", "not declared".to_string(), "—"),
        }
        say(
            "reads images",
            if now.accepts_images { "yes" } else { "no" }.to_string(),
            "profile",
        );
    }
    if let Some(now) = view.effort().now.clone() {
        say("thinking", now, from_prefs("thinking"));
    }
    say(
        "prompt sections",
        format!("{} in force", sources.prompt_sections),
        &match sources.prompt_overrides {
            0 => "shipped, none overridden".to_string(),
            n => format!("{n} from ~/.lattice/prompts"),
        },
    );
    say(
        "assembly overlay",
        if sources.overlay_present {
            "present"
        } else {
            "none"
        }
        .to_string(),
        "~/.lattice/assembly.json",
    );
    say(
        "trust grants",
        thousands(sources.grants as u64),
        "~/.lattice/trust.json",
    );
    say(
        "ledgers",
        format!("{} kept", sources.ledgers),
        "~/.lattice/ledgers",
    );
    rows.insert(
        0,
        (
            String::new(),
            format!("  {:<28}{}", "in force", "who said so"),
        ),
    );
    rows
}
