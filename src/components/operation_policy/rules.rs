use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::shell;

/// Declaration order is intentional: the strictest matching rule wins.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Prompt,
    Forbidden,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandRule {
    pub pattern: Vec<String>,
    pub decision: Decision,
}

impl CommandRule {
    pub fn matches(&self, argv: &[String]) -> bool {
        !self.pattern.is_empty() && argv.starts_with(&self.pattern)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantMatcher {
    CommandPrefix {
        tool: String,
        prefix: Vec<String>,
    },
    ExactArguments {
        tool: String,
        arguments: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effects: Option<Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub commands: Vec<Vec<String>>,
    pub decomposed: bool,
}

impl Invocation {
    pub fn parse(script: &str) -> Self {
        match shell::commands(script) {
            Some(commands) => Self {
                commands,
                decomposed: true,
            },
            None => Self {
                // This is the actual shell-tools invocation, not an inferred
                // subset of a script that happened to contain familiar words.
                commands: vec![vec!["bash".into(), "-c".into(), script.into()]],
                decomposed: false,
            },
        }
    }

    pub fn decision(
        &self,
        rules: &[CommandRule],
        grants: &[GrantMatcher],
        tool: &str,
        arguments: &Value,
    ) -> Decision {
        self.commands
            .iter()
            .map(|argv| {
                let configured = rules
                    .iter()
                    .filter(|rule| rule.matches(argv))
                    .map(|rule| rule.decision)
                    .max();
                let granted = grants.iter().any(|grant| match grant {
                    GrantMatcher::CommandPrefix {
                        tool: target,
                        prefix,
                    } => target == tool && !prefix.is_empty() && argv.starts_with(prefix),
                    GrantMatcher::ExactArguments {
                        tool: target,
                        arguments: expected,
                        effects,
                    } => target == tool && expected == arguments && effects.is_none(),
                });
                match (configured, granted) {
                    (Some(decision), _) => decision,
                    (None, true) => Decision::Allow,
                    (None, false) => Decision::Prompt,
                }
            })
            .max()
            .unwrap_or(Decision::Prompt)
    }

    /// These are explicit proposals displayed before a person saves a grant.
    /// No shell-wide prefix is inferred from opaque scripts or interpreters.
    pub fn proposals(&self, tool: &str, arguments: &Value) -> Vec<GrantMatcher> {
        let broad = self.commands.iter().any(|argv| {
            let program = argv.first().map(String::as_str).unwrap_or_default();
            let name = std::path::Path::new(program)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(program);
            argv.len() < 2
                || matches!(
                    name,
                    "bash"
                        | "sh"
                        | "zsh"
                        | "dash"
                        | "fish"
                        | "env"
                        | "sudo"
                        | "exec"
                        | "eval"
                        | "command"
                        | "xargs"
                        | "python"
                        | "python3"
                        | "node"
                        | "perl"
                        | "ruby"
                )
                || (program == "git"
                    && argv.get(1).is_some_and(|a| a == "push")
                    && argv.get(2).is_none_or(|a| a.starts_with('-')))
        });
        if !self.decomposed || broad {
            return vec![GrantMatcher::ExactArguments {
                tool: tool.into(),
                arguments: arguments.clone(),
                effects: None,
            }];
        }
        self.commands
            .iter()
            .map(|argv| {
                // The agreed narrow push example retains the remote. Other
                // commands retain every argument rather than guessing a boundary.
                let keep = if argv.len() >= 3
                    && argv[0] == "git"
                    && argv[1] == "push"
                    && !argv[2].starts_with('-')
                {
                    3
                } else {
                    argv.len()
                };
                GrantMatcher::CommandPrefix {
                    tool: tool.into(),
                    prefix: argv[..keep].to_vec(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn push_grant() -> GrantMatcher {
        GrantMatcher::CommandPrefix {
            tool: "Run".into(),
            prefix: vec!["git".into(), "push".into(), "origin".into()],
        }
    }

    #[test]
    fn narrow_prefix_matches_argument_boundaries_not_substrings() {
        for script in [
            "git push origin main",
            "git push origin dev",
            "git push origin --force",
        ] {
            let args = json!({"command":script});
            assert_eq!(
                Invocation::parse(script).decision(&[], &[push_grant()], "Run", &args),
                Decision::Allow
            );
        }
        for script in [
            "git push upstream main",
            "git push origin-other main",
            "git push-other origin main",
            "git commit -m text",
            "git -C repo push origin main",
            "echo git push origin main",
        ] {
            let args = json!({"command":script});
            assert_eq!(
                Invocation::parse(script).decision(&[], &[push_grant()], "Run", &args),
                Decision::Prompt,
                "{script}"
            );
        }
    }

    #[test]
    fn an_allowed_prefix_does_not_authorize_an_uncovered_compound_command() {
        let script = "git push origin main && git commit -m update";
        let invocation = Invocation::parse(script);
        assert_eq!(invocation.commands.len(), 2);
        assert_eq!(
            invocation.decision(&[], &[push_grant()], "Run", &json!({"command":script})),
            Decision::Prompt
        );
    }

    #[test]
    fn strict_rules_win_over_persistent_grants_and_longer_allow_rules() {
        let args = json!({"command":"git push origin main"});
        let invocation = Invocation::parse(args["command"].as_str().unwrap());
        let allow = CommandRule {
            pattern: vec!["git".into(), "push".into(), "origin".into()],
            decision: Decision::Allow,
        };
        for decision in [Decision::Prompt, Decision::Forbidden] {
            let strict = CommandRule {
                pattern: vec!["git".into()],
                decision,
            };
            assert_eq!(
                invocation.decision(
                    &[allow.clone(), strict.clone()],
                    &[push_grant()],
                    "Run",
                    &args
                ),
                decision
            );
            assert_eq!(
                invocation.decision(&[strict, allow.clone()], &[push_grant()], "Run", &args),
                decision
            );
        }
    }

    #[test]
    fn opaque_scripts_are_matched_as_whole_shell_requests() {
        let script = "git push origin main > push.log";
        let args = json!({"command":script});
        let invocation = Invocation::parse(script);
        assert!(!invocation.decomposed);
        assert_eq!(invocation.commands, vec![vec!["bash", "-c", script]]);
        assert_eq!(
            invocation.decision(&[], &[push_grant()], "Run", &args),
            Decision::Prompt
        );
        let proposals = invocation.proposals("Run", &args);
        assert!(matches!(
            proposals.as_slice(),
            [GrantMatcher::ExactArguments { .. }]
        ));
        assert_eq!(
            invocation.decision(&[], &proposals, "Run", &args),
            Decision::Allow
        );
        assert_eq!(
            invocation.decision(
                &[],
                &proposals,
                "Run",
                &json!({"command":"git push origin dev > push.log"})
            ),
            Decision::Prompt
        );
    }

    #[test]
    fn push_proposal_keeps_the_remote_and_other_commands_do_not_guess_scope() {
        assert_eq!(
            Invocation::parse("git push origin main").proposals("Run", &json!({})),
            vec![push_grant()]
        );
        assert_eq!(
            Invocation::parse("cargo test --lib permissions").proposals("Run", &json!({})),
            vec![GrantMatcher::CommandPrefix {
                tool: "Run".into(),
                prefix: vec![
                    "cargo".into(),
                    "test".into(),
                    "--lib".into(),
                    "permissions".into()
                ]
            }]
        );
        assert!(!CommandRule {
            pattern: vec![],
            decision: Decision::Allow
        }
        .matches(&["git".into()]));
    }
}
