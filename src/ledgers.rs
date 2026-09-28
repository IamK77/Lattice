//! Where conversations are kept, and how to find one again.
//!
//! Three programs used to keep their records in three directories under three
//! naming schemes — `tui/20260729-074225.jsonl` by timestamp, `streams/main.jsonl`
//! by whatever a client called itself, `chat/` for the example — with nothing
//! linking them. Finding "the conversation about the search component" meant
//! grepping eighty-odd files one at a time.
//!
//! One directory now, one naming scheme, and an index. The name starts with
//! the time so that sorting by name is sorting by time, and carries the host
//! so two programs cannot collide.
//!
//! The index is DERIVED: one line per conversation, giving its file, when it
//! ran, how big it got, and its opening line as a title. Losing it costs
//! nothing — [`rebuild`] reads the ledgers back. That is why it is allowed to
//! be written cheaply, by appending, with the last line for a stream winning.

use std::path::{Path, PathBuf};

use serde_json::Value;

mod source;

/// The one directory conversations are written to from now on.
pub fn dir(home: &Path) -> PathBuf {
    home.join(".lattice").join("ledgers")
}

/// Every directory a conversation might be found in, newest scheme first.
///
/// The older ones are still read. Nothing is moved: a record already written
/// is history, and rewriting where it lives to satisfy a naming scheme would
/// be churn for its own sake. New conversations go to the first.
pub fn search_path(home: &Path) -> Vec<PathBuf> {
    let lattice = home.join(".lattice");
    vec![
        lattice.join("ledgers"),
        lattice.join("tui"),
        lattice.join("chat"),
    ]
}

/// The name a fresh conversation is written under: sortable by time, and
/// saying which program opened it.
pub fn name_for(stamp: &str, host: &str, stream: Option<&str>) -> String {
    match stream {
        Some(stream) => format!("{stamp}-{host}-{stream}.ledger"),
        None => format!("{stamp}-{host}.ledger"),
    }
}

/// Resolve a named ledger without abandoning an existing legacy stream.
/// A published segmented copy takes precedence over its retained original.
pub fn named_path(directory: &Path, name: &str) -> PathBuf {
    let segmented = directory.join(format!("{name}.ledger"));
    let legacy = directory.join(format!("{name}.jsonl"));
    if segmented.exists() || !legacy.exists() {
        segmented
    } else {
        legacy
    }
}

/// Stream physical lines across the volumes of one logical ledger. This does
/// not acquire a writer lease, repair source, or load the whole history.
pub fn lines(path: &Path) -> std::io::Result<impl Iterator<Item = std::io::Result<String>>> {
    source::lines(path)
}

/// Visit only newly committed complete lines after a logical byte cursor.
/// The caller must commit its accumulated state only when this returns Ok.
pub fn visit_appended(
    path: &Path,
    offset: u64,
    visit: impl FnMut(&crate::EventEnvelope) -> std::io::Result<()>,
) -> std::io::Result<u64> {
    source::visit_appended(path, offset, visit)
}

/// Every ledger this project has, newest first — the ones opened in `here`.
///
/// Conversations from every project share one directory, so "continue the last
/// one" has to mean the last one HERE. It did not: it took the newest across
/// the whole machine, so `eva -c` in one project could pick up another's.
///
/// A ledger that never recorded where it was opened (written before the field
/// existed) is offered to everyone rather than to nobody — the alternative is
/// a person's whole history becoming unreachable by name.
pub fn here(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    all(home)
        .into_iter()
        .filter(|p| belongs_here(p, cwd))
        .collect()
}

/// Continuing needs one match, not a summary of every older conversation.
pub fn latest_here(home: &Path, cwd: &Path) -> Option<PathBuf> {
    all(home).into_iter().find(|p| belongs_here(p, cwd))
}

fn belongs_here(path: &Path, cwd: &Path) -> bool {
    let location = source::cwd(path).ok().flatten();
    location.is_none_or(|theirs| Path::new(&theirs) == cwd)
}

