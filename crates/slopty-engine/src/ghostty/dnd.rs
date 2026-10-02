//! Drops onto a program that asks for them through the Kitty drag and drop protocol (OSC 72).
//!
//! The drag happens on a client and the program runs here, so the engine is the protocol's
//! terminal end: a viewer's drag over the tile is reported to the program, the program's answer
//! goes back for the drag's feedback, and on the drop the program reads the representations it
//! wants. Files are given as `file://` URLs of the copies uploaded to this machine, never of the
//! client's own files, so the program opens them itself.
//!
//! A representation may be given on the drop or after it (an upload finishing): a request for
//! one that is still coming waits for it, and requests are answered in the order made.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::kitty::dnd::{Errno, Event, Operation, Operations, Position};
pub use slopty_proto::terminal::{DropOperation, DropPoint, DropRep};

use super::GhosttyEngine;
use crate::{EngineError, EngineEvent};

const fn operation(operation: Operation) -> DropOperation {
    match operation {
        Operation::None => DropOperation::None,
        Operation::Copy => DropOperation::Copy,
        Operation::Move => DropOperation::Move,
    }
}

const fn position(at: DropPoint) -> Position {
    let operations = match (at.copy, at.moves) {
        (true, true) => Operations::ANY,
        (true, false) => Operations::COPY,
        (false, true) => Operations::MOVE,
        (false, false) => Operations::NONE,
    };
    Position {
        cell_x: at.col as u32,
        cell_y: at.row as u32,
        pixel_x: at.x,
        pixel_y: at.y,
        operations,
    }
}

/// The protocol's state the engine keeps, shared with the callback.
#[derive(Default)]
pub(super) struct Drops {
    /// What the program did during the last write, in order.
    events: Vec<Event>,
    /// The drop the program may read, until it concludes it.
    reps: Vec<Rep>,
}

/// A representation as the engine holds it.
struct Rep {
    mime: String,
    state: RepState,
}

enum RepState {
    Coming,
    Here(Vec<u8>),
    Gone,
}

/// The callback's side: what happened, and a flag the write path reads for free.
pub(super) struct Shared {
    drops: Rc<RefCell<Drops>>,
    moved: Rc<Cell<bool>>,
}

/// Turn the protocol on: the program may now register for drops.
pub(super) fn install(term: &mut Terminal<'static, 'static>) -> Result<Shared, EngineError> {
    let drops = Rc::new(RefCell::new(Drops::default()));
    let moved = Rc::new(Cell::new(false));
    let (for_events, for_moved) = (Rc::clone(&drops), Rc::clone(&moved));
    term.on_kitty_dnd(move |_, event| {
        for_events.borrow_mut().events.push(event);
        for_moved.set(true);
    })?;
    Ok(Shared { drops, moved })
}

