//! What every model adapter shares, whatever wire format it speaks: the
//! material fingerprint rule (contract-wide, not provider-specific), SSE
//! line framing, and the structured error payload with judgment fields.

use serde_json::{json, Value};

use crate::kernel::log::LogReader;
use sha2::{Digest, Sha256};

/// Everything an error has to say, including what it is wrapping.
///
/// Rust errors nest: the outer one names the operation, and WHY it failed
/// lives in its source, and often in that source's source. Printing only the
/// outer one produced the least useful sentence a person can be handed —
/// "error sending request for url (…)" — which says a request failed to a
/// person who already knows a request failed, and withholds the one word
/// that would have told them what to do: refused, timed out, unresolved,
/// certificate.
///
/// Reads as one sentence, outermost first, because that is the order a
/// person needs it in: what was being attempted, then what stopped it.
pub fn full_cause(error: &dyn std::error::Error) -> String {
    let mut said = error.to_string();
    let mut at = error.source();
    while let Some(inner) = at {
        let text = inner.to_string();
        // Wrappers often restate their source; adding it twice helps nobody.
        if !said.contains(&text) {
            said.push_str(": ");
            said.push_str(&text);
        }
        at = inner.source();
    }
    said
}

/// Structured error payload with the judgment fields policies branch on
pub fn error_info(
    code: &str,
    message: &str,
    blame: &str,
    retryable: bool,
    transient: bool,
) -> Value {
    json!({
        "code": code,
        "message": message,
        "blame": blame,
        "retryable": retryable,
        "transient": transient,
    })
}

/// What a live stream chunk was. The reply and the thinking arrive
/// interleaved on one connection and must never be concatenated: one is the
/// answer, the other is the road to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fragment {
    Text(String),
    Reasoning(String),
}

impl Fragment {
    /// The live-notification payload for this chunk. Reply chunks stay
    /// unmarked (the shape every frontend already reads); thinking is marked,
    /// so a frontend that does not know the phase shows nothing rather than
    /// pasting thought into the answer.
    /// The chunk's characters, whichever phase it belonged to.
    pub fn text(&self) -> &str {
        match self {
            Fragment::Text(chunk) | Fragment::Reasoning(chunk) => chunk,
        }
    }

    pub fn note(&self) -> Value {
        match self {
            Fragment::Text(chunk) => json!({"chunk": chunk}),
            Fragment::Reasoning(chunk) => json!({"chunk": chunk, "phase": "reasoning"}),
        }
    }
}

/// Lattice's effort ruler: every rung any known provider offers, weakest
/// first. Deliberately WIDE — wider than any single model — because a model
/// declares which of these it actually has, and a request for one it lacks
/// lands on the nearest it does.
///
/// Going wide rather than picking a middle-sized ladder is what makes the
/// words portable. The alternative, matching by NAME across providers, is
/// actively wrong: DeepSeek's ladder is `high`, `max` — where `high` is its
/// FLOOR — while on Anthropic and OpenAI `high` sits well up the scale. The
/// same word is not the same amount, so only position can be trusted.
///
/// `off` is deliberately absent. It is not the bottom rung but the absence of
/// thinking, and nearest-matching must never turn "do not think" into "think
/// a little" on a model whose floor happens to be higher.
pub const RUNGS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn parse(word: &str) -> Option<Self> {
        let word = word.to_ascii_lowercase();
        let word = if word == "mid" {
            "medium".to_string()
        } else {
            word
        };
        Self::ALL.into_iter().find(|rung| rung.name() == word)
    }

    pub fn name(self) -> &'static str {
        RUNGS[self as usize]
    }

    /// Where this rung sits on the ruler.
    pub fn rank(self) -> usize {
        self as usize
    }

    /// Every rung, weakest first — for a frontend listing the choices.
    pub const ALL: [Effort; 6] = [
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
}