/// Find the newest nonempty location. Memory is bounded by one line, and
/// recent launches are near the tail. Missing/invalid records do not erase
/// the last known location, matching the full summary's selection rule.
fn recorded_cwd(
    reader: &mut (impl std::io::Read + std::io::Seek),
) -> std::io::Result<Option<String>> {
    use std::io::SeekFrom;
    #[derive(serde::Deserialize)]
    struct Record {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        payload: Location,
    }
    #[derive(Default, serde::Deserialize)]
    struct Location {
        #[serde(default)]
        cwd: Value,
    }
    fn location(reversed: &mut [u8]) -> Option<String> {
        reversed.reverse();
        // Unknown fields are skipped, not built into a whole event tree.
        let record: Record = serde_json::from_slice(reversed).ok()?;
        if !matches!(
            record.kind.as_str(),
            "core.stream.opened" | "core.stream.resumed"
        ) {
            return None;
        }
        record
            .payload
            .cwd
            .as_str()
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_string)
    }
    let mut end = reader.seek(SeekFrom::End(0))?;
    let mut buffer = [0u8; 8192];
    let mut line = Vec::new();
    while end > 0 {
        let count = end.min(buffer.len() as u64) as usize;
        end -= count as u64;
        reader.seek(SeekFrom::Start(end))?;
        reader.read_exact(&mut buffer[..count])?;
        for byte in buffer[..count].iter().rev() {
            if *byte == b'\n' {
                if let Some(cwd) = location(&mut line) {
                    return Ok(Some(cwd));
                }
                line.clear();
            } else {
                line.push(*byte);
            }
        }
    }
    Ok(location(&mut line))
}

/// Every ledger across the search path, newest first.
pub fn all(home: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = search_path(home)
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl" || e == "ledger"))
        .filter(|p| p.file_name().is_some_and(|n| n != "index.jsonl"))
        .filter(|p| {
            p.extension().is_none_or(|extension| extension != "jsonl")
                || !crate::kernel::migrate::retained_original(p)
        })
        .filter(|p| {
            crate::EventLog::source_paths(p).is_ok_and(|files| {
                files
                    .first()
                    .is_some_and(|file| file.metadata().is_ok_and(|m| m.is_file() && m.len() > 0))
            })
        })
        .collect();
    // By name, which is by time — the file's own mtime moves when a
    // conversation is resumed, and "most recent" should mean the newest
    // conversation, not the one most recently touched.
    found.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    found.reverse();
    found
}

/// What one conversation is, in the terms someone looking for it would use.
/// One day's model usage, summed across every conversation that day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Day {
    pub calls: u64,
    pub prompt: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    pub conversations: u64,
}

/// Usage per day, read out of the ledgers themselves.
///
/// There is no second book: every conversation already records what each call
/// cost, with the time it happened. The cost of that is honest and worth
/// saying — history reaches back exactly as far as the ledgers do, so
/// compacting or deleting old conversations takes their numbers with them.
pub fn usage_by_day(home: &Path) -> std::collections::BTreeMap<String, Day> {
    let mut days: std::collections::BTreeMap<String, Day> = std::collections::BTreeMap::new();
    for path in all(home) {
        let Ok(lines) = source::lines(&path) else {
            continue;
        };
        let mut file_days: std::collections::BTreeMap<String, Day> = Default::default();
        let mut complete = true;
        let mut counted_this_file: std::collections::HashSet<String> = Default::default();
        for line in lines {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    eprintln!("cannot read usage for {}: {error}", path.display());
                    complete = false;
                    break;
                }
            };
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if event["type"] != crate::core_events::MODEL_CALL_COMPLETED {
                continue;
            }
            let Some(day) = event["time"].as_str().and_then(|t| t.get(..10)) else {
                continue;
            };
            let usage = &event["payload"]["usage"];
            let at = |path: &str| -> u64 {
                let mut cur = usage;
                for step in path.split('.') {
                    match cur.get(step) {
                        Some(next) => cur = next,
                        None => return 0,
                    }
                }
                cur.as_u64().unwrap_or(0)
            };
            let pick = |names: &[&str]| names.iter().map(|n| at(n)).find(|v| *v > 0).unwrap_or(0);
            let entry = file_days.entry(day.to_string()).or_default();
            entry.calls += 1;
            entry.prompt += pick(&["prompt_tokens", "input_tokens"]);
            entry.cached += pick(&[
                "prompt_tokens_details.cached_tokens",
                "input_tokens_details.cached_tokens",
                "prompt_cache_hit_tokens",
                "cache_read_input_tokens",
            ]);
            entry.output += pick(&["completion_tokens", "output_tokens"]);
            entry.reasoning += pick(&[
                "completion_tokens_details.reasoning_tokens",
                "output_tokens_details.reasoning_tokens",
            ]);
            if counted_this_file.insert(day.to_string()) {
                entry.conversations += 1;
            }
        }
        if complete {
            for (day, value) in file_days {
                let entry = days.entry(day).or_default();
                entry.calls += value.calls;
                entry.prompt += value.prompt;
                entry.cached += value.cached;
                entry.output += value.output;
                entry.reasoning += value.reasoning;
                entry.conversations += value.conversations;
            }
        }
    }
    days
}

