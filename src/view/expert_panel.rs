//! Data-only projection of the frontend's expert manager.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ACCESS: &[(&str, &str)] = &[
    ("read", "Read & search"),
    ("write", "Edit files"),
    ("web", "Web"),
    ("commands", "Commands"),
    ("skill-install", "Install skills"),
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Panel {
    pub mode: Mode,
    pub notice: String,
    pub waiting: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Mode {
    List {
        rows: Vec<Value>,
        selected: usize,
    },
    Detail {
        details: Value,
    },
    Form {
        values: Vec<String>,
        active: usize,
        cursor: usize,
        editing: bool,
        access_at: usize,
    },
}
