//! Ledger questions and local answers have different lifetimes. Answering
//! hides one question immediately; only a recorded outcome settles its fact.

use lattice::{
    components::{browser_tools, trust_policy},
    core_events as ce, EventEnvelope,
};

#[cfg(test)]
#[path = "authorizations/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Authorizations {
    questions: Vec<Question>,
}

struct Question {
    request: String,
    held: String,
    // Kept with its occurrence, not in a set keyed by ID: folding has never
    // deduplicated requests. This flag is excluded from history snapshots.
    answered_locally: bool,
}

impl Authorizations {
    pub fn next(&self) -> Option<&str> {
        self.questions
            .iter()
            .find(|q| !q.answered_locally)
            .map(|q| q.request.as_str())
    }
    pub fn answerable_count(&self) -> usize {
        self.questions
            .iter()
            .filter(|q| !q.answered_locally)
            .count()
    }
    pub fn answer_oldest(&mut self) -> Option<String> {
        let question = self.questions.iter_mut().find(|q| !q.answered_locally)?;
        question.answered_locally = true;
        Some(question.request.clone())
    }
    pub fn history(&self) -> Vec<(String, String)> {
        self.questions
            .iter()
            .map(|q| (q.request.clone(), q.held.clone()))
            .collect()
    }
    /// Installing historical state resets the local answers, as replacing the
    /// old live queue did. Reopening an unresolved question is not a resend.
    pub fn restore(&mut self, questions: Vec<(String, String)>) {
        self.questions = questions
            .into_iter()
            .map(|(request, held)| Question {
                request,
                held,
                answered_locally: false,
            })
            .collect();
    }
    pub fn observe(&mut self, event: &EventEnvelope) {
        if event.event_type == trust_policy::AUTH_REQUESTED
            || event.event_type == browser_tools::AUTH_REQUESTED
        {
            let held = event.payload["held"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| event.causes.first().cloned())
                .unwrap_or_default();
            self.questions.push(Question {
                request: event.id.clone(),
                held,
                answered_locally: false,
            });
        } else if event.event_type == trust_policy::DECISION
            || event.event_type == browser_tools::DECISION
            || ce::is_outcome(&event.event_type)
        {
            self.questions.retain(|question| {
                !event.causes.iter().any(|cause| cause == &question.held)
                    && event.payload["held"].as_str() != Some(question.held.as_str())
            });
        }
    }
}