mod summary;
pub use summary::Summary;

pub fn summarize(path: &Path) -> Option<Value> {
    let mut summary = summary::Summary::default();
    for line in source::lines(path).ok()? {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            summary.invalid_line();
            continue;
        };
        summary.observe(
            event["stream"].as_str().unwrap_or_default(),
            event["time"].as_str(),
            event["type"].as_str().unwrap_or_default(),
            &event["payload"],
            event["causes"].as_array().is_some_and(|c| c.is_empty()),
        );
    }
    summary.finish(path)
}

/// Summarize an already-open ledger in bounded batches without copying bodies.
pub fn summarize_reader(
    path: &Path,
    reader: &crate::kernel::log::LogReader,
) -> std::io::Result<Option<Value>> {
    let through = reader.snapshot_end();
    let checkpoint = reader.load_checkpoint::<summary::Summary>("ledger-summary", 1, through)?;
    let mut summary = checkpoint.state.unwrap_or_default();
    reader.visit_range(checkpoint.through + 1, through, |events| {
        for event in events {
            summary.observe_event(event)?;
        }
        Ok(())
    })?;
    if let Err(error) = summary.save_checkpoint(reader) {
        eprintln!("cannot save ledger summary: {error}");
    }
    Ok(summary.finish(path))
}

/// Write the index from scratch by reading every ledger back.
///
/// The whole index is derived, so this is always allowed to run and always
/// produces the truth. It is also what makes appending to the index safe: a
/// duplicated or stale line is a cosmetic problem with a one-command fix.
pub fn rebuild(home: &Path) -> std::io::Result<usize> {
    let at = dir(home);
    std::fs::create_dir_all(&at)?;
    let mut written = 0;
    let mut out = String::new();
    for ledger in all(home).into_iter().rev() {
        if let Some(entry) = summarize(&ledger) {
            out.push_str(&entry.to_string());
            out.push('\n');
            written += 1;
        }
    }
    std::fs::write(at.join("index.jsonl"), out)?;
    Ok(written)
}

/// Note one conversation in the index, without reading the others.
///
/// Appends. A stream noted twice has two lines and the later one is the truth,
/// which is the cheapest thing that can be done at the end of a session and
/// costs only tidiness — [`rebuild`] settles it.
pub fn note(home: &Path, path: &Path) -> std::io::Result<()> {
    let Some(entry) = summarize(path) else {
        return Ok(());
    };
    note_entry(home, &entry)
}

/// Append already-computed metadata without reopening the conversation.
pub fn note_entry(home: &Path, entry: &Value) -> std::io::Result<()> {
    let at = dir(home);
    std::fs::create_dir_all(&at)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(at.join("index.jsonl"))?;
    use std::io::Write;
    writeln!(file, "{entry}")
}

// ── Keeping the directory in order ──────────────────────────────────────────

/// How long a conversation stays as it is, in days.
///
/// Read from `~/.lattice/preferences.json` under `ledger`. Absent means the
/// whole sweep is off — which is the default, because rewriting or moving
/// somebody's records is not something to start doing without being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Policy {
    pub compact_after_days: Option<u64>,
    pub archive_after_days: Option<u64>,
    pub delete_after_days: Option<u64>,
}

impl Policy {
    pub fn from_preferences(doc: &Value) -> Option<Self> {
        let section = doc.get("ledger")?.as_object()?;
        let days = |key: &str| section.get(key).and_then(Value::as_u64);
        Some(Policy {
            compact_after_days: days("compactAfterDays"),
            archive_after_days: days("archiveAfterDays"),
            delete_after_days: days("deleteAfterDays"),
        })
    }

