//! Current filesystem facts for the configuration panel, read on demand.
//! These are not recovered conversation state or a startup snapshot.

use serde_json::Value;
use std::path::Path;

pub(super) struct ConfigSources {
    pub preferences: Value,
    pub prompt_overrides: usize,
    pub prompt_sections: usize,
    pub overlay_present: bool,
    pub grants: usize,
    pub ledgers: usize,
}

impl ConfigSources {
    pub fn read(home: &Path, preferences: Value) -> Self {
        let prompt_overrides = std::fs::read_dir(lattice::prompts::user_dir(home))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some())
                    .count()
            })
            .unwrap_or(0);
        let prompt_sections = lattice::prompts::load(Some(home)).len();
        let overlay_present = home.join(".lattice").join("assembly.json").exists();
        let grants = std::fs::read_to_string(home.join(".lattice").join("trust.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["grants"].as_array().map(|a| a.len()))
            .unwrap_or(0);
        let ledgers = lattice::ledgers::all(home).len();
        Self {
            preferences,
            prompt_overrides,
            prompt_sections,
            overlay_present,
            grants,
            ledgers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configuration_sources_are_current_reads_and_preserve_missing_file_fallbacks() {
        let home = tempfile::tempdir().unwrap();
        let first = ConfigSources::read(home.path(), json!({}));
        assert_eq!(first.prompt_overrides, 0);
        assert!(first.prompt_sections > 0);
        assert!(!first.overlay_present);
        assert_eq!(first.grants, 0);
        assert_eq!(first.ledgers, 0);
        let base = home.path().join(".lattice");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("assembly.json"), "{}").unwrap();
        std::fs::write(base.join("trust.json"), r#"{"grants":[{},{}]}"#).unwrap();
        let prompts = lattice::prompts::user_dir(home.path());
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(prompts.join("example.txt"), "example").unwrap();
        std::fs::write(prompts.join("extensionless"), "ignored").unwrap();
        let next = ConfigSources::read(home.path(), json!({"thinking":null}));
        assert!(next.overlay_present);
        assert_eq!(next.grants, 2);
        assert_eq!(next.prompt_overrides, 1);
        assert!(next.preferences.get("thinking").is_some());
        for malformed in ["not json", r#"{"grants":3}"#] {
            std::fs::write(base.join("trust.json"), malformed).unwrap();
            assert_eq!(ConfigSources::read(home.path(), json!({})).grants, 0);
        }
    }
}
