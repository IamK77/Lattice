//! The laws both wires obey, over ledgers nobody hand-wrote.
//!
//! Each adapter already pins a handful of built-by-hand ledgers. Those catch
//! the shapes someone thought of. This file pins the LAWS instead and lets a
//! seeded generator build the ledgers: a call answered twice, an answer that
//! arrives before its call, a call nobody ever answered, an interruption
//! followed by a late result, two calls whose results come back swapped.
//!
//! Why these laws and not others: a tool_call_id carrying two answers is not a
//! cosmetic problem. Every dialect refuses the pair, the pair is in the
//! material from then on, and every later turn is refused too — a conversation
//! that cannot be continued, out of a ledger that cannot be edited. The
//! adapter is the last place that can still prevent it.
//!
//! The strongest check here is the last one, and it needs no judgement of mine:
//! two independently written adapters must answer the same set of calls for the
//! same ledger. Where they disagree, one of them is wrong.

use serde_json::{json, Value};

use lattice::core_events as ce;
use lattice::core_events::core_event_decls;
use lattice::{EventDraft, EventLog};

// ── The generator ──────────────────────────────────────────────────────────

/// xorshift64*, so a failing case names a seed and that seed reproduces it.
///
/// Deliberately not a crate: what is hard here is building a ledger the log
/// will accept, not producing random bits, and that part is ours either way.
struct Seeded(u64);

impl Seeded {
    fn new(seed: u64) -> Self {
        // Zero is xorshift's fixed point — it would hand back nothing but zero.
        Self(seed | 1)
    }

    fn bits(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.bits() % n
    }
}

/// A ledger under construction, with the material the gate would have selected
/// out of it. Only some events are material: a tool call is READ off the model
/// turn that asked for it, so `tool.exec_started` stays in the ledger without
/// ever becoming a part — which is exactly the arrangement an interruption has
/// to be traced through.
struct Building {
    log: EventLog,
    parts: Vec<Value>,
    calls: usize,
}

impl Building {
    fn new() -> Self {
        Self {
            log: EventLog::in_memory(core_event_decls(), "main"),
            parts: Vec::new(),
            calls: 0,
        }
    }

    fn put(&mut self, draft: EventDraft, source: &str) -> String {
        self.log.append(draft, source).unwrap().id
    }

    /// Append AND select: this event is in the material the model will see.
    fn material(&mut self, draft: EventDraft, source: &str) -> String {
        let id = self.put(draft, source);
        self.parts.push(json!({ "event": id }));
        id
    }

    fn user(&mut self, text: &str) -> String {
        self.material(
            EventDraft::new(ce::USER_MESSAGE, &[], json!({ "text": text })),
            "ui",
        )
    }

    fn next_call(&mut self) -> String {
        self.calls += 1;
        format!("call_{}", self.calls)
    }

    /// A model turn. With no calls it is a plain reply; with calls it is the
    /// turn those calls belong to.
    fn reply(&mut self, cause: Option<&str>, calls: &[String]) -> String {
        let causes: Vec<&str> = cause.into_iter().collect();
        let mut payload = json!({"status": "ok"});
        if calls.is_empty() {
            payload["text"] = json!("here you go");
        } else {
            payload["toolCalls"] = Value::Array(
                calls
                    .iter()
                    .map(|id| json!({"id": id, "tool": "calc", "arguments": {"numbers": [1, 2]}}))
                    .collect(),
            );
        }
        self.material(
            EventDraft::new(ce::MODEL_CALL_COMPLETED, &causes, payload),
            "model",
        )
    }

    /// The request event. Never material — the model already knows it asked.
    fn started(&mut self, cause: &str, call: &str) -> String {
        self.put(
            EventDraft::new(
                ce::TOOL_EXEC_STARTED,
                &[cause],
                json!({"call": call, "tool": "calc", "arguments": {"numbers": [1, 2]}}),
            ),
            "tools",
        )
    }

    fn completed(&mut self, cause: &str, call: &str) {
        self.material(
            EventDraft::new(
                ce::TOOL_EXEC_COMPLETED,
                &[cause],
                json!({"call": call, "status": "ok", "result": 3.0}),
            ),
            "tools",
        );
    }

    /// The other ending. It carries no call id of its own — the only way back
    /// to the call is through the request it was caused by.
    fn interrupted(&mut self, started: &str) {
        self.material(
            EventDraft::new(ce::INTERRUPTED, &[started], json!({"by": "core"})),
            "core",
        );
    }
}