    pub fn is_off(&self) -> bool {
        self.compact_after_days.is_none()
            && self.archive_after_days.is_none()
            && self.delete_after_days.is_none()
    }
}

/// What a sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub compacted: usize,
    pub archived: usize,
    pub deleted: usize,
    pub bytes_freed: u64,
}

/// Old conversations, out of the way of the current ones.
///
/// Archived rather than compressed. Compression would save more and cost the
/// one thing this whole design is for: a compressed ledger is one the agent
/// cannot read at all. Moving it keeps every line searchable and only takes it
/// out of "continue the last conversation" and off the index's main list.
pub fn archive_dir(home: &Path) -> PathBuf {
    dir(home).join("archive")
}

/// Apply the policy to every conversation. Returns what it did.
///
/// Age is the file's own last-modified time, so a conversation resumed
/// yesterday counts as active however long ago it started.
///
/// `now` is passed in rather than read, so a test can state the date instead
/// of waiting for one.
pub fn sweep(
    home: &Path,
    policy: Policy,
    now: std::time::SystemTime,
    types: &[crate::contracts::event::EventTypeDecl],
) -> std::io::Result<Swept> {
    let mut done = Swept::default();
    if policy.is_off() {
        return Ok(done);
    }
    let day = 60 * 60 * 24;
    for ledger in all(home) {
        let Ok(meta) = std::fs::metadata(&ledger) else {
            continue;
        };
        // Segmented ledgers are not inputs to the legacy destructive sweep.
        // In particular, deleting their documents before remove_file fails on
        // the directory would destroy half a conversation without removing it.
        if meta.is_dir() {
            continue;
        }
        let age_days = meta
            .modified()
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .map(|d| d.as_secs() / day)
            .unwrap_or(0);

        // Strongest first: something old enough to delete is not worth
        // compacting on the way out.
        if policy.delete_after_days.is_some_and(|n| age_days >= n) {
            // Noted before it goes. That a conversation existed is not the
            // same fact as what was in it, and deleting the second should not
            // quietly delete the first.
            if let Some(entry) = summarize(&ledger) {
                let mut entry = entry;
                entry["deleted"] = Value::Bool(true);
                let _ = append_line(&dir(home).join("deleted.jsonl"), &entry);
            }
            done.bytes_freed += meta.len();
            let documents = crate::contracts::document::documents_dir(&ledger);
            let _ = std::fs::remove_dir_all(&documents);
            std::fs::remove_file(&ledger)?;
            done.deleted += 1;
            continue;
        }
        if policy.archive_after_days.is_some_and(|n| age_days >= n) {
            let to = archive_dir(home);
            std::fs::create_dir_all(&to)?;
            let Some(name) = ledger.file_name() else {
                continue;
            };
            // The documents travel with it — they are half of the record.
            let documents = crate::contracts::document::documents_dir(&ledger);
            if documents.is_dir() {
                if let Some(stem) = ledger.file_stem() {
                    let _ = std::fs::rename(&documents, to.join(stem));
                }
            }
            std::fs::rename(&ledger, to.join(name))?;
            done.archived += 1;
            continue;
        }
        if policy.compact_after_days.is_some_and(|n| age_days >= n) {
            let before = meta.len();
            if let Ok(report) = crate::compact(&ledger, types) {
                if report.rewritten > 0 {
                    done.compacted += 1;
                    done.bytes_freed += before.saturating_sub(report.bytes_after);
                }
            }
        }
    }
    Ok(done)
}

