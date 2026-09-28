//! The tools that are NOT in the schema, and the way back to them.
//!
//! Every tool declaration sent with a call costs tokens on that call and on
//! every call after it, and the cost is the smaller half of the problem: a
//! model's ability to pick the right tool falls off once the list runs past a
//! few dozen, and published measurements put the collapse at three-fold. A
//! runtime whose agents install their own tools has a list that grows by
//! design, so this arrives before the list is long rather than after.
//!
//! The shape is the industry's, and the important part of it is WHERE a
//! discovered declaration lands. It must not go into the tool schema: that
//! sits in the cached prefix of every call, and adding to it throws away the
//! cache for the whole conversation behind it. It goes into the RESULT of the
//! search instead — the tail of the conversation, which every turn appends to
//! anyway. Nothing before it is recomputed.
//!
//! What the model gets for free is the NAMES: the fragment below lists them,
//! which is about a tenth of what the declarations would cost and is enough to
//! know that something exists and roughly what to search for. Names in the
//! prompt, shapes on demand.
//!
//! Calling one goes through the resident dispatcher (`UseDeferredTool`),
//! because a provider will not accept a call to a name absent from the schema.
//! The loop unwraps that mechanically, keeping the same call id, so routing,
//! the trust gate and the result all see an ordinary call.

use serde_json::{json, Value};

use crate::contracts::component::{ComponentManifest, PortDecl, RuntimeKind};
use crate::contracts::core_events as ce;
use crate::contracts::event::{EventDraft, EventEnvelope};
use crate::kernel::host::{Component, Ctx};

pub const NAME: &str = "tool-catalog";
pub const FIND_TOOLS: &str = "FindTools";

/// Most matches one search hands back. Five is what the industry's built-in
/// search returns, and it is the point where a result stops being a shortlist.
const DEFAULT_LIMIT: usize = 5;

pub fn tool_decl() -> Value {
    json!({
        "name": FIND_TOOLS,
        "description": "Look up tools that are not in this schema. Searches names, \
            descriptions and argument names for your words, and returns the full \
            declaration of what it finds — call one with `UseDeferredTool`, passing \
            its name and arguments. The names of these tools are listed in your \
            system prompt; search with one of them, or with what you are trying to do.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "a tool name, or words \
                    describing what you want to do"},
                "limit": {"type": "integer"},
            },
            "required": ["query"],
        },
        // Reading a list this process already holds. Nothing is touched.
        "effects": {"reversible": true},
    })
}

pub fn manifest() -> ComponentManifest {
    ComponentManifest {
        name: NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        runtime: RuntimeKind::Inproc,
        entry: format!("builtin:{NAME}"),
        inputs: vec![PortDecl::new("execute", &[ce::TOOL_EXEC_STARTED])],
        outputs: vec![PortDecl::new("outcome", &[ce::TOOL_EXEC_COMPLETED])],
        events: Vec::new(),
        default_wiring: Vec::new(),
        capabilities: None,
        implements: vec!["tool-provider".to_string()],
        tools: vec![tool_decl()],
        // Filled in at startup by `restore`, once the deferred names are known
        // from config. Naming them is the whole trick: a name is a tenth of a
        // declaration and enough to know what to search for.
        prompt: None,
        handle_timeout_ms: Some(5_000),
        concurrency: None,
    }
}

pub struct ToolCatalog {
    /// The tools kept out of the schema. Held by name because the assembly
    /// decides what is common, not the component that provides it.
    deferred: Vec<String>,
    exclusive: bool,
}

