//! A prompt the shell redraws after a resize, cleared from its first row.
//!
//! A shell that says it redraws its prompt (`133;A;redraw=1`) has libghostty clear the prompt on
//! a resize, from the prompt's first row down, so what the shell draws again is not left beside
//! a stale copy. libghostty finds that row by walking up from the cursor to the nearest row
//! marked as a prompt start. Its reflow copies a row's mark onto every row that row wraps onto,
//! though (`PageList` `copyRowMetadata`), so a prompt a narrower screen wraps is a column of
//! starts: the clear begins on the cursor's row and the rows above it stay. zsh then redraws
//! from the row it counts up to at the old width, below a stale copy of the prompt's head.
//!
//! So before a change of width the engine clears the prompt itself, at the old width, where the
//! marks are the ones the shell printed, and puts the cursor at the start of its row. The rows
//! the prompt held stay rows through the reflow (a blank prompt row is kept), and the shell's
//! count up lands on the prompt's first row again.

use libghostty_vt::screen::RowSemanticPrompt;
use libghostty_vt::terminal::{Point, PointCoordinate, PromptRedraw};

use super::GhosttyEngine;
use crate::EngineError;

impl GhosttyEngine {
    /// Clear the prompt the cursor is in, when the shell redraws all of it and libghostty's
    /// parser is at ground, so the engine's own sequences cannot land inside the shell's. Both
    /// come from the terminal, which a full reset (RIS) returns to redrawing nothing.
    pub(super) fn clear_prompt_before_reflow(&mut self) -> Result<(), EngineError> {
        if self.term.prompt_redraw()? != PromptRedraw::Full
            || !self.term.is_cursor_at_prompt()?
            || self.on_alt
            || !self.term.is_vt_ground()?
        {
            return Ok(());
        }
        let cursor = self.term.cursor_y()?;
        let Some(start) = self.prompt_start(cursor)? else { return Ok(()) };
        let mut clear = Vec::new();
        for row in start..self.size.rows {
            clear.extend_from_slice(format!("\x1b[{};1H\x1b[2K", row.saturating_add(1)).as_bytes());
        }
        clear.extend_from_slice(format!("\x1b[{};1H", cursor.saturating_add(1)).as_bytes());
        self.term.vt_write(&clear);
        Ok(())
    }

    /// The first row of the prompt the cursor is in, as libghostty's `promptIterator` finds it
    /// walking up from the cursor: the nearest prompt start, or the top of a run of
    /// continuation rows. Only the screen is searched.
    fn prompt_start(&self, cursor: u16) -> Result<Option<u16>, EngineError> {
        let mut run_top = None;
        for y in (0..=cursor).rev() {
            let at = Point::Active(PointCoordinate { x: 0, y: u32::from(y) });
            match self.term.grid_ref(at)?.row()?.semantic_prompt()? {
                RowSemanticPrompt::Prompt => return Ok(Some(y)),
                RowSemanticPrompt::Continuation => run_top = Some(y),
                RowSemanticPrompt::None if run_top.is_some() => return Ok(run_top),
                RowSemanticPrompt::None => {}
            }
        }
        Ok(run_top)
    }
}