/// Which of a model's OWN rungs to send for a request at `want`.
///
/// Exact if the model has it, otherwise the nearest by position on the ruler.
/// Ties round UP, for two reasons: DeepSeek documents its own aliasing that
/// way (`xhigh` sits one step from both its rungs and it maps to `max`), and
/// the two errors are not equal — guessing high costs money you can see on a
/// bill, guessing low costs answer quality you cannot see at all.
///
/// `available` holds the model's own words. Words this ruler does not know are
/// skipped rather than guessed at: a provider's private setting has a position
/// nobody here can work out, and inventing one would silently mis-aim every
/// request. `None` when nothing usable was declared — the caller falls back to
/// its dialect's own default.
pub fn place(want: Effort, available: &[String]) -> Option<&str> {
    available
        .iter()
        .filter_map(|word| {
            RUNGS
                .iter()
                .position(|rung| *rung == word.to_ascii_lowercase())
                .map(|rank| (word.as_str(), rank))
        })
        .min_by_key(|(_, rank)| (rank.abs_diff(want.rank()), usize::MAX - rank))
        .map(|(word, _)| word)
}

/// The thinking knob, stated dialect-free. Config carries a neutral value and
/// each adapter translates it: absent = send nothing (the provider's own
/// default, and the only safe choice against a generic OpenAI-compatible
/// endpoint that rejects unknown parameters), `false` = off, a ladder word =
/// on at that rung, any other string = that provider's own word, untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thinking {
    Off,
    /// A rung of the neutral ladder; the dialect decides what it means on the
    /// wire.
    Rung(Effort),
    /// A value Lattice does not interpret, forwarded exactly as written. The
    /// escape hatch for an assembly that needs a word the ladder does not
    /// have — removing it would make a provider-specific setting impossible
    /// to express at all.
    Raw(String),
}

/// The thinking setting for THIS call: what the request carries wins, the
/// adapter's own config is the fallback.
///
/// The same order `system` follows, and for the same reason — a station on
/// the ask wire may set it per call, and it must be able to override what the
/// adapter was built with. Without this, changing the setting would mean
/// rebuilding a running component, which nothing in Lattice can do.
pub fn thinking_for_call(payload: &Value, fallback: Option<&Thinking>) -> Option<Thinking> {
    match payload.get("thinking") {
        Some(value) if !value.is_null() => thinking_from_config(Some(value)),
        _ => fallback.cloned(),
    }
}

/// Read the neutral `thinking` config key. Anything unrecognized reads as
/// absent: a knob nobody understands must not silently become a request
/// parameter.
pub fn thinking_from_config(value: Option<&Value>) -> Option<Thinking> {
    match value {
        Some(Value::Bool(false)) => Some(Thinking::Off),
        Some(Value::Bool(true)) => Some(Thinking::Rung(Effort::High)),
        Some(Value::String(word)) if word == "off" || word.is_empty() => Some(Thinking::Off),
        Some(Value::String(word)) => Some(match Effort::parse(word) {
            Some(rung) => Thinking::Rung(rung),
            None => Thinking::Raw(word.clone()),
        }),
        _ => None,
    }
}

/// The pictures attached to a user message, as (media type, base64 body).
///
/// The ledger carries only REFERENCES — one JSON object per line, read line by
/// line, and an inlined image would put megabytes of base64 on a line every
/// reader has to walk past. So the bytes are fetched here, at the moment a call
/// is built, from the directory beside the ledger.
///
/// An attachment that cannot be read is SKIPPED, not fatal. The picture is gone
/// either way, and refusing to send the sentence that came with it helps
/// nobody. Same for one with no media type: a dialect has to tell the provider
/// what it is, and guessing from a content-addressed file name is not possible.
///
/// Shared, because what to read is the same question for every dialect; only
/// how to spell the block differs.
pub fn attached_images(payload: &Value, docs: Option<&std::path::Path>) -> Vec<(String, String)> {
    use base64::Engine;
    let Some(list) = payload.get("images").and_then(Value::as_array) else {
        return Vec::new();
    };
    let Some(dir) = docs else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list {
        let Some(media) = item.get("mediaType").and_then(Value::as_str) else {
            continue;
        };
        let Some(reference) = crate::contracts::document::DocRef::of(item) else {
            continue;
        };
        let Ok(bytes) = reference.read_bytes(dir) else {
            continue;
        };
        out.push((
            media.to_string(),
            base64::engine::general_purpose::STANDARD.encode(&bytes),
        ));
    }
    out
}