/// One turn of a conversation, chosen by the generator. The awkward ones are
/// here on purpose: they are the states a real ledger reaches after a restart,
/// a crash, a gate re-sending a request, or two tools finishing out of order.
fn turn(b: &mut Building, rng: &mut Seeded) {
    match rng.below(9) {
        0 => {
            b.user("what is 1 + 2?");
        }
        1 => {
            let said = b.user("say something");
            b.reply(Some(&said), &[]);
        }
        2 => {
            // The ordinary shape: one call, one result.
            let call = b.next_call();
            let asked = b.reply(None, std::slice::from_ref(&call));
            let started = b.started(&asked, &call);
            b.completed(&started, &call);
        }
        3 => {
            // Two calls in one turn, results possibly swapped — the ledger is
            // ordered causally, not conversationally.
            let (a, c) = (b.next_call(), b.next_call());
            let asked = b.reply(None, &[a.clone(), c.clone()]);
            let sa = b.started(&asked, &a);
            let sc = b.started(&asked, &c);
            if rng.below(2) == 0 {
                b.completed(&sa, &a);
                b.completed(&sc, &c);
            } else {
                b.completed(&sc, &c);
                b.completed(&sa, &a);
            }
        }
        4 => {
            // Settled instead of answered: the call ended, outcome unknown.
            let call = b.next_call();
            let asked = b.reply(None, std::slice::from_ref(&call));
            let started = b.started(&asked, &call);
            b.interrupted(&started);
        }
        5 => {
            // Asked, and nothing ever came back — the state a ledger is left
            // in when the process dies before it can be settled.
            let call = b.next_call();
            let asked = b.reply(None, std::slice::from_ref(&call));
            b.started(&asked, &call);
        }
        6 => {
            // Two results for one call. The ledger may legally hold both —
            // the kernel will not write a second ending, but a component that
            // was already working can still report one. Whoever renders it has
            // to pick one.
            let call = b.next_call();
            let asked = b.reply(None, std::slice::from_ref(&call));
            let started = b.started(&asked, &call);
            b.completed(&started, &call);
            b.completed(&started, &call);
        }
        7 => {
            // Settled, and then the tool that was still running answered.
            let call = b.next_call();
            let asked = b.reply(None, std::slice::from_ref(&call));
            let started = b.started(&asked, &call);
            b.interrupted(&started);
            b.completed(&started, &call);
        }
        _ => {
            // Two calls, one answer. The turn cannot be replayed whole.
            let (a, c) = (b.next_call(), b.next_call());
            let asked = b.reply(None, &[a.clone(), c.clone()]);
            let sa = b.started(&asked, &a);
            b.started(&asked, &c);
            b.completed(&sa, &a);
        }
    }
}

fn ledger(seed: u64, turns: usize) -> Building {
    let mut rng = Seeded::new(seed);
    let mut b = Building::new();
    for _ in 0..turns {
        turn(&mut b, &mut rng);
    }
    b
}

// ── Reading each wire back ─────────────────────────────────────────────────

/// A call block or an answer block, in the order the wire carries them.
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    Call(String),
    Answer(String),
}

fn anthropic_slots(messages: &[Value]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for message in messages {
        let Some(content) = message["content"].as_array() else {
            continue;
        };
        for block in content {
            match block["type"].as_str() {
                Some("tool_use") => {
                    slots.push(Slot::Call(block["id"].as_str().unwrap_or("").to_string()))
                }
                Some("tool_result") => slots.push(Slot::Answer(
                    block["tool_use_id"].as_str().unwrap_or("").to_string(),
                )),
                _ => {}
            }
        }
    }
    slots
}

