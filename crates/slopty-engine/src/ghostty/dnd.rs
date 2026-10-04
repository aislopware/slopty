//! Drops onto a program that asks for them through the Kitty drag and drop protocol (OSC 72).
//!
//! The drag happens on a client and the program runs here, so the engine is the protocol's
//! terminal end: a viewer's drag over the tile is reported to the program, the program's answer
//! goes back for the drag's feedback, and on the drop the program reads the types it wants.
//! Files are given as `file://` URLs of the copies uploaded to this machine, never of the
//! client's own files, so the program opens them itself.
//!
//! The drop's bytes are read lazily, as upstream ghostty reads a native drop (#14536): a type
//! may be given before the program asks for it (the client pushed what the program accepted),
//! or only once it asks, when the engine says it wants it ([`EngineEvent::DropWants`]) and its
//! bytes come whole or as a stream. Requests are answered in the order made. A drop the
//! program never accepted is refused, as kitty refuses one: it would never be read or
//! concluded.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::kitty::dnd::{Errno, Event, Operation, Operations, Position};
pub use slopty_proto::terminal::{DropOperation, DropPoint};

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
    /// The drop the program may read, by its index in the drop's list, until it concludes it.
    reps: Vec<RepState>,
}

/// Where the bytes of one dropped type are.
#[derive(Default)]
enum RepState {
    /// Not here, and not asked for.
    #[default]
    Absent,
    /// Asked for ([`EngineEvent::DropWants`]), and not here yet.
    Wanted,
    /// Arriving as a stream before the program asked: held until it does.
    Arriving(Vec<u8>),
    /// Streaming straight into the answer to the program's request `id`.
    Streaming(u32),
    /// Here whole.
    Here(Vec<u8>),
    /// Will not come.
    Gone,
}

/// What became of a drop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dropped {
    /// The program does not ask for drops: it was told nothing.
    NotAsked,
    /// The program never accepted the drag (it did not answer, or answered none): the drop is
    /// refused and concluded as nothing.
    Refused,
    /// The program has the drop, and reads what it wants of it.
    Given,
}