impl ToolCatalog {
    pub fn from_config(config: Option<&Value>) -> Self {
        let get = |key: &str| config.and_then(|c| c.get(key));
        Self {
            deferred: get("deferred")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            exclusive: get("exclusive").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

/// The prompt fragment: the names, and how to turn one into a call.
pub fn fragment(deferred: &[String]) -> Option<String> {
    if deferred.is_empty() {
        return None;
    }
    Some(format!(
        "These tools exist but are NOT in your schema, so you cannot call them \
         directly: {}.\nTo use one, call `{FIND_TOOLS}` to get its full declaration, \
         then call it through `UseDeferredTool` with its name and arguments. \
         Searching also finds tools installed during this conversation.",
        deferred.join(", ")
    ))
}

impl Component for ToolCatalog {
    fn restore(&mut self, ctx: &mut Ctx) {
        ctx.set_prompt(fragment(&self.deferred));
    }

    fn handle(&mut self, _port: &str, event: &EventEnvelope, ctx: &mut Ctx) {
        let tool = event.payload["tool"].as_str().unwrap_or_default();
        if tool != FIND_TOOLS {
            if self.exclusive {
                let mut payload = err("tool.unknown", &format!("unknown tool: {tool}"));
                payload["call"] = event.payload["call"].clone();
                ctx.emit(
                    "outcome",
                    EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
                );
            }
            return; // fan-out convention: silence on foreign tools
        }

        let mut payload = match event.payload["arguments"]["query"].as_str() {
            Some(query) => {
                let limit = event.payload["arguments"]["limit"]
                    .as_u64()
                    .map(|n| n.clamp(1, 20) as usize)
                    .unwrap_or(DEFAULT_LIMIT);
                self.search(query, limit, ctx)
            }
            None => err("tool.bad_arguments", "missing 'query' argument"),
        };
        payload["call"] = event.payload["call"].clone();
        ctx.emit(
            "outcome",
            EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&event.id], payload),
        );
    }
}

impl ToolCatalog {
    fn search(&self, query: &str, limit: usize, ctx: &Ctx) -> Value {
        let needles = query_words(query);
        let exact = query.trim();

        // Everything not already in the model's schema: the names the assembly
        // deferred, plus anything installed during this conversation (which is
        // deferred too until it is promoted, and may never be).
        let mut scored: Vec<(u32, String, Value)> = Vec::new();
        for decl in ctx.tool_decls() {
            let name = decl["name"].as_str().unwrap_or_default().to_string();
            let hidden = self.deferred.iter().any(|d| d == &name) || decl["installed"] == true;
            if !hidden {
                continue;
            }
            // Exact intent must not compete with incidental prose matches.
            if name.to_lowercase() == exact.to_lowercase() {
                return json!({"status": "ok", "result": {"tools": [decl], "more": false}});
            }
            let score = score(&decl, &needles);
            if score > 0 {
                scored.push((score, name, decl));
            }
        }
        // RANKED, not first-come. A word like "a" turns up in half the
        // descriptions, so taking the first few in list order hands back
        // whatever happened to be declared earliest and drops the tool the
        // caller was plainly asking for. Ties break by name so the same
        // question gets the same answer.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let more = scored.len() > limit;
        let found: Vec<Value> = scored.into_iter().take(limit).map(|(_, _, d)| d).collect();
        json!({"status": "ok", "result": {"tools": found, "more": more}})
    }
}

/// How well a declaration answers the caller's words. A hit in the NAME is
/// worth far more than one in the prose, because a name is what someone types
/// when they already know what they want, and prose is full of ordinary words
/// that match everything.
fn score(decl: &Value, needles: &[String]) -> u32 {
    let name = decl["name"].as_str().unwrap_or_default().to_lowercase();
    let description = decl["description"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let mut arguments = String::new();
    if let Some(props) = decl["parameters"]["properties"].as_object() {
        for (arg, spec) in props {
            arguments.push(' ');
            arguments.push_str(&arg.to_lowercase());
            arguments.push(' ');
            arguments.push_str(
                &spec["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase(),
            );
        }
    }
    let mut total = 0;
    for needle in needles {
        let hit = word_score(&name, needle, 8)
            + word_score(&description, needle, 2)
            + word_score(&arguments, needle, 1);
        // Covering another distinct query word beats repeating one strong hit.
        total += hit + if hit > 0 { 32 } else { 0 };
    }
    total
}

fn query_words(query: &str) -> Vec<String> {
    let mut words: Vec<_> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    words.truncate(64);
    words
}

fn word_score(text: &str, needle: &str, weight: u32) -> u32 {
    if text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == needle)
    {
        weight * 2
    } else if text.contains(needle) {
        weight
    } else {
        0
    }
}

fn err(code: &str, message: &str) -> Value {
    json!({"status": "error", "error": {
        "code": code,
        "message": message,
        "blame": "request",
        "retryable": false,
        "transient": false,
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repetition_does_not_change_query_weight() {
        assert_eq!(
            query_words("Watch watch FILE file"),
            query_words("file watch")
        );
    }

    #[test]
    fn whole_words_and_query_coverage_rank_before_incidental_substrings() {
        let whole = json!({"name": "A", "description": "read file"});
        let partial = json!({"name": "B", "description": "already filed"});
        let words = query_words("read file");
        assert!(score(&whole, &words) > score(&partial, &words));
        let name_only = json!({"name": "Read"});
        assert!(score(&whole, &words) > score(&name_only, &words));
    }
}
