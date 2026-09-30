//! Frontend-owned drafts and request correlation. No definition files are written here.
#[cfg(test)]
#[path = "expert_controls/tests.rs"]
mod tests;
use lattice::{EventEnvelope, Session};
use ratatui::crossterm::event::KeyCode;
use serde_json::{json, Value};

const FIELDS: &[&str] = &[
    "Scope",
    "ID",
    "Name",
    "Description",
    "Model",
    "Capabilities",
    "Instructions",
];

struct Form {
    values: Vec<String>,
    at: usize,
    cursor: usize,
    original: Option<Value>,
    access_at: usize,
}
impl Form {
    fn new(definition: &Value, original: Option<Value>) -> Self {
        let scope = original
            .as_ref()
            .and_then(|v| v["target"]["scope"].as_str())
            .unwrap_or("project");
        let mut values = vec![scope.into()];
        for key in ["id", "name", "description", "model"] {
            values.push(definition[key].as_str().unwrap_or_default().into());
        }
        values.push(
            definition["capabilities"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "read".into()),
        );
        values.push(
            definition["instructions"]
                .as_str()
                .unwrap_or_default()
                .into(),
        );
        let at = if original.is_some() { 2 } else { 1 };
        let cursor = values[at].len();
        Self {
            values,
            at,
            cursor,
            original,
            access_at: 0,
        }
    }
    fn definition(&self) -> Result<Value, String> {
        if !["project", "personal"].contains(&self.values[0].as_str()) {
            return Err("Scope must be project or personal".into());
        }
        let definition = json!({"v":1,"id":self.values[1],"name":self.values[2],"description":self.values[3],
            "model":self.values[4],"capabilities":self.values[5].split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>(),"instructions":self.values[6]});
        lattice::experts::Definition::parse(&serde_json::to_vec(&definition).unwrap())?;
        Ok(definition)
    }
    fn insert(&mut self, text: &str) -> Result<(), String> {
        if matches!(self.at, 0 | 4 | 5) {
            return Err("Use the arrow keys to select a value for this field".into());
        }
        if text
            .chars()
            .any(|c| c.is_control() && !(self.at == 6 && matches!(c, '\n' | '\t' | '\r')))
        {
            return Err("This field accepts a single line".into());
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.values[self.at].insert_str(self.cursor, &text);
        self.cursor += text.len();
        Ok(())
    }
    fn key(&mut self, key: KeyCode) {
        if self.at == 0 && !matches!(key, KeyCode::Tab | KeyCode::BackTab) {
            if matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                self.values[0] = if self.values[0] == "project" {
                    "personal"
                } else {
                    "project"
                }
                .into();
                self.cursor = self.values[0].len();
            }
            return;
        }
        if self.at == 5 && !matches!(key, KeyCode::Tab | KeyCode::BackTab) {
            use lattice::view::expert_panel::ACCESS;
            match key {
                KeyCode::Left => self.access_at = self.access_at.saturating_sub(1),
                KeyCode::Right => self.access_at = (self.access_at + 1).min(ACCESS.len() - 1),
                KeyCode::Char(' ') => {
                    let current: Vec<_> = self.values[5].split(',').map(str::trim).collect();
                    let selected = ACCESS[self.access_at].0;
                    self.values[5] = ACCESS
                        .iter()
                        .filter(|(id, _)| current.contains(id) != (*id == selected))
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.cursor = self.values[5].len();
                }
                _ => {}
            }
            return;
        }
        if self.at == 4 && !matches!(key, KeyCode::Tab | KeyCode::BackTab) {
            return;
        }
        let text = &mut self.values[self.at];
        match key {
            KeyCode::Tab | KeyCode::BackTab => {
                let start = if self.original.is_some() { 2 } else { 0 };
                let count = FIELDS.len() - start;
                self.at = start
                    + (self.at - start + if key == KeyCode::Tab { 1 } else { count - 1 }) % count;
                self.cursor = self.values[self.at].len();
            }
            KeyCode::Left => {
                self.cursor = text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => {
                self.cursor += text[self.cursor..].chars().next().map_or(0, char::len_utf8)
            }
            KeyCode::Up | KeyCode::Down if self.at == 6 => {
                let start = text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = text[start..self.cursor].chars().count();
                let range = if key == KeyCode::Up && start > 0 {
                    let end = start - 1;
                    Some((text[..end].rfind('\n').map_or(0, |i| i + 1), end))
                } else if key == KeyCode::Down {
                    text[self.cursor..].find('\n').map(|i| {
                        let next = self.cursor + i + 1;
                        (
                            next,
                            next + text[next..].find('\n').unwrap_or(text.len() - next),
                        )
                    })
                } else {
                    None
                };
                if let Some((start, end)) = range {
                    self.cursor = start
                        + text[start..end]
                            .char_indices()
                            .nth(column)
                            .map_or(end - start, |(i, _)| i);
                }
            }
            KeyCode::Home => self.cursor = text[..self.cursor].rfind('\n').map_or(0, |i| i + 1),
            KeyCode::End => {
                self.cursor += text[self.cursor..]
                    .find('\n')
                    .unwrap_or(text.len() - self.cursor)
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = text[..self.cursor].char_indices().next_back().unwrap().0;
                text.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < text.len() => {
                text.remove(self.cursor);
            }
            KeyCode::Enter if self.at == 6 => {
                let _ = self.insert("\n");
            }
            KeyCode::Char(c) if !c.is_control() => {
                let _ = self.insert(&c.to_string());
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub(super) struct ExpertControls {
    listing: Value,
    selected: usize,
    details: Option<Value>,
    form: Option<Form>,
    pending: Option<(String, String)>,
    outgoing: Vec<(String, String, Value)>,
    notice: String,
}
impl ExpertControls {
    fn request(&mut self, operation: &str, arguments: Value, intent: &str) {
        if self.pending.is_some() {
            return;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!("panel-{}-{clock}-{serial}", std::process::id());
        self.pending = Some((id.clone(), intent.into()));
        self.notice = format!("{} — waiting for result or confirmation", operation);
        self.outgoing.push((id, operation.into(), arguments));
    }
    pub fn refresh(&mut self) {
        self.request("list", json!({}), "list");
    }
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }
    pub fn ensure_loaded(&mut self) {
        if self.listing.is_null() && self.form.is_none() {
            self.refresh();
        }
    }
    pub fn flush(&mut self, session: Option<&Session>) {
        for (id, operation, arguments) in self.outgoing.drain(..) {
            if let Some(session) = session {
                session.manage_experts(&id, &operation, arguments);
            } else {
                self.pending = None;
                self.notice = "Expert management needs a live session".into();
            }
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.pending.is_some() {
            return;
        }
        if let Some(form) = self.form.as_mut() {
            self.notice = form.insert(text).err().unwrap_or_default();
        }
    }
    pub fn observe(&mut self, event: &EventEnvelope) {
        if event.event_type != lattice::components::expert_ui::RESULT {
            return;
        }
        let Some((id, intent)) = self.pending.as_ref() else {
            return;
        };
        if event.payload["request"] != *id {
            return;
        }
        let intent = intent.clone();
        self.pending = None;
        if event.payload["status"] != "ok" {
            self.notice = format!(
                "{} — inspect current state before retrying. Draft retained.",
                event.payload["error"]["message"]
                    .as_str()
                    .unwrap_or("Operation interrupted")
            );
            return;
        }
        let result = &event.payload["result"];
        match intent.as_str() {
            "list" => {
                self.listing = result.clone();
                if let Some(rows) = self.listing["experts"].as_array_mut() {
                    rows.sort_by_key(|row| {
                        match row["name"].as_str().unwrap_or_default().split(':').next() {
                            Some("builtin") => 0,
                            Some("project") => 1,
                            _ => 2,
                        }
                    });
                }
                self.selected = self.selected.min(self.rows().len().saturating_sub(1));
                self.notice = "Choose an expert or create your own.".into();
            }
            "inspect" => {
                self.details = Some(result.clone());
                self.notice.clear();
            }
            "create-inspect" => {
                if !result["fileVersion"].is_null() {
                    self.notice =
                        "That identity already exists. Choose another ID; no file was changed."
                            .into();
                    return;
                }
                self.save_with(result);
            }
            "save" => {
                self.form = None;
                self.details = result.get("details").cloned();
                self.notice = if result["unchanged"] == true {
                    "Content unchanged. Existing activation state was preserved.".into()
                } else {
                    "Saved. Activate explicitly to make this revision available.".into()
                };
            }
            "activate" => {
                self.details = result.get("details").cloned();
                self.notice = if result["details"]["ready"] == true {
                    "Activated. This revision is ready to use.".into()
                } else {
                    "Activation recorded. Check availability before delegating.".into()
                };
            }
            "delete" => {
                self.details = None;
                self.notice =
                    "Deleted. Accepted jobs and history are unchanged. Press F5 to refresh.".into();
            }
            _ => {}
        }
    }
    fn rows(&self) -> &[Value] {
        self.listing["experts"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    fn save_with(&mut self, details: &Value) {
        let Some(form) = &self.form else {
            return;
        };
        match form.definition() {
            Ok(definition) => self.request("save", json!({"operation":"put","target":details["target"],"fileVersion":details["fileVersion"],"expectedActivation":details["activation"],"definition":definition,"reason":"Save the expert definition reviewed in the management panel"}), "save"),
            Err(error) => self.notice = error,
        }
    }
    fn open_form(&mut self, definition: &Value, original: Option<Value>) {
        let mut form = Form::new(definition, original);
        self.notice.clear();
        if form.original.is_none() && self.listing["projectAvailable"] == false {
            form.values[0] = "personal".into();
            self.notice = "Project and personal roots coincide; use personal scope.".into();
        }
        self.form = Some(form);
    }

    pub fn key(&mut self, key: KeyCode) -> bool {
        if matches!(key, KeyCode::PageUp | KeyCode::PageDown) {
            return false;
        }
        if self.pending.is_some() {
            return key != KeyCode::Esc;
        }
        if self.form.is_some() {
            if key == KeyCode::Esc {
                self.form = None;
                self.notice = "Draft discarded; no save requested.".into();
            } else if key == KeyCode::F(2) {
                let form = self.form.as_ref().unwrap();
                if let Err(error) = form.definition() {
                    self.notice = error;
                } else if let Some(original) = form.original.clone() {
                    self.save_with(&original);
                } else {
                    let name = format!("{}:{}", form.values[0], form.values[1]);
                    self.request("inspect", json!({"expert":name}), "create-inspect");
                }
            } else if self.form.as_ref().unwrap().at == 0
                && self.listing["projectAvailable"] == false
                && matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char(' '))
            {
                self.notice = "Project and personal roots coincide; use personal scope.".into();
            } else if self.form.as_ref().unwrap().at == 4
                && matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char(' '))
            {
                let choices: Vec<_> = self.listing["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let form = self.form.as_mut().unwrap();
                if choices.is_empty() {
                    self.notice = "No configured models. Add one with /model first.".into();
                } else {
                    let current = choices.iter().position(|id| *id == form.values[4]);
                    let at = current.map_or(0, |i| {
                        (i + if key == KeyCode::Left {
                            choices.len() - 1
                        } else {
                            1
                        }) % choices.len()
                    });
                    form.values[4] = choices[at].into();
                    form.cursor = form.values[4].len();
                }
            } else {
                self.form.as_mut().unwrap().key(key);
            }
            return true;
        }
        match key {
            KeyCode::Esc if self.details.is_some() => {
                self.details = None;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Right | KeyCode::Tab => return false,
            KeyCode::F(5) => {
                self.details = None;
                self.refresh();
            }
            KeyCode::Up if self.details.is_none() => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Down if self.details.is_none() => {
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::Enter if self.details.is_none() => {
                if let Some(row) = self.rows().get(self.selected) {
                    self.request("inspect", json!({"expert":row["name"]}), "inspect");
                }
            }
            KeyCode::Char('n') => {
                self.open_form(&json!({"capabilities":["read"]}), None);
            }
            KeyCode::Char('e' | 'c') if self.details.is_some() => {
                let details = self.details.as_ref().unwrap();
                if key == KeyCode::Char('e') && details["builtin"] == true {
                    self.notice = "Built-ins are read-only. Press c to copy.".into();
                } else {
                    let mut definition = details
                        .get("copyTemplate")
                        .unwrap_or(&details["definition"])
                        .clone();
                    let original = if key == KeyCode::Char('e') {
                        Some(details.clone())
                    } else {
                        definition["id"] = json!("");
                        None
                    };
                    self.open_form(&definition, original);
                }
            }
            KeyCode::Char('a' | 'd') if self.details.is_some() => {
                let details = self.details.as_ref().unwrap();
                let operation = if key == KeyCode::Char('a') {
                    "activate"
                } else {
                    "delete"
                };
                if let Ok(mut arguments) =
                    lattice::experts::catalog::mutation_arguments(details, operation)
                {
                    arguments["reason"] = json!(format!(
                        "{} the inspected expert from its management panel",
                        operation
                    ));
                    self.request(operation, arguments, operation);
                } else {
                    self.notice =
                        "This operation is not available. Inspect a custom expert first.".into();
                }
            }
            KeyCode::PageUp | KeyCode::PageDown => return false,
            _ => {}
        }
        true
    }
    pub fn display(&self) -> lattice::view::expert_panel::Panel {
        use lattice::view::expert_panel::{Mode, Panel};
        let mode = if let Some(form) = &self.form {
            Mode::Form {
                values: form.values.clone(),
                active: form.at,
                cursor: form.cursor,
                editing: form.original.is_some(),
                access_at: form.access_at,
            }
        } else if let Some(details) = &self.details {
            Mode::Detail {
                details: details.clone(),
            }
        } else {
            Mode::List {
                rows: self.rows().to_vec(),
                selected: self.selected,
            }
        };
        Panel {
            mode,
            notice: self.notice.clone(),
            waiting: self.pending.is_some(),
        }
    }
}
