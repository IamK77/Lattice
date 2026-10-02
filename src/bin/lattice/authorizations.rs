//! Ledger questions and local answers have different lifetimes. Answering
//! hides one question immediately; only a recorded outcome settles its fact.

use lattice::{
    components::{browser_tools, operation_policy, trust_policy},
    core_events as ce,
    view::{AuthorizationChoice as Choice, AuthorizationPrompt},
    EventEnvelope,
};

#[cfg(test)]
#[path = "authorizations/tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Authorizations {
    questions: Vec<Question>,
    reader: Option<lattice::LogReader>,
    scoped: bool,
}

struct Details {
    description: String,
    flow: bool,
    permanent: bool,
}

impl Details {
    fn from_event(event: &EventEnvelope) -> Self {
        Self {
            description: lattice::view::authorization_description(&event.payload),
            flow: event.event_type != operation_policy::AUTH_REQUESTED
                || event.payload["grants"]
                    .as_array()
                    .is_some_and(|g| !g.is_empty()),
            permanent: event.event_type == trust_policy::AUTH_REQUESTED,
        }
    }
}

struct Question {
    request: String,
    held: String,
    // Kept with its occurrence, not in a set keyed by ID: folding has never
    // deduplicated requests. Local presentation is excluded from snapshots.
    answered_locally: bool,
    selected: Choice,
    detail_line: usize,
    details: std::cell::OnceCell<Details>,
}

impl Authorizations {
    pub fn bind_reader(&mut self, reader: lattice::LogReader) {
        self.reader = Some(reader);
    }

    pub fn set_scoped(&mut self, supported: bool) {
        self.scoped = supported;
    }

    pub fn prompt(&self) -> std::io::Result<Option<AuthorizationPrompt>> {
        let Some(question) = self.questions.iter().find(|q| !q.answered_locally) else {
            return Ok(None);
        };
        if question.details.get().is_none() {
            let details = if let Some(reader) = &self.reader {
                let event = reader.get(&question.request)?.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "authorization request is missing",
                    )
                })?;
                Details::from_event(&event)
            } else {
                // Headless state fixtures can restore IDs without a backing ledger.
                Details {
                    description: format!("Request {}", question.request),
                    flow: false,
                    permanent: false,
                }
            };
            let _ = question.details.set(details);
        }
        let details = question.details.get().expect("resolved question");
        let mut choices = Vec::new();
        // Legacy admission answers persist trust. Never label that as once.
        if self.scoped || !details.permanent {
            choices.push(Choice::Once);
        }
        if self.scoped && details.flow {
            choices.push(Choice::Flow);
        }
        if details.permanent {
            choices.push(Choice::Permanent);
        }
        choices.push(Choice::Refuse);
        let selected = if choices.contains(&question.selected) {
            question.selected
        } else {
            Choice::Refuse
        };
        Ok(Some(AuthorizationPrompt {
            request: question.request.clone(),
            description: details.description.clone(),
            choices,
            selected,
            detail_line: question.detail_line,
        }))
    }

    pub fn select_refuse(&mut self) {
        if let Some(question) = self.questions.iter_mut().find(|q| !q.answered_locally) {
            question.selected = Choice::Refuse;
        }
    }

    pub fn move_selection(&mut self, delta: isize) -> std::io::Result<()> {
        if let Some(prompt) = self.prompt()? {
            let at = prompt
                .choices
                .iter()
                .position(|c| *c == prompt.selected)
                .unwrap_or(0);
            let next = at
                .saturating_add_signed(delta)
                .min(prompt.choices.len() - 1);
            if let Some(question) = self.questions.iter_mut().find(|q| !q.answered_locally) {
                question.selected = prompt.choices[next];
            }
        }
        Ok(())
    }

    pub fn scroll_to(&mut self, line: usize) {
        if let Some(question) = self.questions.iter_mut().find(|q| !q.answered_locally) {
            question.detail_line = line;
        }
    }

    #[cfg(test)]
    pub fn answer_selected(&mut self) -> Option<(String, bool)> {
        let prompt = self.prompt().ok()??;
        self.answer_oldest();
        Some((prompt.request, prompt.selected != Choice::Refuse))
    }

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
                selected: Choice::Refuse,
                detail_line: 0,
                details: std::cell::OnceCell::new(),
            })
            .collect();
    }
    pub fn observe(&mut self, event: &EventEnvelope) {
        if event.event_type == operation_policy::AUTH_REQUESTED
            || event.event_type == trust_policy::AUTH_REQUESTED
            || event.event_type == browser_tools::AUTH_REQUESTED
            || event.event_type == lattice::components::expert_definitions::AUTH_REQUESTED
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
                selected: Choice::Refuse,
                detail_line: 0,
                details: std::cell::OnceCell::from(Details::from_event(event)),
            });
        } else if event.event_type == operation_policy::DECISION
            || event.event_type == trust_policy::DECISION
            || event.event_type == browser_tools::DECISION
            || event.event_type == lattice::components::expert_definitions::DECISION
            || ce::is_outcome(&event.event_type)
        {
            self.questions.retain(|question| {
                !event.causes.iter().any(|cause| cause == &question.held)
                    && event.payload["held"].as_str() != Some(question.held.as_str())
            });
        }
    }
}