fn openai_slots(messages: &[Value]) -> Vec<Slot> {
    let mut slots = Vec::new();
    for message in messages {
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                slots.push(Slot::Call(call["id"].as_str().unwrap_or("").to_string()));
            }
        }
        if message["role"] == "tool" {
            slots.push(Slot::Answer(
                message["tool_call_id"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    slots
}

fn answered(slots: &[Slot]) -> Vec<String> {
    let mut ids: Vec<String> = slots
        .iter()
        .filter_map(|s| match s {
            Slot::Answer(id) => Some(id.clone()),
            Slot::Call(_) => None,
        })
        .collect();
    ids.sort();
    ids
}

/// The three laws that hold whatever the ledger looked like, checked on one
/// wire's rendering of it.
fn check_pairing(slots: &[Slot], wire: &str, seed: u64) {
    let mut open: Vec<String> = Vec::new();
    let mut closed: Vec<String> = Vec::new();
    for slot in slots {
        match slot {
            Slot::Call(id) => {
                assert!(
                    !open.contains(id) && !closed.contains(id),
                    "{wire}/seed {seed}: {id} is asked for twice in one material"
                );
                open.push(id.clone());
            }
            Slot::Answer(id) => {
                assert!(
                    !closed.contains(id),
                    "{wire}/seed {seed}: {id} is answered twice — the pair every \
                     dialect refuses, and it would stay in the material for good"
                );
                let at = open.iter().position(|o| o == id);
                let at = at.unwrap_or_else(|| {
                    panic!("{wire}/seed {seed}: {id} is answered, but nothing asked for it")
                });
                open.remove(at);
                closed.push(id.clone());
            }
        }
    }
    assert!(
        open.is_empty(),
        "{wire}/seed {seed}: {open:?} was asked for and never answered — a call \
         with no ending must be left out of the material, not replayed"
    );
}

/// This wire additionally requires each assistant turn to be followed
/// immediately by its own results, with nothing in between.
fn check_openai_adjacency(messages: &[Value], seed: u64) {
    for (at, message) in messages.iter().enumerate() {
        let Some(calls) = message["tool_calls"].as_array() else {
            continue;
        };
        let mut want: Vec<String> = calls
            .iter()
            .map(|c| c["id"].as_str().unwrap_or("").to_string())
            .collect();
        let mut next = at + 1;
        while !want.is_empty() {
            let following = messages.get(next).unwrap_or_else(|| {
                panic!("seed {seed}: turn {at} asked for {want:?} and the material ends")
            });
            assert_eq!(
                following["role"], "tool",
                "seed {seed}: turn {at} is separated from its results by a {:?} message",
                following["role"]
            );
            let id = following["tool_call_id"].as_str().unwrap_or("").to_string();
            let found = want.iter().position(|w| *w == id).unwrap_or_else(|| {
                panic!("seed {seed}: turn {at} is followed by a result for {id}, which it never asked for")
            });
            want.remove(found);
            next += 1;
        }
    }
}

// ── The laws ───────────────────────────────────────────────────────────────

/// Seeds are fixed rather than drawn from the clock: a suite whose inputs
/// change per run reports a failure that the next run cannot reproduce.
const SEEDS: [u64; 64] = {
    let mut seeds = [0u64; 64];
    let mut i = 0;
    while i < 64 {
        // An odd, well-spread constant per index — no clock, no randomness.
        seeds[i] = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        i += 1;
    }
    seeds
};

#[test]
fn no_call_is_ever_answered_twice_on_either_wire() {
    for seed in SEEDS {
        let b = ledger(seed, 12);
        let reader = b.log.reader();

        let anthropic =
            lattice::components::anthropic_model::materialize(&b.parts, &reader, None).unwrap();
        check_pairing(&anthropic_slots(&anthropic), "anthropic", seed);

        let openai =
            lattice::components::openai_model::materialize(&b.parts, &reader, None).unwrap();
        check_pairing(&openai_slots(&openai), "openai", seed);
    }
}

#[test]
fn each_turn_is_followed_immediately_by_its_own_results() {
    for seed in SEEDS {
        let b = ledger(seed, 12);
        let openai =
            lattice::components::openai_model::materialize(&b.parts, &b.log.reader(), None)
                .unwrap();
        check_openai_adjacency(&openai, seed);
    }
}

/// The differential check, and the one that needs no judgement of mine: two
/// adapters written independently must agree on which calls the material
/// answers. A disagreement is not a matter of taste — one of them is wrong.
#[test]
fn both_wires_answer_the_same_calls_for_the_same_ledger() {
    for seed in SEEDS {
        let b = ledger(seed, 12);
        let reader = b.log.reader();
        let anthropic =
            lattice::components::anthropic_model::materialize(&b.parts, &reader, None).unwrap();
        let openai =
            lattice::components::openai_model::materialize(&b.parts, &reader, None).unwrap();
        assert_eq!(
            answered(&anthropic_slots(&anthropic)),
            answered(&openai_slots(&openai)),
            "seed {seed}: the two dialects disagree about which calls this \
             ledger answers"
        );
    }
}

/// Materializing is a reading, not a move: the same ledger read twice is the
/// same material. This is the whole call-it-again obligation for this slice,
/// which is pure — there is no state here to advance.
#[test]
fn reading_the_same_ledger_twice_gives_the_same_material() {
    for seed in SEEDS {
        let b = ledger(seed, 12);
        let reader = b.log.reader();
        for wire in ["anthropic", "openai"] {
            let once = match wire {
                "anthropic" => {
                    lattice::components::anthropic_model::materialize(&b.parts, &reader, None)
                }
                _ => lattice::components::openai_model::materialize(&b.parts, &reader, None),
            }
            .unwrap();
            let again = match wire {
                "anthropic" => {
                    lattice::components::anthropic_model::materialize(&b.parts, &reader, None)
                }
                _ => lattice::components::openai_model::materialize(&b.parts, &reader, None),
            }
            .unwrap();
            assert_eq!(once, again, "{wire}/seed {seed}");
        }
    }
}
