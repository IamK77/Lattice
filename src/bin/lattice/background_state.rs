//! A stream's background history and its local display readings. The host's
//! cross-tab projection is deliberately outside this owner.
use lattice::{core_events as ce, view::Live, EventEnvelope};
use serde_json::Value;

#[cfg(test)]
#[path = "background_state/tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct HistoricalRow {
    pub kind: &'static str,
    pub key: String,
    pub label: String,
    pub standing: bool,
    pub fires: usize,
    pub ledger: Option<String>,
}

impl HistoricalRow {
    pub fn display(&self, since: usize) -> Live {
        Live {
            kind: self.kind,
            key: self.key.clone(),
            label: self.label.clone(),
            standing: self.standing,
            fires: self.fires,
            ledger: self.ledger.clone(),
            since,
            tools: 0,
            tokens: 0,
            read_len: 0,
        }
    }
    #[cfg(test)]
    fn from_display(row: &Live) -> Self {
        Self {
            kind: row.kind,
            key: row.key.clone(),
            label: row.label.clone(),
            standing: row.standing,
            fires: row.fires,
            ledger: row.ledger.clone(),
        }
    }
}

pub(super) enum Mutation {
    Start(HistoricalRow),
    Cancel(String),
    Wake(String),
}

#[derive(Default)]
pub(super) struct History {
    rows: Vec<HistoricalRow>,
}

/// Removed positions refer to the old arrays, in increasing order. A new row,
/// when present, is always the final history row.
pub(super) struct Change {
    removed_indices: Vec<usize>,
    appended: bool,
}