impl GhosttyEngine {
    /// Whether the program asks for drops: a viewer's drag over the tile then goes to
    /// [`Self::drag_over`] instead of being pasted as paths.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drop_target(&self) -> Result<bool, EngineError> {
        Ok(self.term.dnd_drop_registered()?)
    }

    /// A viewer's drag of `mimes` is over the terminal at `at`. `false` when the program does
    /// not ask for drops, and nothing was told.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drag_over(&mut self, at: DropPoint, mimes: &[String]) -> Result<bool, EngineError> {
        let mimes: Vec<&str> = mimes.iter().map(String::as_str).collect();
        let told = self.term.dnd_drop_move(position(at), &mimes)?;
        if told == Some(true) {
            self.drop_ended(DropOperation::None);
        }
        self.after_dnd();
        Ok(told.is_some())
    }

    /// The viewer's drag left the terminal without dropping.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drag_left(&mut self) -> Result<(), EngineError> {
        let _told = self.term.dnd_drop_leave()?;
        self.after_dnd();
        Ok(())
    }

    /// The viewer dropped `reps` onto the terminal at `at`; the program reads what it wants of
    /// them until it concludes the drop. `false` when the program does not ask for drops.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn dropped(&mut self, at: DropPoint, reps: Vec<DropRep>) -> Result<bool, EngineError> {
        let mimes: Vec<String> = reps.iter().map(|r| r.mime.clone()).collect();
        let mime_refs: Vec<&str> = mimes.iter().map(String::as_str).collect();
        let told = self.term.dnd_drop(position(at), &mime_refs)?;
        if told == Some(true) {
            self.drop_ended(DropOperation::None);
        }
        if told.is_some() {
            self.drops.drops.borrow_mut().reps = reps
                .into_iter()
                .map(|r| Rep {
                    mime: r.mime,
                    state: r.data.map_or(RepState::Coming, RepState::Here),
                })
                .collect();
        }
        self.after_dnd();
        Ok(told.is_some())
    }

    /// The bytes of the dropped `mime` that were still coming, or `None` when they will not
    /// come (the upload failed). A request waiting for them is answered.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drop_data(&mut self, mime: &str, data: Option<Vec<u8>>) -> Result<(), EngineError> {
        {
            let mut drops = self.drops.drops.borrow_mut();
            if let Some(rep) = drops
                .reps
                .iter_mut()
                .find(|r| r.mime == mime && matches!(r.state, RepState::Coming))
            {
                rep.state = data.map_or(RepState::Gone, RepState::Here);
            }
        }
        self.serve_drop()
    }

    /// What the program did with drops during a write: told as events, and its requests
    /// answered as far as the data is here.
    #[inline]
    pub(super) fn after_dnd(&mut self) {
        // Inlined so a write the program did no drag and drop in pays one load and a branch.
        if self.drops.moved.get() {
            self.heard_dnd();
        }
    }

    #[inline(never)]
    fn heard_dnd(&mut self) {
        self.drops.moved.set(false);
        let events = std::mem::take(&mut self.drops.drops.borrow_mut().events);
        for event in events {
            match event {
                Event::Registration => {
                    let accepts = self.term.dnd_drop_registered().unwrap_or(false);
                    self.events.borrow_mut().push(EngineEvent::DropTarget { accepts });
                }
                Event::Acceptance => {
                    let operation = self
                        .term
                        .dnd_drop_accepted()
                        .ok()
                        .flatten()
                        .map_or(DropOperation::None, operation);
                    let mimes = self
                        .term
                        .dnd_drop_accepted_mimes()
                        .ok()
                        .flatten()
                        .map(|raw| {
                            raw.split(|&b| b == 0)
                                .filter(|m| !m.is_empty())
                                .map(|m| String::from_utf8_lossy(m).into_owned())
                                .collect()
                        })
                        .unwrap_or_default();
                    self.events.borrow_mut().push(EngineEvent::DropAccepted { operation, mimes });
                }
                Event::Concluded(op) => self.drop_ended(operation(op)),
                // Requests are served below, in order; a drag the program offers is not
                // carried to the clients yet.
                _ => {}
            }
        }
        if let Err(e) = self.serve_drop() {
            tracing::debug!(error = %e, "a drop request could not be answered");
        }
    }

    /// The program concluded the drop with `operation`, or another drag ended it.
    fn drop_ended(&self, operation: DropOperation) {
        self.drops.drops.borrow_mut().reps.clear();
        self.events.borrow_mut().push(EngineEvent::DropConcluded { operation });
    }

    /// Answer the program's drop requests while their data is here.
    fn serve_drop(&mut self) -> Result<(), EngineError> {
        while let Some(request) = self.term.dnd_drop_request()? {
            let id = request.id;
            let index = usize::try_from(request.mime_index).unwrap_or(usize::MAX);
            let answer = {
                let drops = self.drops.drops.borrow();
                match drops.reps.get(index).map(|r| &r.state) {
                    Some(RepState::Coming) => return Ok(()),
                    Some(RepState::Here(data)) => Ok(data.clone()),
                    Some(RepState::Gone) => Err(Errno::Eio),
                    None => Err(Errno::Enoent),
                }
            };
            // The answers write to the pty through the pty-write callback: no borrow is held.
            match answer {
                Ok(data) => {
                    self.term.dnd_drop_respond_data(id, &data)?;
                    self.term.dnd_drop_respond_end(id)?;
                }
                Err(errno) => self.term.dnd_drop_respond_error(id, errno)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
