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
use libghostty_vt::terminal::{Point, PointCoordinate};

use super::GhosttyEngine;
use crate::EngineError;
use crate::osc133::Redraw;

impl GhosttyEngine {
    /// Clear the prompt the cursor is in, when the shell redraws all of it and the bytes so far
    /// leave the parser where the engine's own sequences cannot land inside the shell's.
    pub(super) fn clear_prompt_before_reflow(&mut self) -> Result<(), EngineError> {
        if self.prompt_redraw != Redraw::Full
            || !self.at_prompt
            || !self.prompt_at_ground
            || self.on_alt
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

/// Whether VT bytes end at ground, with no escape sequence, control string or UTF-8 character
/// left open for the next write to finish, given whether the bytes before them did. Judged from
/// the last escape on, and wrong only towards "open": a control string holding escapes of its
/// own (tmux's passthrough) reads as open.
pub(super) fn ends_at_ground(was: bool, bytes: &[u8]) -> bool {
    let closed = match memchr::memrchr(0x1b, bytes) {
        None => was,
        Some(at) => match bytes.get(at.saturating_add(1)..).unwrap_or_default().split_first() {
            None | Some((b'P' | b'X' | b'^' | b'_', _)) => false,
            Some((b'[', rest)) => rest.iter().any(|b| (0x40..=0x7e).contains(b)),
            Some((b']', rest)) => rest.contains(&0x07),
            Some((0x20..=0x2f, rest)) => rest.iter().any(|b| (0x30..=0x7e).contains(b)),
            Some(_) => true,
        },
    };
    closed && utf8_complete(bytes)
}

/// Whether `bytes` do not end inside a UTF-8 character.
fn utf8_complete(bytes: &[u8]) -> bool {
    for (back, &b) in bytes.iter().rev().take(4).enumerate() {
        if b & 0xc0 == 0x80 {
            continue;
        }
        let len = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return back.saturating_add(1) >= len;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_is_judged_from_the_last_escape_on() {
        let cases: &[(bool, &[u8], bool)] = &[
            (true, b"plain", true),
            (false, b"plain", false),
            (false, b"\x1b[0mtyped", true),
            (true, b"\x1b[3", false),
            (true, b"\x1b[38;5", false),
            (true, b"\x1b]133;B\x07ls", true),
            (true, b"\x1b]0;title", false),
            (true, b"\x1b]0;title\x1b\\", true),
            (true, b"\x1b_Gf=100;AAAA", false),
            (true, b"\x1bP", false),
            (true, b"\x1b(B", true),
            (true, b"\x1b(", false),
            (true, b"\x1b7", true),
            (true, b"\x1b", false),
            (true, "d\u{e9}j\u{e0}".as_bytes(), true),
            (true, &[b'a', 0xc3], false),
            (true, &[0xe2, 0x94], false),
            (true, &[0xe2, 0x94, 0x80], true),
            (true, &[0xf0, 0x9f, 0x98], false),
        ];
        for &(was, bytes, at_ground) in cases {
            assert_eq!(ends_at_ground(was, bytes), at_ground, "{was} {bytes:?}");
        }
    }
}
