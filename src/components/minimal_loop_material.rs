//! Paged event pointers. Tombstones preserve ordinal identity when an expanded
//! input supersedes its source; the source ledger remains authoritative.
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::derived_pages::{Item, Pages, Slot};
use crate::LogReader;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    id: String,
    user: bool,
    active: bool,
}

impl Item for Pointer {
    fn lookup_key(&self) -> Option<&str> {
        Some(&self.id)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    pages: Vec<Slot>,
    count: usize,
}

pub(super) struct Store {
    root: Option<PathBuf>,
    pages: Pages<Pointer>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            root: None,
            pages: Pages::open(None, Vec::new(), 0).expect("empty pointer directory is valid"),
        }
    }
}

impl Store {
    pub fn open(reader: &LogReader, state: State) -> io::Result<Self> {
        let root = reader
            .path()
            .filter(|path| path.is_dir())
            .map(ToOwned::to_owned);
        Ok(Self {
            pages: Pages::open(root.clone(), state.pages, state.count)?,
            root,
        })
    }

    pub fn snapshot(&self) -> io::Result<State> {
        Ok(State {
            pages: self.pages.directory()?,
            count: self.pages.len(),
        })
    }

    pub fn fork(&self) -> io::Result<Self> {
        let pages = if self.root.is_some() {
            Pages::open(self.root.clone(), self.pages.directory()?, self.pages.len())?
        } else {
            let mut pages = Pages::open(None, Vec::new(), 0)?;
            for index in 0..self.pages.len() {
                pages.push(self.pages.get(index)?)?;
            }
            pages
        };
        Ok(Self {
            root: self.root.clone(),
            pages,
        })
    }

    pub fn push(&mut self, id: String, user: bool) -> io::Result<()> {
        self.pages.push(Pointer {
            id,
            user,
            active: true,
        })?;
        Ok(())
    }

    pub fn supersede(&mut self, causes: &[String]) -> io::Result<()> {
        for id in causes {
            if let Some(index) = self.pages.find_last(id)? {
                let mut pointer = self.pages.get(index)?;
                if pointer.user && pointer.active {
                    pointer.active = false;
                    self.pages.replace(index, pointer)?;
                }
            }
        }
        Ok(())
    }

    pub fn visit(&self, mut visit: impl FnMut(&str)) -> io::Result<()> {
        for index in 0..self.pages.len() {
            let pointer = self.pages.get(index)?;
            if pointer.active {
                visit(&pointer.id);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn stats(&self) -> (usize, usize) {
        (self.pages.resident_records(), self.pages.read_count())
    }

    #[cfg(test)]
    pub fn collect(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.visit(|id| ids.push(id.to_string())).unwrap();
        ids
    }
}
