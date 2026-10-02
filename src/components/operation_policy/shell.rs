//! Conservative, all-or-nothing shell decomposition for authorization only.
//!
//! Adapted from OpenAI Codex, copyright 2025 OpenAI, Apache-2.0:
//! codex-rs/shell-command/src/bash.rs at
//! 444da310e108da16aaeb18fd790b0ac464f08aca. See NOTICE and LICENSE.
//! Lattice changes: expose only the strict script parser, combine literal
//! extraction, reject empty commands, remove shell discovery and dangerous-
//! command heuristics, and use Lattice-specific regression tests. The returned
//! argv list is NOT an execution plan: execute the original script unchanged.

use tree_sitter::{Node, Parser};

pub(super) fn commands(script: &str) -> Option<Vec<Vec<String>>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(script, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut pending = vec![root];
    let mut command_nodes = Vec::new();
    while let Some(node) = pending.pop() {
        let kind = node.kind();
        if node.is_named() {
            if !matches!(
                kind,
                "program"
                    | "list"
                    | "pipeline"
                    | "command"
                    | "command_name"
                    | "word"
                    | "string"
                    | "string_content"
                    | "raw_string"
                    | "number"
                    | "concatenation"
            ) {
                return None;
            }
            if matches!(kind, "word" | "number") && !literal_word(node, script) {
                return None;
            }
            if kind == "command" {
                command_nodes.push(node);
            }
        } else if !matches!(kind, "&&" | "||" | ";" | "|" | "\"" | "'") && !kind.trim().is_empty() {
            return None;
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor));
    }
    command_nodes.sort_by_key(Node::start_byte);
    if command_nodes.is_empty() {
        return None;
    }
    command_nodes
        .into_iter()
        .map(|node| {
            let mut words = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                let word = if child.kind() == "command_name" {
                    let name = child.named_child(0)?;
                    // Do not normalize or reinterpret an executable's spelling.
                    if name.kind() != "word" {
                        return None;
                    }
                    literal(name, script)?
                } else {
                    literal(child, script)?
                };
                words.push(word);
            }
            (!words.is_empty()).then_some(words)
        })
        .collect()
}

fn literal_word(node: Node<'_>, script: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next().is_none()
        && node.utf8_text(script.as_bytes()).is_ok_and(|word| {
            !word.starts_with('=')
                && !word.contains(['{', '}', '*', '?', '[', ']', '\\', '~', '^', '#', '$', '`'])
        })
}

fn literal(node: Node<'_>, script: &str) -> Option<String> {
    let text = node.utf8_text(script.as_bytes()).ok()?;
    match node.kind() {
        "word" | "number" if literal_word(node, script) => Some(text.to_owned()),
        "raw_string" => Some(text.strip_prefix('\'')?.strip_suffix('\'')?.to_owned()),
        "string" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|n| n.kind() != "string_content")
            {
                return None;
            }
            let content = text.strip_prefix('"')?.strip_suffix('"')?;
            if content.as_bytes().windows(2).any(|pair| {
                pair[0] == b'\\' && matches!(pair[1], b'$' | b'`' | b'"' | b'\\' | b'\n')
            }) {
                return None;
            }
            Some(content.to_owned())
        }
        "concatenation" => {
            let mut result = String::new();
            let mut cursor = node.walk();
            for part in node.named_children(&mut cursor) {
                result.push_str(&literal(part, script)?);
            }
            (!result.is_empty()).then_some(result)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_each_command_without_rewriting_the_original_script() {
        let script = "git push origin main && git commit -m 'two words' | cat; pwd || echo fail";
        assert_eq!(
            commands(script).unwrap(),
            vec![
                vec!["git", "push", "origin", "main"],
                vec!["git", "commit", "-m", "two words"],
                vec!["cat"],
                vec!["pwd"],
                vec!["echo", "fail"],
            ]
        );
    }

    #[test]
    fn quoted_operators_are_arguments_not_extra_commands() {
        assert_eq!(
            commands("printf '%s' 'a && b; c | d'").unwrap(),
            vec![vec!["printf", "%s", "a && b; c | d"]]
        );
        assert_eq!(
            commands(r#"rg -g"*.rs" '/usr'"/"local"#).unwrap(),
            vec![vec!["rg", "-g*.rs", "/usr/local"]]
        );
        assert_eq!(
            commands("echo '' \"\"").unwrap(),
            vec![vec!["echo", "", ""]]
        );
    }

    #[test]
    fn unsupported_syntax_never_returns_a_partial_command_list() {
        for suffix in [
            "echo $HOME",
            "echo $(pwd)",
            "echo `pwd`",
            "cat <(pwd)",
            "git push origin main > out",
            "X=y git push origin main",
            "(git push origin main)",
            "for x in a; do echo x; done",
            "echo ~",
            "echo *.rs",
            "echo {a,b}",
            r"echo a\ b",
            "echo HEAD~1",
            "echo HEAD^",
            "echo foo#bar",
            "echo =sh",
            "echo 'unfinished",
            "echo a &&",
            "echo a & echo b",
            "echo a ;; echo b",
            "echo a | | cat",
            r#"echo "\$HOME""#,
        ] {
            let script = format!("git push origin main && {suffix}");
            assert!(
                commands(&script).is_none(),
                "must review the whole script: {script}"
            );
        }
        assert!(commands("").is_none());
        assert!(commands("# a comment").is_none());
    }
}