fn append_line(path: &Path, entry: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{entry}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn conversation(stream: &str, said: &str) -> Vec<Value> {
        vec![
            json!({"v":1,"id":"ev_1_a","seq":1,"stream":stream,"time":"2026-07-29T09:00:00.000Z",
                   "type":"core.stream.opened","source":"core","causes":[],
                   "payload":{"lattice":"0.1.0","host":"tui","model":"m","instances":{}}}),
            json!({"v":1,"id":"ev_2_b","seq":2,"stream":stream,"time":"2026-07-29T09:00:01.000Z",
                   "type":"core.input.user_message","source":"ui","causes":[],
                   "payload":{"text":said}}),
            json!({"v":1,"id":"ev_3_c","seq":3,"stream":stream,"time":"2026-07-29T09:00:02.000Z",
                   "type":"core.input.user_message","source":"skills","causes":["ev_2_b"],
                   "payload":{"text":"forwarded, not what they typed"}}),
            json!({"v":1,"id":"ev_4_d","seq":4,"stream":stream,"time":"2026-07-29T09:00:03.000Z",
                   "type":"core.control.turn_completed","source":"loop","causes":["ev_3_c"],
                   "payload":{}}),
        ]
    }

    #[test]
    fn segmented_history_discovery_usage_and_summary_are_read_only_and_logical() {
        let home = tempfile::tempdir().unwrap();
        let at = dir(home.path());
        std::fs::create_dir_all(&at).unwrap();
        let path = at.join("20990101-tui.ledger");
        let mut log = crate::EventLog::open_segmented(
            crate::core_events::core_event_decls(),
            "logical",
            path.clone(),
            1,
        )
        .unwrap();
        log.append(crate::EventDraft::new("core.stream.opened", &[], json!({"lattice":"test","started":"2026-01-01T00:00:00Z","host":"tui","model":"offline","instances":{},"cwd":"/fixture"})), "core").unwrap();
        log.append(
            crate::EventDraft::new(
                crate::core_events::USER_MESSAGE,
                &[],
                json!({"text":"one conversation"}),
            ),
            "ui",
        )
        .unwrap();
        let completed = log
            .append(
                crate::EventDraft::new(
                    crate::core_events::MODEL_CALL_COMPLETED,
                    &[],
                    json!({"status":"ok","usage":{"input_tokens":12,"output_tokens":3}}),
                ),
                "model",
            )
            .unwrap();
        let sources = crate::EventLog::source_paths(&path).unwrap();
        assert_eq!(sources.len(), 3);
        let originals: Vec<_> = sources
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
        assert_eq!(all(home.path()), vec![path.clone()]);
        assert_eq!(
            latest_here(home.path(), Path::new("/fixture")),
            Some(path.clone())
        );
        assert!(here(home.path(), Path::new("/another")).is_empty());
        let expected = summarize(&path).unwrap();
        assert_eq!(expected["events"], 3);
        assert_eq!(expected["title"], "one conversation");
        assert_eq!(
            expected["bytes"],
            originals.iter().map(Vec::len).sum::<usize>()
        );
        assert_eq!(
            usage_by_day(home.path())[&completed.time[..10]],
            Day {
                calls: 1,
                prompt: 12,
                output: 3,
                conversations: 1,
                ..Day::default()
            }
        );
        assert_eq!(
            summarize_reader(&path, &log.reader()).unwrap(),
            Some(expected.clone())
        );
        let before = log.reader().memory_stats().unwrap().cache.unwrap();
        assert_eq!(
            summarize_reader(&path, &log.reader()).unwrap(),
            Some(expected)
        );
        let after = log.reader().memory_stats().unwrap().cache.unwrap();
        assert_eq!(
            after.hits + after.decodes,
            before.hits + before.decodes,
            "warm exit summary reads no old event bodies"
        );
        let attachment = crate::document::documents_dir(&path).join("keep.txt");
        std::fs::create_dir_all(attachment.parent().unwrap()).unwrap();
        std::fs::write(&attachment, "keep the original attachment").unwrap();
        let swept = sweep(
            home.path(),
            Policy {
                delete_after_days: Some(0),
                ..Policy::default()
            },
            std::time::UNIX_EPOCH,
            &crate::core_events::core_event_decls(),
        )
        .unwrap();
        assert_eq!(swept, Swept::default());
        assert_eq!(
            std::fs::read_to_string(attachment).unwrap(),
            "keep the original attachment"
        );
        for (path, original) in sources.iter().zip(originals) {
            assert_eq!(std::fs::read(path).unwrap(), original);
        }
    }

    #[test]
    fn a_name_sorts_by_time_and_says_which_program_wrote_it() {
        assert_eq!(
            name_for("20260729-074225", "tui", None),
            "20260729-074225-tui.ledger"
        );
        assert_eq!(
            name_for("20260729-091502", "daemon", Some("main")),
            "20260729-091502-daemon-main.ledger"
        );
        let mut names = [
            name_for("20260729-091502", "daemon", Some("main")),
            name_for("20260728-074225", "tui", None),
        ];
        names.sort();
        assert!(
            names[0].starts_with("20260728"),
            "sorting by name is by time"
        );
    }

    /// Older layouts are still read. A record already written is history, and
    /// moving it to satisfy a naming scheme would be churn for its own sake.
    #[test]
    fn conversations_are_found_across_the_old_directories_too() {
        let home = tempfile::tempdir().unwrap();
        ledger(
            &home.path().join(".lattice/ledgers"),
            "20260729-100000-tui.jsonl",
            &conversation("new", "the newest one"),
        );
        ledger(
            &home.path().join(".lattice/tui"),
            "20260701-100000.jsonl",
            &conversation("old", "an old one"),
        );
        let found = all(home.path());
        assert_eq!(found.len(), 2, "both layouts: {found:?}");
        assert!(
            found[0].ends_with("20260729-100000-tui.jsonl"),
            "newest first: {found:?}"
        );
    }

    #[test]
    fn the_index_says_enough_to_recognise_a_conversation() {
        let home = tempfile::tempdir().unwrap();
        ledger(
            &home.path().join(".lattice/ledgers"),
            "20260729-100000-tui.jsonl",
            &conversation("st_1", "看下你自己的流水"),
        );
        assert_eq!(rebuild(home.path()).unwrap(), 1);

        let text =
            std::fs::read_to_string(home.path().join(".lattice/ledgers/index.jsonl")).unwrap();
        let entry: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(entry["stream"], "st_1");
        assert_eq!(entry["file"], "20260729-100000-tui.jsonl");
        assert_eq!(entry["events"], 4);
        assert_eq!(entry["turns"], 1);
        assert_eq!(entry["host"], "tui");
        assert_eq!(entry["model"], "m");
        assert_eq!(entry["opened"], "2026-07-29T09:00:00.000Z");
        assert_eq!(
            entry["title"], "看下你自己的流水",
            "what the person said, which is how anyone remembers a conversation"
        );
    }

    /// The forwarded copy of a user's message is a station's re-emission, not
    /// what they typed. A title taken from it would say the wrong thing.
    #[test]
    fn the_title_is_what_the_person_typed_not_a_forward_of_it() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260729-100000-tui.jsonl",
            &conversation("st_1", "the original"),
        );
        assert_eq!(summarize(&path).unwrap()["title"], "the original");
    }

    /// Appending is allowed to leave a stale line; rebuilding settles it.
    #[test]
    fn rebuilding_replaces_whatever_was_there() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260729-100000-tui.jsonl",
            &conversation("st_1", "first"),
        );
        note(home.path(), &path).unwrap();
        note(home.path(), &path).unwrap();
        let index = home.path().join(".lattice/ledgers/index.jsonl");
        assert_eq!(
            std::fs::read_to_string(&index).unwrap().lines().count(),
            2,
            "appending twice leaves two lines"
        );
        rebuild(home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&index).unwrap().lines().count(),
            1,
            "and rebuilding settles it"
        );
    }

    fn aged(path: &Path, days: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400 + 60);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    /// Absent configuration means nothing happens. Rewriting or moving
    /// somebody's records is not something to start doing unasked.
    #[test]
    fn the_sweep_is_off_until_it_is_asked_for() {
        assert_eq!(Policy::from_preferences(&json!({})), None);
        assert_eq!(Policy::from_preferences(&json!({"thinking": "high"})), None);
        let set = Policy::from_preferences(&json!({"ledger": {"compactAfterDays": 7}})).unwrap();
        assert_eq!(set.compact_after_days, Some(7));
        assert!(!set.is_off());
        assert!(Policy::from_preferences(&json!({"ledger": {}}))
            .unwrap()
            .is_off());
    }

    #[test]
    fn an_old_conversation_is_moved_aside_with_its_documents() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260601-100000-tui.jsonl",
            &conversation("st_old", "months ago"),
        );
        let documents = path.with_extension("");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(documents.join("ev_2-system.txt"), "the prompt").unwrap();
        aged(&path, 40);

        let policy = Policy {
            archive_after_days: Some(30),
            ..Policy::default()
        };
        let done = sweep(
            home.path(),
            policy,
            std::time::SystemTime::now(),
            &crate::core_events::core_event_decls(),
        )
        .unwrap();
        assert_eq!(done.archived, 1);
        assert!(!path.exists(), "moved out of the way");
        let to = archive_dir(home.path());
        assert!(to.join("20260601-100000-tui.jsonl").is_file());
        assert_eq!(
            std::fs::read_to_string(to.join("20260601-100000-tui").join("ev_2-system.txt"))
                .unwrap(),
            "the prompt",
            "the documents travel with it — they are half of the record"
        );
        // And it is no longer offered as a conversation to continue
        assert!(all(home.path()).is_empty(), "{:?}", all(home.path()));
    }

    /// That a conversation existed is not the same fact as what was in it.
    /// Deleting the second must not quietly delete the first.
    #[test]
    fn a_deleted_conversation_leaves_a_note_that_it_existed() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260101-100000-tui.jsonl",
            &conversation("st_gone", "long ago"),
        );
        aged(&path, 400);

        let done = sweep(
            home.path(),
            Policy {
                delete_after_days: Some(365),
                ..Policy::default()
            },
            std::time::SystemTime::now(),
            &crate::core_events::core_event_decls(),
        )
        .unwrap();
        assert_eq!(done.deleted, 1);
        assert!(!path.exists());

        let noted = std::fs::read_to_string(dir(home.path()).join("deleted.jsonl")).unwrap();
        let entry: Value = serde_json::from_str(noted.lines().next().unwrap()).unwrap();
        assert_eq!(entry["stream"], "st_gone");
        assert_eq!(entry["title"], "long ago");
        assert_eq!(entry["deleted"], true);
    }

    /// A conversation still in use is left alone, however long ago it started.
    /// Age is when it was last touched — resuming one makes it current again.
    #[test]
    fn a_conversation_still_in_use_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260101-100000-tui.jsonl",
            &conversation("st_live", "started long ago, still going"),
        );
        let done = sweep(
            home.path(),
            Policy {
                compact_after_days: Some(7),
                archive_after_days: Some(30),
                delete_after_days: Some(60),
            },
            std::time::SystemTime::now(),
            &crate::core_events::core_event_decls(),
        )
        .unwrap();
        assert_eq!(done, Swept::default(), "nothing was done to it");
        assert!(path.exists());
    }

    /// "Continue the last conversation" means the last one HERE.
    ///
    /// Every project's conversations share one directory. Without this, `-c`
    /// in one project picked up the newest conversation on the whole machine,
    /// which could be another project's entirely.
    #[test]
    fn continuing_only_offers_the_conversations_of_this_project() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".lattice/ledgers");
        let mine = |cwd: &str, said: &str| {
            let mut lines = conversation("st", said);
            lines[0]["payload"]["cwd"] = json!(cwd);
            lines
        };
        ledger(
            &dir,
            "20260701-100000-tui.jsonl",
            &mine("/work/alpha", "alpha"),
        );
        ledger(
            &dir,
            "20260702-100000-tui.jsonl",
            &mine("/work/beta", "beta"),
        );
        // Written before the field existed: offered to everyone rather than to
        // nobody, or a person's history becomes unreachable by name.
        ledger(
            &dir,
            "20260703-100000-tui.jsonl",
            &conversation("st", "older"),
        );

        let alpha = here(home.path(), Path::new("/work/alpha"));
        let titles: Vec<String> = alpha
            .iter()
            .filter_map(|p| summarize(p))
            .filter_map(|s| s["title"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            titles,
            vec!["older".to_string(), "alpha".to_string()],
            "this project's, plus the one that never said: {titles:?}"
        );
        assert_eq!(here(home.path(), Path::new("/work/beta")).len(), 2);
        assert_eq!(
            here(home.path(), Path::new("/work/gamma")).len(),
            1,
            "a project with no history of its own sees only the unmarked one"
        );
        // The index still lists everything: finding an old conversation is a
        // different question from continuing this project's last one.
        assert_eq!(all(home.path()).len(), 3);
        for project in ["/work/alpha", "/work/beta", "/work/gamma"] {
            let expected = here(home.path(), Path::new(project)).into_iter().next();
            assert_eq!(latest_here(home.path(), Path::new(project)), expected);
        }
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(latest_here(empty.path(), Path::new("/work/alpha")), None);
    }

    #[test]
    fn location_lookup_matches_summary_across_tail_and_chunk_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let long = format!("/work/{}", "directory".repeat(1800));
        for cwd in ["/work/alpha", long.as_str()] {
            for ending in ["", "\n", "\r\n", "\n{broken tail"] {
                let lines = [
                    json!({"type":"core.stream.opened", "payload":{"cwd":"/old"}}),
                    json!({"type":"core.stream.resumed", "payload":{"cwd":cwd}}),
                    json!({"type":"core.input.user_message", "payload":{"text":"z".repeat(20000),"cwd":"/not-a-launch"}}),
                    json!({"type":"core.stream.resumed", "payload":{"cwd":""}}),
                    json!({"type":"core.stream.resumed", "payload":{"cwd":17}}),
                ];
                let text = lines
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    + ending;
                std::fs::write(&path, &text).unwrap();
                let actual = recorded_cwd(&mut std::io::Cursor::new(text.as_bytes())).unwrap();
                assert_eq!(actual.as_deref(), summarize(&path).unwrap()["cwd"].as_str());
                assert_eq!(actual.as_deref(), Some(cwd));
            }
        }
        // UTF-8 is reconstructed as bytes before JSON parsing, even when a
        // multibyte character straddles the fixed-size read blocks.
        let cwd = format!("/work/{}", "\u{754c}".repeat(5000));
        let text = json!({"type":"core.stream.opened", "payload":{"cwd":cwd}}).to_string();
        assert_eq!(
            recorded_cwd(&mut std::io::Cursor::new(text)).unwrap(),
            Some(cwd)
        );
        assert_eq!(
            recorded_cwd(&mut std::io::Cursor::new(b" \n{}\n")).unwrap(),
            None
        );
    }

    #[test]
    fn a_recent_location_does_not_read_the_old_payloads() {
        use std::io::{Cursor, Read, Seek, SeekFrom};
        struct Counted {
            inner: Cursor<Vec<u8>>,
            bytes: usize,
        }
        impl Read for Counted {
            fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
                let n = self.inner.read(into)?;
                self.bytes += n;
                Ok(n)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(from)
            }
        }
        let text = format!(
            "{}\n{}\n",
            "x".repeat(1024 * 1024),
            json!({"type":"core.stream.resumed", "payload":{"cwd":"/work"}})
        );
        let mut input = Counted {
            inner: Cursor::new(text.into_bytes()),
            bytes: 0,
        };
        assert_eq!(recorded_cwd(&mut input).unwrap().as_deref(), Some("/work"));
        assert_eq!(
            input.bytes, 8192,
            "one tail block, independent of transcript size"
        );
    }

    #[test]
    #[ignore = "set LATTICE_PROFILE_LEDGER to an immutable snapshot; read-only lookup profile"]
    fn profile_ledger_selection() {
        let input = std::env::var("LATTICE_PROFILE_LEDGER").expect("snapshot required");
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir(home.path())).unwrap();
        let path = dir(home.path()).join("20260901-000000-tui.jsonl");
        std::fs::copy(input, &path).unwrap();
        let summary = summarize(&path).unwrap();
        let cwd = Path::new(summary["cwd"].as_str().unwrap_or("/unknown"));
        let began = std::time::Instant::now();
        let old: Vec<_> = all(home.path())
            .into_iter()
            .filter(|p| {
                summarize(p)
                    .and_then(|s| s["cwd"].as_str().map(str::to_string))
                    .is_none_or(|theirs| Path::new(&theirs) == cwd)
            })
            .collect();
        eprintln!("full-summary selection: {:?}", began.elapsed());
        let began = std::time::Instant::now();
        let new = latest_here(home.path(), cwd);
        eprintln!("tail-only latest selection: {:?}", began.elapsed());
        assert_eq!(new.as_ref(), old.first());
    }

    /// The index is not a ledger and must never be listed as one.
    #[test]
    fn the_index_is_not_mistaken_for_a_conversation() {
        let home = tempfile::tempdir().unwrap();
        let path = ledger(
            &home.path().join(".lattice/ledgers"),
            "20260729-100000-tui.jsonl",
            &conversation("st_1", "hello"),
        );
        note(home.path(), &path).unwrap();
        assert_eq!(all(home.path()).len(), 1, "{:?}", all(home.path()));
    }
}