/// Render a wake event's payload as one line of user-facing text — what the
/// model reads when a background task, timer, or monitor woke this turn.
/// Shared so both dialects present a wake identically.
pub fn wake_text(payload: &Value) -> String {
    let source = payload["source"].as_str().unwrap_or("wake");
    let mut out = format!("[woken by {source}]");
    if let Some(summary) = payload["summary"].as_str() {
        if !summary.is_empty() {
            out.push(' ');
            out.push_str(summary);
        }
    }
    if !payload["body"].is_null() {
        out.push('\n');
        out.push_str(&payload["body"].to_string());
    }
    out
}

/// The call ids the material actually ANSWERS: every tool result inside it.
///
/// A tool call with no result here must not be replayed as if it had happened.
/// Both wire formats forbid an assistant turn whose tool calls go unanswered,
/// and reject the whole request — so ONE call left hanging (a gate still
/// waiting on a human, an interrupt, a crash) would make every later message
/// in that conversation fail, forever, with no way back. Leaving the call out
/// costs the model one memory; leaving it in costs it the conversation.
pub fn answered_calls(
    parts: &[Value],
    log: &LogReader,
) -> Result<std::collections::HashSet<String>, String> {
    let mut calls = std::collections::HashSet::new();
    // A digest replaces text, not the fact that the call was answered.
    for id in parts.iter().filter_map(|part| {
        part["event"]
            .as_str()
            .or_else(|| part["digest"]["of"].as_str())
    }) {
        let Some(event) = log.header(id).map_err(|e| e.to_string())? else {
            continue;
        };
        if event.event_type == crate::contracts::core_events::TOOL_EXEC_COMPLETED {
            if let Some(call) = event.call.as_deref() {
                calls.insert(call.to_string());
            }
        }
    }
    Ok(calls)
}

/// Both wire formats want an assistant's tool calls followed immediately by
/// their results. The ledger does not order things that way and should not
/// have to: its order is causal, and a background wake or a gate holding one
/// call can put several assistant turns in a row before any result arrives.
/// Translating that causal record into the shape a dialect demands is exactly
/// the adapter's job, so each adapter looks its answers up here and places
/// them where the format needs them.
/// One call, ONE answer — whichever part the index chose.
///
/// A call can end up with two answers on the ledger, and both are honest: the
/// kernel says "this ran past its deadline" and the tool, noticing the same
/// cancellation, says "cancelled, here is what I had". Both are correct
/// history. Neither wire format will take them both: two `tool` messages for
/// one `tool_call_id` is an orphan, and the provider answers 400 — the round
/// dies, and so does every later one, because the pair stays in the material.
///
/// So the ADAPTER picks one. Which is the index's business; this only says
/// whether the part in hand is the one that was picked, so the other is
/// skipped instead of rendered.
pub fn is_chosen_answer(
    answers: &std::collections::HashMap<String, usize>,
    parts: &[Value],
    at: usize,
    log: &LogReader,
) -> Result<bool, String> {
    let Some(call) = answered_call(&parts[at], log)? else {
        // A stream/model interruption settles no tool call. It cannot be a
        // selected tool answer, even when there is no duplicate to suppress.
        return Ok(false);
    };
    Ok(answers.get(&call) == Some(&at))
}