impl History {
    pub fn new(rows: Vec<HistoricalRow>) -> Self {
        Self { rows }
    }
    pub fn rows(&self) -> &[HistoricalRow] {
        &self.rows
    }
    fn remove(&mut self, key: &str) -> Vec<usize> {
        let removed = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(at, row)| (row.key == key).then_some(at))
            .collect();
        self.rows.retain(|row| row.key != key);
        removed
    }
    pub fn apply(&mut self, mutation: Mutation) -> Change {
        match mutation {
            Mutation::Start(row) => {
                let removed_indices = self.remove(&row.key);
                self.rows.push(row);
                Change {
                    removed_indices,
                    appended: true,
                }
            }
            Mutation::Cancel(key) => Change {
                removed_indices: self.remove(&key),
                appended: false,
            },
            Mutation::Wake(key) => {
                let mut ended = false;
                for row in &mut self.rows {
                    if row.key == key {
                        row.fires += 1;
                        ended |= !row.standing;
                    }
                }
                // The old reducer removed by key, not by standing flag. Thus
                // one completed duplicate ends every row with that key.
                Change {
                    removed_indices: if ended { self.remove(&key) } else { Vec::new() },
                    appended: false,
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct Background {
    history: History,
    // A positional display cache also owns local progress. Positions, not keys,
    // preserve separate readings even in legacy snapshots with duplicate keys.
    rows: Vec<Live>,
}

impl Background {
    pub fn rows(&self) -> &[Live] {
        &self.rows
    }
    pub fn history(&self) -> &History {
        &self.history
    }
    pub fn restore(&mut self, history: History, settled_tick: usize) {
        self.rows = history
            .rows
            .iter()
            .map(|row| row.display(settled_tick))
            .collect();
        self.history = history;
    }
    pub fn apply(&mut self, mutation: Mutation, tick: usize) {
        let change = self.history.apply(mutation);
        for at in change.removed_indices.into_iter().rev() {
            self.rows.remove(at);
        }
        if change.appended {
            self.rows.push(
                self.history
                    .rows
                    .last()
                    .expect("appended history row")
                    .display(tick),
            );
        }
        debug_assert_eq!(self.history.rows.len(), self.rows.len());
        for (row, history) in self.rows.iter_mut().zip(&self.history.rows) {
            row.fires = history.fires;
        }
    }
    /// Commit a row's counters and logical byte cursor only after a successful
    /// whole read. Model interpretation is supplied by the current parent view.
    pub fn refresh(&mut self, model: &str, dialect: &str) {
        for row in &mut self.rows {
            let Some(path) = row.ledger.as_ref() else {
                continue;
            };
            let mut tools = row.tools;
            let mut tokens = row.tokens;
            let result = lattice::ledgers::visit_appended(
                std::path::Path::new(path),
                row.read_len,
                |event| {
                    match event.event_type.as_str() {
                        ce::TOOL_EXEC_STARTED => tools = tools.saturating_add(1),
                        ce::MODEL_CALL_COMPLETED => {
                            if let Some(usage) =
                                super::accounting::read_usage(&event.payload, model, dialect)
                            {
                                tokens = tokens
                                    .saturating_add(usage.prompt)
                                    .saturating_add(usage.output);
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                },
            );
            if let Ok(offset) = result {
                row.read_len = offset;
                row.tools = tools;
                row.tokens = tokens;
            }
        }
    }
    #[cfg(test)]
    pub fn fixture_push(&mut self, row: Live) {
        self.history.rows.push(HistoricalRow::from_display(&row));
        self.rows.push(row);
    }
    #[cfg(test)]
    pub fn fixture_edit(&mut self, at: usize, edit: impl FnOnce(&mut Live)) {
        edit(&mut self.rows[at]);
        self.history.rows[at] = HistoricalRow::from_display(&self.rows[at]);
    }
}

/// Request pairing belongs to EventInputs. Missing requests do not authorize
/// guesses about either starts or cancellations. Path resolution is supplied
/// by the coordinator after a real expert start has been recognized.
pub(super) fn interpret(
    event: &EventEnvelope,
    request: Option<(&str, &Value)>,
) -> Option<Mutation> {
    let payload = &event.payload;
    match event.event_type.as_str() {
        ce::TOOL_EXEC_COMPLETED => {
            let (tool, args) = request?;
            if let Some(key) = cancelled_key(tool, args) {
                return Some(Mutation::Cancel(key));
            }
            if payload["status"] != "ok" {
                return None;
            }
            started(tool, args, &payload["result"]).map(Mutation::Start)
        }
        ce::WAKE => Some(Mutation::Wake(match payload["body"]["watch"].as_u64() {
            Some(id) => format!("watch:{id}"),
            None => payload["source"].as_str().unwrap_or_default().to_string(),
        })),
        _ => None,
    }
}

pub(super) fn started(tool: &str, args: &Value, result: &Value) -> Option<HistoricalRow> {
    let head = |value: &Value| {
        value
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (kind, key, label, standing) = if let Some(id) = result["timer"].as_u64() {
        let every = args["interval_ms"].as_u64();
        (
            "timer",
            format!("timer:{id}"),
            match every {
                Some(ms) => format!("every {}s", ms / 1000),
                None => format!("once in {}s", args["delay_ms"].as_u64().unwrap_or(0) / 1000),
            },
            every.is_some(),
        )
    } else if let Some(id) = result["watch"].as_u64() {
        (
            "watch",
            format!("watch:{id}"),
            args["path"].as_str().unwrap_or_default().to_string(),
            true,
        )
    } else {
        let job = match result.get("job")? {
            Value::String(id) if !id.is_empty() => id.clone(),
            Value::Number(id) if id.is_u64() => id.to_string(),
            _ => return None,
        };
        if tool == "ask" {
            // Foreground delegation returns its final outcome, not a start
            // receipt. It will not emit a later wake to retire a background row.
            if args["background"] == false {
                return None;
            }
            (
                "expert",
                format!("expert:{job}"),
                format!(
                    "{} · {}",
                    args["expert"].as_str().unwrap_or("expert"),
                    head(&args["prompt"])
                ),
                false,
            )
        } else if tool == "Run" && result["background"] == true {
            (
                "command",
                format!("background:{job}"),
                head(&args["command"]),
                false,
            )
        } else {
            return None;
        }
    };
    Some(HistoricalRow {
        kind,
        key,
        label,
        standing,
        fires: 0,
        ledger: None,
    })
}

fn cancelled_key(tool: &str, args: &Value) -> Option<String> {
    if let Some(id) = args["timer"].as_u64() {
        if tool == "Unschedule" {
            return Some(format!("timer:{id}"));
        }
    }
    if let Some(id) = args["watch"].as_u64() {
        if tool == "Unwatch" {
            return Some(format!("watch:{id}"));
        }
    }
    None
}
