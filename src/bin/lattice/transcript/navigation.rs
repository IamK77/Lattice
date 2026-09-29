//! A viewport is addressed by content, not by an invented total row count.

use std::io;

use super::{render::render_transcript_group, wrap_positioned, Row};
use lattice::view::View;
use lattice::view::{TranscriptBlock as Block, TranscriptPosition as Position};

#[derive(Clone, Debug)]
pub(crate) struct Located {
    pub row: Row,
    pub position: Position,
}

pub(crate) struct Page {
    pub rows: Vec<Located>,
    pub before: bool,
    pub after: bool,
    #[cfg(test)]
    pub groups_built: usize,
}

#[cfg(test)]
#[path = "navigation/tests.rs"]
mod tests;

struct Cursor<'a> {
    view: &'a dyn View,
    spinner: char,
    width: usize,
    height: usize,
    block: Block,
    end: usize,
    rows: Vec<Located>,
    at: usize,
    groups_built: usize,
}

impl<'a> Cursor<'a> {
    fn new(view: &'a dyn View, spinner: char, width: usize, height: usize) -> Self {
        Self {
            view,
            spinner,
            width,
            height,
            block: Block::Streaming,
            end: view.entry_count(),
            rows: Vec::new(),
            at: 0,
            groups_built: 0,
        }
    }

    fn load(&mut self, block: Block) -> io::Result<()> {
        self.block = block;
        self.at = 0;
        let logical = match block {
            Block::Welcome => {
                self.rows = crate::terminal_host::brand::brand_art(
                    self.view.title(),
                    self.width,
                    self.height,
                )
                .into_iter()
                .enumerate()
                .map(|(line, text)| Located {
                    row: Row {
                        line: text,
                        owner: None,
                        links: Vec::new(),
                    },
                    position: Position {
                        block,
                        line,
                        byte: 0,
                    },
                })
                .collect();
                return Ok(());
            }
            Block::Streaming => {
                let total = self.view.entry_count();
                let loaded = self.view.transcript_group(total)?;
                if loaded.first != total || loaded.total != total || !loaded.entries.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "transcript tail is not empty at its boundary",
                    ));
                }
                render_transcript_group(self.view, self.spinner, self.width, &loaded, true)
            }
            Block::Entries(first) if first < self.view.entry_count() => {
                let loaded = self.view.transcript_group(first)?;
                let end = loaded
                    .first
                    .checked_add(loaded.entries.len())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "transcript group ordinal overflow",
                        )
                    })?;
                if loaded.first > first
                    || end <= first
                    || end > loaded.total
                    || loaded.total != self.view.entry_count()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "transcript group does not contain its requested ordinal",
                    ));
                }
                self.block = Block::Entries(loaded.first);
                self.end = end;
                self.groups_built += 1;
                render_transcript_group(self.view, self.spinner, self.width, &loaded, false)
            }
            Block::Entries(_) => return self.load(Block::Streaming),
        };
        self.rows = wrap_positioned(logical, self.width)
            .into_iter()
            .map(|located| Located {
                row: located.row,
                position: Position {
                    block: located.entry.map(Block::Entries).unwrap_or(self.block),
                    line: located.line,
                    byte: located.byte,
                },
            })
            .collect();
        Ok(())
    }

    fn previous_block(&self) -> Option<Block> {
        match self.block {
            Block::Welcome => None,
            Block::Entries(0) => Some(Block::Welcome),
            Block::Entries(first) => Some(Block::Entries(first - 1)),
            Block::Streaming => Some(
                self.view
                    .entry_count()
                    .checked_sub(1)
                    .map(Block::Entries)
                    .unwrap_or(Block::Welcome),
            ),
        }
    }

    fn next_block(&self) -> Option<Block> {
        match self.block {
            Block::Streaming => None,
            Block::Welcome if self.view.entry_count() == 0 => Some(Block::Streaming),
            Block::Welcome => Some(Block::Entries(0)),
            Block::Entries(_) if self.end < self.view.entry_count() => {
                Some(Block::Entries(self.end))
            }
            Block::Entries(_) => Some(Block::Streaming),
        }
    }

    fn cross(&mut self, forward: bool) -> io::Result<bool> {
        loop {
            let next = if forward {
                self.next_block()
            } else {
                self.previous_block()
            };
            let Some(next) = next else { return Ok(false) };
            self.load(next)?;
            if !self.rows.is_empty() {
                self.at = if forward { 0 } else { self.rows.len() - 1 };
                return Ok(true);
            }
        }
    }

    fn shift(&mut self, mut count: usize, forward: bool) -> io::Result<()> {
        while count > 0 {
            let room = if forward {
                self.rows.len().saturating_sub(self.at + 1)
            } else {
                self.at
            };
            let step = count.min(room);
            if forward {
                self.at += step;
            } else {
                self.at -= step;
            }
            count -= step;
            if count == 0 || !self.cross(forward)? {
                break;
            }
            count -= 1;
        }
        Ok(())
    }

    fn end(&mut self) -> io::Result<()> {
        self.load(Block::Streaming)?;
        if self.rows.is_empty() {
            self.cross(false)?;
        }
        self.at = self.rows.len().saturating_sub(1);
        Ok(())
    }

    fn seek(&mut self, position: Position) -> io::Result<()> {
        self.load(position.block)?;
        if self.rows.is_empty() {
            if !self.cross(true)? {
                self.end()?;
            }
            return Ok(());
        }
        self.at = self
            .rows
            .iter()
            .rposition(|row| {
                row.position.block == position.block
                    && (row.position.line, row.position.byte) <= (position.line, position.byte)
            })
            .unwrap_or_else(|| self.rows.len().saturating_sub(1));
        Ok(())
    }

    fn collect(mut self) -> io::Result<Page> {
        let before = self.at > 0 || self.previous_block().is_some();
        let mut rows = Vec::with_capacity(self.height);
        let mut after = false;
        for _ in 0..self.height {
            let Some(row) = self.rows.get(self.at) else {
                break;
            };
            rows.push(row.clone());
            after = if self.at + 1 < self.rows.len() {
                self.at += 1;
                true
            } else {
                self.cross(true)?
            };
            if !after {
                break;
            }
        }
        Ok(Page {
            rows,
            before,
            after,
            #[cfg(test)]
            groups_built: self.groups_built,
        })
    }
}

pub(crate) fn page(
    view: &dyn View,
    spinner: char,
    width: usize,
    height: usize,
) -> io::Result<Page> {
    if height == 0 || (view.panel().is_some() && view.pending_auth().is_none()) {
        return Ok(Page {
            rows: Vec::new(),
            before: false,
            after: false,
            #[cfg(test)]
            groups_built: 0,
        });
    }
    let mut cursor = Cursor::new(view, spinner, width, height);
    if let Some((position, shift)) = view.transcript_position().filter(|_| view.scroll() > 0) {
        cursor.seek(position)?;
        cursor.shift(shift.unsigned_abs(), shift >= 0)?;
        let page = cursor.collect()?;
        // Near the end, fill upwards just as bottom-following does.
        if page.rows.len() == height {
            return Ok(page);
        }
        let mut cursor = Cursor::new(view, spinner, width, height);
        cursor.end()?;
        cursor.shift(height - 1, false)?;
        return cursor.collect();
    }
    cursor.end()?;
    cursor.shift(view.scroll().saturating_add(height - 1), false)?;
    cursor.collect()
}