/// Where streamed bytes of a dropped type went ([`GhosttyEngine::drop_chunk`]).
///
/// [`GhosttyEngine::drop_chunk`]: crate::GhosttyEngine::drop_chunk
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Streamed {
    /// Into the answer to the program's request, as its input.
    Answered,
    /// Held whole until the program asks for the type.
    Held,
    /// Nowhere: the type is here whole already, or will not come.
    Unwanted,
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

    /// The viewer dropped a drag of `mimes` onto the terminal at `at`; the program reads what it
    /// wants of it until it concludes the drop. Nothing of it is here yet: what the program asks
    /// for is wanted ([`EngineEvent::DropWants`]) unless given first. A drag the program never
    /// accepted is refused: it hears the drag leave, and the drop concludes as nothing.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn dropped(&mut self, at: DropPoint, mimes: &[String]) -> Result<Dropped, EngineError> {
        if !self.term.dnd_drop_registered()? {
            return Ok(Dropped::NotAsked);
        }
        let accepted = self.term.dnd_drop_accepted()?;
        if accepted.is_none_or(|op| op == Operation::None) {
            let _told = self.term.dnd_drop_leave()?;
            self.drop_ended(DropOperation::None);
            self.after_dnd();
            return Ok(Dropped::Refused);
        }
        let mime_refs: Vec<&str> = mimes.iter().map(String::as_str).collect();
        let told = self.term.dnd_drop(position(at), &mime_refs)?;
        if told == Some(true) {
            self.drop_ended(DropOperation::None);
        }
        if told.is_some() {
            let mut drops = self.drops.drops.borrow_mut();
            drops.reps.clear();
            drops.reps.resize_with(mimes.len(), RepState::default);
        }
        self.after_dnd();
        Ok(if told.is_some() { Dropped::Given } else { Dropped::NotAsked })
    }

    /// The whole bytes of the dropped type at `index`, or `None` when they will not come. A
    /// request waiting for them is answered; given ahead of one, they wait for it.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drop_data(&mut self, index: usize, data: Option<Vec<u8>>) -> Result<(), EngineError> {
        if let Some(rep) = self.drops.drops.borrow_mut().reps.get_mut(index) {
            *rep = data.map_or(RepState::Gone, RepState::Here);
        }
        self.serve_drop()
    }

    /// More bytes of the dropped type at `index`, as they stream in. The program's request for
    /// it, when it is the one being answered, gets them at once; otherwise they are held for it.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drop_chunk(&mut self, index: usize, bytes: &[u8]) -> Result<Streamed, EngineError> {
        let serving = self.serving(index)?;
        let mut drops = self.drops.drops.borrow_mut();
        let Some(rep) = drops.reps.get_mut(index) else { return Ok(Streamed::Unwanted) };
        Ok(match (std::mem::take(rep), serving) {
            (RepState::Absent | RepState::Wanted | RepState::Streaming(_), Some(id)) => {
                *rep = RepState::Streaming(id);
                drop(drops);
                self.term.dnd_drop_respond_data(id, bytes)?;
                Streamed::Answered
            }
            (RepState::Absent | RepState::Wanted, None) => {
                *rep = RepState::Arriving(bytes.to_vec());
                Streamed::Held
            }
            (RepState::Arriving(mut held), _) => {
                held.extend_from_slice(bytes);
                *rep = RepState::Arriving(held);
                Streamed::Held
            }
            // Given already, or gone: a late stream adds nothing.
            (state, _) => {
                *rep = state;
                Streamed::Unwanted
            }
        })
    }

    /// The stream of the dropped type at `index` ended: `complete` when all of it came. A
    /// request it streamed into is ended (or failed), and the next one served.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn drop_end(&mut self, index: usize, complete: bool) -> Result<(), EngineError> {
        let ended = {
            let mut drops = self.drops.drops.borrow_mut();
            let Some(rep) = drops.reps.get_mut(index) else { return Ok(()) };
            match std::mem::take(rep) {
                // Streamed into the answer, so not kept: asked again, it is wanted again.
                RepState::Streaming(id) => Some(id),
                RepState::Arriving(held) if complete => {
                    *rep = RepState::Here(held);
                    None
                }
                RepState::Arriving(_) | RepState::Wanted | RepState::Absent => {
                    *rep = if complete { RepState::Here(Vec::new()) } else { RepState::Gone };
                    None
                }
                state => {
                    *rep = state;
                    None
                }
            }
        };
        if let Some(id) = ended {
            if complete {
                self.term.dnd_drop_respond_end(id)?;
            } else {
                self.term.dnd_drop_respond_error(id, Errno::Eio)?;
            }
        }
        self.serve_drop()
    }

    /// The program's request being answered, when it is for the dropped type at `index`.
    fn serving(&self, index: usize) -> Result<Option<u32>, EngineError> {
        Ok(self
            .term
            .dnd_drop_request()?
            .filter(|r| usize::try_from(r.mime_index).ok() == Some(index))
            .map(|r| r.id))
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

    /// Answer the program's drop requests while their data is here, and say which type the
    /// first waiting one wants.
    fn serve_drop(&mut self) -> Result<(), EngineError> {
        while let Some(request) = self.term.dnd_drop_request()? {
            let id = request.id;
            let index = usize::try_from(request.mime_index).unwrap_or(usize::MAX);
            let answer = {
                let mut drops = self.drops.drops.borrow_mut();
                match drops.reps.get_mut(index) {
                    Some(rep @ RepState::Absent) => {
                        *rep = RepState::Wanted;
                        drop(drops);
                        self.events.borrow_mut().push(EngineEvent::DropWants { index });
                        return Ok(());
                    }
                    Some(RepState::Wanted | RepState::Arriving(_) | RepState::Streaming(_)) => {
                        return Ok(());
                    }
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
