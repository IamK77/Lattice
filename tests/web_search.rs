//! Web search with the engine as configuration.
//!
//! Nothing here touches the network. The two things worth testing are the two
//! places the per-backend knowledge lives — how a question is shaped and how
//! an answer is read — and both are pure functions for exactly that reason. A
//! test that needed a key and a live service would run on one machine, on a
//! good day, and would be deleted within a month.
//!
//! The response fixtures are the shapes each service documents. They are the
//! part most likely to rot, and the part where rot is silent: a renamed field
//! yields an empty result list, not an error.

use serde_json::json;

use lattice::components::web_search::{self, Backend};

#[test]
fn a_backend_is_named_in_the_words_people_use() {
    assert_eq!(Backend::parse("brave"), Some(Backend::Brave));
    assert_eq!(
        Backend::parse("Tavily"),
        Some(Backend::Tavily),
        "case is not a decision"
    );
    assert_eq!(
        Backend::parse("searx"),
        Some(Backend::Searxng),
        "the short name too"
    );
    assert_eq!(
        Backend::parse("altavista"),
        None,
        "and an unknown one is not guessed at"
    );
}

// ── Shaping the question ───────────────────────────────────────────────────

#[test]
fn brave_takes_its_key_in_a_header_and_its_query_in_the_url() {
    let ask = web_search::build(
        Backend::Brave,
        "https://api.search.brave.com/res/v1/web/search",
        Some("k-123"),
        "rust lifetimes",
        3,
    );
    assert!(!ask.post, "a GET");
    assert!(
        ask.url.contains("q=rust%20lifetimes"),
        "the query is escaped: {}",
        ask.url
    );
    assert!(ask.url.contains("count=3"), "{}", ask.url);
    assert!(
        ask.headers
            .contains(&("X-Subscription-Token".to_string(), "k-123".to_string())),
        "{:?}",
        ask.headers
    );
    assert!(ask.body.is_none());
}

#[test]
fn tavily_takes_its_key_in_the_body() {
    let ask = web_search::build(
        Backend::Tavily,
        "https://api.tavily.com/search",
        Some("k-123"),
        "rust lifetimes",
        3,
    );
    assert!(ask.post, "a POST");
    let body: serde_json::Value = serde_json::from_str(&ask.body.expect("a body")).unwrap();
    assert_eq!(
        body["api_key"], "k-123",
        "the odd one out: the key is not a header"
    );
    assert_eq!(body["query"], "rust lifetimes");
    assert_eq!(body["max_results"], 3);
    // And the key is nowhere in the headers, where a proxy log would keep it
    assert!(
        !ask.headers.iter().any(|(_, v)| v == "k-123"),
        "{:?}",
        ask.headers
    );
}

#[test]
fn serper_takes_its_key_in_a_header_and_its_query_in_the_body() {
    let ask = web_search::build(
        Backend::Serper,
        "https://google.serper.dev/search",
        Some("k-123"),
        "rust lifetimes",
        3,
    );
    assert!(ask.post);
    assert!(ask
        .headers
        .contains(&("X-API-KEY".to_string(), "k-123".to_string())));
    let body: serde_json::Value = serde_json::from_str(&ask.body.expect("a body")).unwrap();
    assert_eq!(body["q"], "rust lifetimes");
    assert_eq!(body["num"], 3);
}

#[test]
fn searxng_needs_no_key_and_takes_the_address_it_was_given() {
    let ask = web_search::build(Backend::Searxng, "http://box.local:8080/", None, "a b", 3);
    assert!(!ask.post);
    assert_eq!(
        ask.url, "http://box.local:8080/search?q=a%20b&format=json",
        "one slash, not two"
    );
    assert!(ask.body.is_none());
    assert_eq!(
        Backend::Searxng.default_key_env(),
        None,
        "yours to run, yours to protect"
    );
}