/// Which call this part answers, if it answers one — a completion by its own
/// `call`, an interruption by the request it settled.
pub(crate) fn answered_call(part: &Value, log: &LogReader) -> Result<Option<String>, String> {
    let Some(id) = part["event"]
        .as_str()
        .or_else(|| part["digest"]["of"].as_str())
    else {
        return Ok(None);
    };
    let Some(event) = log.header(id).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    match event.event_type.as_str() {
        crate::contracts::core_events::TOOL_EXEC_COMPLETED => Ok(event.call),
        crate::contracts::core_events::INTERRUPTED => {
            for cause in &event.causes {
                let Some(started) = log.header(cause).map_err(|e| e.to_string())? else {
                    continue;
                };
                if started.event_type == crate::contracts::core_events::TOOL_EXEC_STARTED {
                    if let Some(call) = started.call {
                        return Ok(Some(call));
                    }
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Where in the material each call's ANSWER sits: call id → part index.
///
/// When a call has more than one (see [`is_chosen_answer`]), the TOOL's own
/// completion wins over the kernel's interruption, whichever order they
/// arrived in: the kernel can only say "this ended", the tool can say what
/// it managed to do first.
pub fn answer_index(
    parts: &[Value],
    log: &LogReader,
) -> Result<std::collections::HashMap<String, usize>, String> {
    let mut index = std::collections::HashMap::new();
    for (at, part) in parts.iter().enumerate() {
        let of = part["event"]
            .as_str()
            .or_else(|| part["digest"]["of"].as_str());
        let Some(event) = of
            .map(|id| log.header(id))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten()
        else {
            continue;
        };
        match event.event_type.as_str() {
            crate::contracts::core_events::TOOL_EXEC_COMPLETED => {
                if let Some(call) = event.call.as_deref() {
                    index.insert(call.to_string(), at);
                }
            }
            // A chain the kernel settled: no result exists and none is coming.
            // It still ANSWERS the call — the model must learn that this one
            // ended, or it will ask for the same thing again forever. What it
            // is told is the truth and no more: interrupted, outcome unknown.
            crate::contracts::core_events::INTERRUPTED => {
                for cause in &event.causes {
                    let Some(started) = log.header(cause).map_err(|e| e.to_string())? else {
                        continue;
                    };
                    if started.event_type != crate::contracts::core_events::TOOL_EXEC_STARTED {
                        continue;
                    }
                    if let Some(call) = started.call.as_deref() {
                        index.entry(call.to_string()).or_insert(at);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(index)
}

/// The prompt fragment that explains the line below. An adapter is what
/// renders an interruption into words the model reads, so an adapter is what
/// tells it how to take them — and the answer must not be the reflex ("no
/// result, try again"), because the call may have half happened.
pub const INTERRUPTED_FRAGMENT: &str =
    "A tool result may come back as \"[interrupted: …]\". That call ended with no \
     result and whether it took effect is UNKNOWN — it may have half happened, on \
     disk or over the network. Find out what the state actually is before doing it \
     again.";

/// What an interruption says to the model in place of a result. Never "it
/// failed": a component can die with its work half done, and a model told
/// "failed" retries — which is the one thing that must not happen to a call
/// that may already have deleted a file or spent money.
pub fn interrupted_text(event: &Value) -> String {
    let by = event["by"].as_str().unwrap_or("interrupted");
    // One case where the kernel knows more than "no result came back": the
    // result DID come back and was refused at the door (a malformed letter,
    // most likely a foreign component's schema drift). Saying only "no
    // result" there would understate it — the work almost certainly ran.
    if by == "rejected" {
        return "[interrupted: rejected] this call ran and answered, but its answer was \
                not valid and could not be recorded; what it actually did is unknown"
            .to_string();
    }
    format!(
        "[interrupted: {by}] this call ended without a result; whether it took effect is unknown"
    )
}

/// What a completed call says to the model.
///
/// Three outcomes, not two. `cancelled` is neither success nor failure, and
/// rendering it as a plain result — the old behaviour — was a quiet lie: a
/// command killed at its deadline handed the model its partial output with
/// nothing to say the output was partial, and the model read half a directory
/// listing as the whole of it. What the tool managed to do still stands, so
/// this says both things: it was cut short, and here is how far it got.
pub fn completion_text(payload: &Value) -> String {
    if payload["status"] == "ok" {
        if let Some(text) = payload["modelText"].as_str().filter(|s| !s.is_empty()) {
            return format!("[Condensed tool receipt; details omitted]\n{text}");
        }
    }
    match payload["status"].as_str() {
        Some("error") => {
            let error = &payload["error"];
            error["message"]
                .as_str()
                .or(error.as_str())
                .unwrap_or("error")
                .to_string()
        }
        Some("cancelled") => format!(
            "[cancelled] this call was cut short before it finished; anything it had already \
             done stands. What it produced up to that point: {}",
            payload["result"]
        ),
        _ => payload["result"].to_string(),
    }
}

/// A concise receipt always names the immutable evidence it stands for.
pub fn completion_text_at(payload: &Value, id: &str) -> String {
    let text = completion_text(payload);
    if payload["status"] == "ok" && payload["modelText"].as_str().is_some_and(|s| !s.is_empty()) {
        format!("{text}\nFull tool result at event {id}; read its ledger line for details.")
    } else {
        text
    }
}

/// The material fingerprint rule — one arm per part kind: pointer parts
/// hash their event id, digest parts hash the digest object (so a note is
/// as tamper-proof as an original), inline parts hash the message. Each
/// newline-terminated. Producers (the loop, a context gate) and verifiers
/// (the adapters) share this one implementation.
pub fn material_fingerprint(parts: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        if let Some(event) = part["event"].as_str() {
            hasher.update(event.as_bytes());
        } else if let Some(digest) = part.get("digest") {
            hasher.update(digest.to_string().as_bytes());
        } else {
            hasher.update(part["inline"].to_string().as_bytes());
        }
        hasher.update(b"\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Verify a claimed fingerprint against the parts actually received.
pub fn verify_fingerprint(parts: &[Value], claimed: Option<&str>) -> Result<(), String> {
    let actual = material_fingerprint(parts);
    match claimed {
        Some(claimed) if claimed == actual => Ok(()),
        Some(_) => Err("material fingerprint mismatch; refusing to call the model".to_string()),
        None => Err("material carries no fingerprint".to_string()),
    }
}

/// Splits an SSE byte stream into the JSON payloads of `data:` lines.
/// The buffer stays bytes and only whole lines are decoded — a multi-byte
/// UTF-8 character split across network chunks must never be corrupted.
/// Non-JSON data lines (e.g. the OpenAI-style `[DONE]` sentinel) are skipped.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Value> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                    out.push(value);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_dialects_render_receipts_while_the_audit_keeps_full_results() {
        use crate::contracts::core_events::{self as ce, core_event_decls};
        use crate::kernel::log::EventLog;
        use crate::EventDraft;
        let mut log = EventLog::in_memory(core_event_decls(), "receipts");
        let asked = log.append(EventDraft::new(ce::MODEL_CALL_COMPLETED, &[], json!({"status":"ok","toolCalls":[{"id":"write","tool":"Write","arguments":{}}]})), "model").unwrap();
        let started = log
            .append(
                EventDraft::new(
                    ce::TOOL_EXEC_STARTED,
                    &[&asked.id],
                    json!({"call":"write","tool":"Write","arguments":{}}),
                ),
                "loop",
            )
            .unwrap();
        let completed = log.append(EventDraft::new(ce::TOOL_EXEC_COMPLETED, &[&started.id], json!({"call":"write","status":"ok","modelText":"saved the file","result":{"preview":"FULL_AUDIT_ONLY"}})), "files").unwrap();
        let parts = [json!({"event":asked.id}), json!({"event":completed.id})];
        for messages in [
            crate::components::openai_model::materialize(&parts, &log.reader(), None).unwrap(),
            crate::components::anthropic_model::materialize(&parts, &log.reader(), None).unwrap(),
        ] {
            let text = serde_json::to_string(&messages).unwrap();
            assert!(text.contains("saved the file"));
            assert!(text.contains(&completed.id));
            assert!(!text.contains("FULL_AUDIT_ONLY"));
        }
        assert_eq!(completed.payload["result"]["preview"], "FULL_AUDIT_ONLY");
    }

    #[test]
    fn a_receipt_is_opt_in_and_never_hides_failure_or_changes_the_record() {
        let full = json!({"status": "ok", "result": {"editDiff": "FULL_SNAPSHOT"}, "modelText": "replaced one match"});
        let before = full.clone();
        let text = completion_text(&full);
        assert!(text.contains("replaced one match"));
        assert!(!text.contains("FULL_SNAPSHOT"));
        assert!(completion_text_at(&full, "ev_42_receipt").contains("ev_42_receipt"));
        assert_eq!(full, before);
        let legacy = json!({"status": "ok", "result": {"modelText": "ordinary business data"}});
        assert_eq!(completion_text(&legacy), legacy["result"].to_string());
        for status in ["error", "cancelled"] {
            let mut failed = full.clone();
            failed["status"] = json!(status);
            failed["error"] = json!({"message": "actual failure"});
            let text = completion_text(&failed);
            assert!(!text.contains("replaced one match"));
            assert!(text.contains(if status == "error" {
                "actual failure"
            } else {
                "FULL_SNAPSHOT"
            }));
        }
    }

    /// The rule the whole thinking dial rests on. A station on the ask wire
    /// stamps the setting onto the request; if an adapter read only its own
    /// config, the stamp would be ignored and turning the dial would do
    /// nothing at all — silently, because the ledger would still show the new
    /// setting on every call.
    #[test]
    fn the_request_outranks_the_adapters_own_config() {
        let config = Thinking::Rung(Effort::Low);

        let carried = json!({"thinking": "max"});
        assert_eq!(
            thinking_for_call(&carried, Some(&config)),
            Some(Thinking::Rung(Effort::Max)),
            "what the call carries must win"
        );

        // Nothing carried: the adapter's own setting still decides, so an
        // assembly with no station on the wire behaves as it always did
        for quiet in [json!({}), json!({"thinking": null})] {
            assert_eq!(
                thinking_for_call(&quiet, Some(&config)),
                Some(config.clone()),
                "with nothing carried the config decides: {quiet}"
            );
        }
        assert_eq!(thinking_for_call(&json!({}), None), None);

        // Off must be expressible per call, not just at build time
        assert_eq!(
            thinking_for_call(&json!({"thinking": false}), Some(&config)),
            Some(Thinking::Off)
        );
    }

    /// DeepSeek is the one provider that wrote its OWN aliasing down, which
    /// makes it a free conformance exam: feed it our ruler and the nearest
    /// rule must reproduce exactly what its documentation promises.
    ///
    /// Its ladder is `high`, `max` — and `high` is the FLOOR, which is why
    /// matching by name would be a disaster here: `/effort low` matched to a
    /// word called `high` happens to be right, and `/effort high` matched to
    /// the same word would be the least thinking on offer.
    #[test]
    fn the_nearest_rule_reproduces_deepseeks_own_published_aliases() {
        let deepseek: Vec<String> = ["high", "max"].iter().map(|s| s.to_string()).collect();

        // "for compatibility, low and medium are mapped to high, and xhigh is
        // mapped to max" — api-docs.deepseek.com/guides/thinking_mode
        assert_eq!(place(Effort::Low, &deepseek), Some("high"));
        assert_eq!(place(Effort::Medium, &deepseek), Some("high"));
        assert_eq!(
            place(Effort::Xhigh, &deepseek),
            Some("max"),
            "one step from each rung — the tie must round up, as DeepSeek does"
        );
        // And the two it really has come back untouched
        assert_eq!(place(Effort::High, &deepseek), Some("high"));
        assert_eq!(place(Effort::Max, &deepseek), Some("max"));
    }

    /// The rungs a model lacks at the BOTTOM matter as much as the top.
    /// `gpt-5.2-pro` has no `low`; asking for it should land on that model's
    /// floor rather than failing or reaching upward past it.
    #[test]
    fn a_request_below_a_models_floor_lands_on_the_floor() {
        let pro: Vec<String> = ["medium", "high", "xhigh"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(place(Effort::Minimal, &pro), Some("medium"));
        assert_eq!(place(Effort::Low, &pro), Some("medium"));
        assert_eq!(
            place(Effort::Max, &pro),
            Some("xhigh"),
            "and max is its top"
        );
    }

    /// `max` must always reach the model's ceiling, whatever that ceiling is
    /// called — that is the whole meaning of the word. It falls out of the
    /// nearest rule rather than needing a rule of its own, because `max` is
    /// the highest position on the ruler.
    #[test]
    fn max_reaches_the_top_of_every_ladder_and_minimal_the_bottom() {
        let ladders: [&[&str]; 4] = [
            &["high", "max"],
            &["medium", "high", "xhigh"],
            &["minimal", "low", "medium", "high"],
            &["low", "medium", "high", "xhigh", "max"],
        ];
        for ladder in ladders {
            let owned: Vec<String> = ladder.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                place(Effort::Max, &owned),
                ladder.last().copied(),
                "max must reach the ceiling of {ladder:?}"
            );
            assert_eq!(
                place(Effort::Minimal, &owned),
                ladder.first().copied(),
                "and minimal the floor of {ladder:?}"
            );
        }
    }

    /// Real models exist with exactly one setting (`gpt-5.2-chat-latest`).
    /// Every request lands there; nothing is a special case.
    #[test]
    fn a_model_with_one_rung_answers_every_request_with_it() {
        let only = vec!["medium".to_string()];
        for rung in Effort::ALL {
            assert_eq!(place(rung, &only), Some("medium"), "{rung:?}");
        }
    }

    /// A word off the ruler has no position anyone can work out, so it is
    /// skipped rather than guessed at — and a list of nothing but such words
    /// yields None, which sends the caller to its dialect default instead of
    /// to a wrong rung.
    #[test]
    fn a_word_the_ruler_does_not_know_is_skipped_not_guessed() {
        let odd: Vec<String> = vec!["turbo".to_string(), "high".to_string()];
        assert_eq!(place(Effort::Max, &odd), Some("high"));
        assert_eq!(place(Effort::Minimal, &odd), Some("high"));
        assert_eq!(place(Effort::Max, &["turbo".to_string()]), None);
        assert_eq!(place(Effort::Max, &[]), None);
    }

    /// A ladder word becomes a rung every dialect can translate; anything
    /// else stays a raw value nobody interprets. Reading an unknown word as a
    /// rung would mean guessing what a provider's private setting means.
    #[test]
    fn a_ladder_word_is_a_rung_and_anything_else_is_left_raw() {
        for rung in Effort::ALL {
            assert_eq!(
                thinking_from_config(Some(&json!(rung.name()))),
                Some(Thinking::Rung(rung)),
                "{rung:?}"
            );
        }
        assert_eq!(
            thinking_from_config(Some(&json!("ultra"))),
            Some(Thinking::Raw("ultra".to_string()))
        );
        assert_eq!(
            thinking_from_config(Some(&json!(false))),
            Some(Thinking::Off)
        );
        assert_eq!(
            thinking_from_config(Some(&json!("off"))),
            Some(Thinking::Off)
        );
        // Absent is NOT off: it means send no parameter at all, which is the
        // only safe thing against an endpoint that rejects unknown ones
        assert_eq!(thinking_from_config(None), None);
        assert_eq!(thinking_from_config(Some(&json!(""))), Some(Thinking::Off));
    }
}
