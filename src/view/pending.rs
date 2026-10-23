//! Display-only receipts for recorded input. No scheduling or ledger mutation.
use super::{
    history::record::{Identity, Kind},
    Entry, TranscriptGroup, TranscriptKind,
};
use crate::{core_events as ce, EventEnvelope};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io, ops::Range};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingInputs {
    inputs: Vec<Input>,
    activity: super::activity::Activity,
}

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    id: String,
    kind: String,
    aliases: BTreeSet<String>,
    label: String,
    cards: Range<usize>,
}

impl PendingInputs {
    pub fn with_activity(activity: super::activity::Activity) -> Self {
        Self {
            activity,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, event: &EventEnvelope) {
        let was_busy = self.activity.busy;
        self.activity.observe(event);
        match event.event_type.as_str() {
            ce::USER_MESSAGE if event.causes.is_empty() => {
                if !was_busy {
                    return;
                }
                let Some(text) = event.payload["text"].as_str() else {
                    return;
                };
                self.inputs.push(Input {
                    id: event.id.clone(),
                    kind: event.event_type.clone(),
                    aliases: BTreeSet::from([event.id.clone()]),
                    label: {
                        let mut label = text.to_owned();
                        for image in event.payload["images"].as_array().into_iter().flatten() {
                            let name = image["name"]
                                .as_str()
                                .or_else(|| image["file"].as_str())
                                .unwrap_or("image");
                            label.push_str(&format!(" [image: {name}]"));
                        }
                        label
                    },
                    cards: 0..0,
                });
            }
            ce::USER_MESSAGE => {
                for input in &mut self.inputs {
                    if input.kind == ce::USER_MESSAGE
                        && event.causes.iter().any(|id| input.aliases.contains(id))
                    {
                        input.aliases.insert(event.id.clone());
                    }
                }
            }
            ce::WAKE => self.inputs.push(Input {
                id: event.id.clone(),
                kind: event.event_type.clone(),
                aliases: BTreeSet::from([event.id.clone()]),
                label: format!("{} ↯", event.payload["source"].as_str().unwrap_or("wake")),
                cards: 0..0,
            }),
            ce::MODEL_CALL_STARTED if event.payload.get("purpose").is_none() => {
                let parts = event.payload["input"]["parts"]
                    .as_array()
                    .or_else(|| event.payload["input"].as_array());
                let included: BTreeSet<_> = parts
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("event").and_then(serde_json::Value::as_str))
                    .collect();
                self.inputs.retain(|input| {
                    !input
                        .aliases
                        .iter()
                        .any(|id| included.contains(id.as_str()))
                });
            }
            _ => {}
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.inputs
            .iter()
            .map(|input| input.label.clone())
            .collect()
    }

    pub fn label(&mut self, id: &str, label: String) {
        if let Some(input) = self.inputs.iter_mut().find(|input| input.id == id) {
            input.label = label;
        }
    }

    pub fn cards(&mut self, id: &str, cards: Range<usize>) {
        if let Some(input) = self
            .inputs
            .iter_mut()
            .find(|input| input.id == id && input.kind == ce::USER_MESSAGE)
        {
            input.cards = cards;
        }
    }

    pub fn valid(&self, total: usize) -> bool {
        let mut end = 0;
        let mut ids = BTreeSet::new();
        self.inputs.iter().all(|input| {
            let valid = ids.insert(&input.id)
                && input.aliases.contains(&input.id)
                && matches!(input.kind.as_str(), ce::USER_MESSAGE | ce::WAKE)
                && input.cards.start <= input.cards.end
                && input.cards.end <= total
                && (input.cards.is_empty()
                    || (input.kind == ce::USER_MESSAGE && input.cards.start >= end));
            if !input.cards.is_empty() {
                end = input.cards.end;
            }
            valid
        })
    }

    pub fn clear_cards(&mut self) {
        for input in &mut self.inputs {
            input.cards = 0..0;
        }
    }

    /// Map a stable source boundary into the currently visible sequence.
    pub fn before(&self, boundary: usize) -> usize {
        boundary
            - self
                .inputs
                .iter()
                .map(|input| {
                    boundary
                        .min(input.cards.end)
                        .saturating_sub(input.cards.start)
                })
                .sum::<usize>()
    }

    /// Inputs and their source ranges are recorded in ledger order.
    pub fn source(&self, mut visible: usize) -> usize {
        for input in &self.inputs {
            if input.cards.start <= visible {
                visible += input.cards.len();
            }
        }
        visible
    }

    pub(in crate::view) fn group(
        &self,
        index: usize,
        total: usize,
        kind: impl Fn(usize) -> io::Result<Kind>,
    ) -> io::Result<Range<usize>> {
        if index >= total {
            return Err(io::Error::other("visible card is outside history"));
        }
        let current = kind(self.source(index))?;
        let joins = |other| match current {
            Kind::User => other == Kind::User,
            Kind::Tool | Kind::Thinking => matches!(other, Kind::Tool | Kind::Thinking),
            _ => false,
        };
        let mut first = index;
        while first > 0 && joins(kind(self.source(first - 1))?) {
            first -= 1;
        }
        let mut end = index + 1;
        while end < total && joins(kind(self.source(end))?) {
            end += 1;
        }
        Ok(first..end)
    }

    pub fn transcript_group(&self, entries: &[Entry], index: usize) -> io::Result<TranscriptGroup> {
        let total = self.before(entries.len());
        let range = if index == total {
            total..total
        } else {
            self.group(index, total, |source| {
                Ok(Identity::of(&entries[source]).kind)
            })?
        };
        Ok(TranscriptGroup {
            first: range.start,
            total,
            previous: range
                .start
                .checked_sub(1)
                .map(|at| TranscriptKind::of(&entries[self.source(at)])),
            entries: range.map(|at| entries[self.source(at)].clone()).collect(),
        })
    }
}
