//! Setup-only presentation language. Message identifiers never drive workflow decisions.
use serde::Deserialize;
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Language {
    #[default]
    English,
    Chinese,
}
impl Language {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::English),
            "zh-CN" => Some(Self::Chinese),
            _ => None,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }
    pub fn detect(saved: Option<&str>, locale: Option<&str>) -> Self {
        saved.and_then(Self::from_code).unwrap_or_else(|| {
            if locale.is_some_and(|s| {
                s.to_ascii_lowercase().split(['_', '-', '.', '@']).next() == Some("zh")
            }) {
                Self::Chinese
            } else {
                Self::English
            }
        })
    }
    pub fn environment() -> Option<String> {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty()))
    }
}

macro_rules! messages {
    ($($name:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
        pub(super) enum Id { $($name),* }
        #[cfg(test)]
        const ALL: &[Id] = &[$(Id::$name),*];
    };
}
messages! {
    Welcome, NoCatalog, Exited, Failure, RawError, Back, Exit, Continue, LanguageMenu, LanguageChoice, LanguageSaved, LanguageNotSaved,
    SelectHelp, MultiHelp, InputHelp, SecretHelp, ConfirmHelp, Yes, No, Unknown, Enabled, Disabled,
    Home, Provider, Custom, RepairEntry, RepairLaunch, UnsafeCatalog, CheckAgain, LaunchOverride,
    Protocol, Recommended, Compatibility, Endpoint, ConnectionSummary, ConnectionMenu, UseConnection, EditConnection, CredentialMenu, LocalKey, EnvKey, ApiKey, EnvName, KeyWarning, HttpWarning, ReplaceKey,
    BadUrl, BadModel, BadName, DuplicateName, BadKey, BadEnv, BadTokens, BadUsage, BadEffort,
    ModelMenu, FetchModels, ManualModel, CachedModels, KeepModel, DiscoveryNotice, DiscoveryFailed, EmptyModels, AvailableModels, ModelIdentifier,
    Review, ReviewSummary, LocalName, DefaultModel, SetDefault, KeepDefault, CredentialLocal, CredentialEnv, SaveContinue, EditModel, EditCapabilities, EditName, ToggleDefault, EditProvider, HomeBack,
    CapabilitiesTitle, CapabilitiesMenu, LimitsMissing, EditLimits, SelectCapabilities, EditEffort, Advanced, ContextWindow, OutputLimit, ImageInput, EffortRungs, WebSearch, ImageGeneration, UsageMapping,
    SourceBundled, SourceRemote, SourceConflict, SourceInputCeiling, SourceProtocol, SourceUnknownEffort, SourceUnknownImage, SourceHosted, SourceEdited, SourceMissing, FieldSummary,
    ContextInput, OutputInput, TokenHelp, SwitchHelp, HostedUnavailable, ImageInputChoice, ImageGenerationChoice, SwitchMenu, EffortCustomNotice, EffortHelp, EffortNone, EffortMenu, AdvancedMenu, CustomEffort, EffortInput, UsageInput,
    NotSaved, Saved, ChangedAfterSave, MissingKey, DefaultNotSaved, RecoveryMenu, ContinueOnce, RetryDefault,
    TestNotice, TestMenu, SkipTest, SendTest, ChangeConfiguration, TestSuccess, EnterTui, TestFailure,
}

#[derive(Deserialize)]
struct Translation {
    en: String,
    zh: String,
}
pub(super) fn phrase(language: Language, id: Id) -> &'static str {
    let entry = &catalog()[&id];
    match language {
        Language::English => &entry.en,
        Language::Chinese => &entry.zh,
    }
}
fn catalog() -> &'static BTreeMap<Id, Translation> {
    static CATALOG: OnceLock<BTreeMap<Id, Translation>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("messages.json")).expect("tested setup translations")
    })
}

pub(super) fn text(language: Language, id: Id, args: &[&str]) -> String {
    let entry = &catalog()[&id];
    let template = match language {
        Language::English => &entry.en,
        Language::Chinese => &entry.zh,
    };
    // Substitute once: a model name containing `{1}` is data, not another template.
    let mut output = String::new();
    let mut rest = template.as_str();
    while let Some((before, after)) = rest.split_once('{') {
        output.push_str(before);
        let (index, tail) = after.split_once('}').expect("tested message placeholder");
        output.push_str(args[index.parse::<usize>().expect("numbered placeholder")]);
        rest = tail;
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_languages_cover_every_message_with_matching_placeholders() {
        assert_eq!(catalog().len(), ALL.len());
        for id in ALL {
            let t = &catalog()[id];
            let placeholders = |s: &str| {
                s.split('{')
                    .skip(1)
                    .map(|part| part.split_once('}').unwrap().0.parse::<usize>().unwrap())
                    .collect::<Vec<_>>()
            };
            assert!(!t.en.is_empty() && !t.zh.is_empty(), "{id:?}");
            let mut en = placeholders(&t.en);
            let mut zh = placeholders(&t.zh);
            en.sort();
            zh.sort();
            assert_eq!(en, zh, "{id:?}");
            let args = vec!["{1}"; en.iter().max().map_or(0, |n| n + 1)];
            for language in [Language::English, Language::Chinese] {
                let _ = text(language, *id, &args);
            }
        }
        assert!(text(Language::English, Id::RepairEntry, &["{1}", "model"]).contains("{1}"));
    }
    #[test]
    fn explicit_language_wins_over_locale_and_unknown_codes_fall_back() {
        assert_eq!(
            Language::detect(Some("en"), Some("zh_CN.UTF-8")),
            Language::English
        );
        assert_eq!(
            Language::detect(Some("zh-CN"), Some("C")),
            Language::Chinese
        );
        for locale in ["zh_CN.UTF-8", "zh-TW", "ZH_hans"] {
            assert_eq!(Language::detect(None, Some(locale)), Language::Chinese);
        }
        for locale in [None, Some("C"), Some("en_US.UTF-8"), Some("de_DE")] {
            assert_eq!(Language::detect(Some("invalid"), locale), Language::English);
        }
    }
}