/// The silent failure this guards: a `&` or a `+` left raw in a query does not
/// error, it searches for something else.
#[test]
fn a_query_full_of_punctuation_is_escaped_not_pasted() {
    let ask = web_search::build(Backend::Brave, "https://x/y", Some("k"), "a&b=c+d/e ü", 5);
    // The VALUE of q, not the whole query string — `&` and `=` between
    // parameters are the separators doing their job.
    let value = ask
        .url
        .split("?q=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .expect("a q parameter");
    for raw in ['&', '=', '+', '/', ' '] {
        assert!(
            !value.contains(raw),
            "{raw:?} survived into the query value: {value}"
        );
    }
    assert!(
        ask.url.contains("%C3%BC"),
        "non-ascii is UTF-8 percent bytes: {}",
        ask.url
    );
}

// ── Reading the answer ─────────────────────────────────────────────────────

#[test]
fn brave_results_are_read_from_its_own_shape() {
    let body = json!({"web": {"results": [
        {"title": "Lifetimes", "url": "https://doc.rust-lang.org/l", "description": "a scope"},
        {"title": "Second", "url": "https://example.com/2", "description": "another"},
    ]}});
    let hits = web_search::read(Backend::Brave, &body, 5);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["title"], "Lifetimes");
    assert_eq!(hits[0]["url"], "https://doc.rust-lang.org/l");
    assert_eq!(
        hits[0]["snippet"], "a scope",
        "brave calls it `description`"
    );
}

#[test]
fn tavily_and_searxng_share_a_shape() {
    let body = json!({"results": [
        {"title": "One", "url": "https://a", "content": "text of one"},
    ]});
    for backend in [Backend::Tavily, Backend::Searxng] {
        let hits = web_search::read(backend, &body, 5);
        assert_eq!(hits.len(), 1, "{backend:?}");
        assert_eq!(
            hits[0]["snippet"], "text of one",
            "{backend:?} calls it `content`"
        );
    }
}

#[test]
fn serper_calls_the_link_a_link() {
    let body = json!({"organic": [
        {"title": "One", "link": "https://a", "snippet": "text of one"},
    ]});
    let hits = web_search::read(Backend::Serper, &body, 5);
    assert_eq!(hits[0]["url"], "https://a", "serper says `link`, not `url`");
    assert_eq!(hits[0]["snippet"], "text of one");
}

/// A renamed field on the service's side yields an empty list rather than an
/// error, which is why the shapes above are pinned. This one pins the failure
/// mode itself: nothing is invented to fill the gap.
#[test]
fn an_unrecognised_shape_yields_nothing_rather_than_nonsense() {
    let body = json!({"items": [{"headline": "One", "href": "https://a"}]});
    for backend in [
        Backend::Brave,
        Backend::Tavily,
        Backend::Serper,
        Backend::Searxng,
    ] {
        assert!(
            web_search::read(backend, &body, 5).is_empty(),
            "{backend:?} invented results out of a shape it does not know"
        );
    }
}

#[test]
fn only_as_many_results_as_were_asked_for() {
    let rows: Vec<serde_json::Value> = (0..10)
        .map(|i| json!({"title": format!("t{i}"), "url": "https://a", "content": "c"}))
        .collect();
    let hits = web_search::read(Backend::Tavily, &json!({"results": rows}), 3);
    assert_eq!(hits.len(), 3);
}

/// A result is a pointer, not the page. An unbounded snippet would put a whole
/// article into the context for each of five hits.
#[test]
fn a_long_snippet_is_clipped() {
    let long = "x".repeat(5000);
    let body = json!({"results": [{"title": "t", "url": "https://a", "content": long}]});
    let hits = web_search::read(Backend::Tavily, &body, 5);
    let snippet = hits[0]["snippet"].as_str().unwrap();
    assert!(
        snippet.chars().count() < 500,
        "clipped: {}",
        snippet.chars().count()
    );
    assert!(snippet.ends_with('…'), "and says it was clipped");
}

/// Some services answer the question outright. When one does it is carried,
/// because it is often all the model needed and the alternative is fetching a
/// page to rediscover it.
#[test]
fn a_direct_answer_is_carried_when_the_service_gives_one() {
    assert_eq!(
        web_search::direct_answer(Backend::Tavily, &json!({"answer": "42"})),
        Some("42".to_string())
    );
    assert_eq!(
        web_search::direct_answer(Backend::Serper, &json!({"answerBox": {"snippet": "42"}})),
        Some("42".to_string())
    );
    assert_eq!(
        web_search::direct_answer(Backend::Brave, &json!({"answer": "42"})),
        None,
        "brave has no such field; inventing one would put a guess in the result"
    );
}

// ── The declaration ────────────────────────────────────────────────────────

#[test]
fn the_tool_declares_the_network_and_nothing_else() {
    let manifest = web_search::manifest();
    let surface = manifest.capabilities.expect("a surface");
    assert_eq!(
        surface.network,
        vec!["*"],
        "the widest an instance could be"
    );
    assert!(
        surface.writes.is_empty() && !surface.executes && surface.reads.is_empty(),
        "searching reads no file and runs no program: {surface:?}"
    );
    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0]["name"], "WebSearch");
}
