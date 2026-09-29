# Decisions — Local echo over line editing

See `docs/DECISIONS.md` for the legend. Newest entries go at the end. The earlier local-echo
rulings are in `terminal.md` ("Local echo waits for an echo after any key it cannot follow", "A
line editor's prompt is guessed at…", "A guess looks like the text it continues…").

- ✅ **← → and ⌫ are guessed inside a shell's input, in mosh's epochs** (2026-09-29, product
  gap L5). The predictor guessed printable keys only. ⌫ took back its own last guess and nothing
  more, and an arrow dropped every guess until the worker had it. Fixing a typo in the middle
  of a line therefore echoed a round trip late on every key, and a key typed in the middle of
  the text was drawn over the character under the cursor instead of pushing the rest right.
  mosh guesses ← → and ⌫ at the cursor, in epochs that Enter, Esc and ↑ ↓ reset
  ([terminaloverlay.cc](https://github.com/mobile-shell/mosh/blob/master/src/frontend/terminaloverlay.cc),
  `new_user_byte`, `cull`, `kill_epoch`). Rulings (`slopty-predict`):
  1. **Where.** ← and → are guessed only on a row whose OSC 133 mark gives the input's start
     (`SemanticMark::input_col`), outside the tty's canonical mode, and not past the input's
     start. → is not guessed at the end of the typed text: a suggestion from zsh-autosuggestions
     or fish takes it there, and with no suggestion nothing moves. ⌫ is guessed past the
     input's start too, or, on a row with no mark, over what the predictor itself typed in the
     epoch. The alternate screen, a hidden cursor, mouse tracking and a password prompt refuse
     every guess, as before (`TermModes::prediction_allowed`).
  2. **What.** A key is a guess of the whole line after it, not of one cell. A printable key
     inside the text pushes the rest one right. ⌫ pulls it one left and leaves a blank where the
     text ended. A wide glyph is stepped over as one character of two cells. A key that would
     move a wide glyph along the line is not guessed, because the overlay draws single cells.
     The text ends where a suggestion's grey starts (faint, palette 8, the greys of the 256
     palette or a grey true colour) or before a right prompt. A right prompt is text that reaches
     the row's last two columns after two blanks or more. It is remembered once seen, so it is
     still known when zsh draws it one blank from a line that has grown to it. A line the
     guesses push up to it hides it, as the shell does. Typing off a suggestion blanks the rest
     of it, and so does ⌫ at the end of the text.
  3. **Checking.** A frame confirms the latest acknowledged key after which every cell with
     evidence, and the cursor when an arrow or ⌫ is among them, read as guessed. So one frame
     can confirm a burst that a line editor drew at once (a held ←), and a frame that shows
     only part of a burst confirms that part. A frame that still reads as before the first of
     them leaves them waiting, as before; anything else is a miss. A blank is no evidence (mosh:
     "too easy for this to trigger falsely", and a suggestion may fill it). Text the cell
     already showed is no evidence either, except a key typed over a suggestion's own next
     glyph, which the echo confirms only by drawing it as typed text rather than grey. A key
     with no evidence is dropped without counting as a hit.
  4. **Epochs.** Any other key (Enter, Esc, ↑ ↓, Tab, a chord) ends the epoch, as it did. So does
     an edit the predictor cannot follow, and so does a miss: the guesses go, nothing is guessed
     until the worker has every key sent so far, and the next guesses are drawn only once one is
     echoed. A miss in a tentative epoch was never drawn, so it neither mutes nor marks (mosh's
     `kill_epoch`). A drawn miss mutes and marks as before, and now also makes what follows
     tentative, as mosh's `reset` does. A key that went out unguessed while the predictor
     waited moves the line too, so the wait now lasts until the worker has that key as well.
     Before, a guess made after the barrier's acknowledgement landed where an unguessed key's
     echo was about to go. For the same reason, reaching `MAX_PENDING`, a refused mode and a
     `flush` now wait for the worker as well.

  Evidence: `crates/slopty-predict/tests/editing.rs` replays a zle-like editor (insert mode,
  a right prompt, a history suggestion that → at the end takes) over a shaped round trip. The
  run below is the same session before and after, at 20 and 40 ms (MEASUREMENTS, "prediction
  over the line editor's keys"). Fixing a typo mid-line:

  | round trip | keys drawn right as pressed | key → right, mean |
  | --- | --- | --- |
  | 20 ms, before → after | 17 of 47 → 45 of 47 | 12.8 → 0.9 ms |
  | 40 ms, before → after | 17 of 47 → 45 of 47 | 25.5 → 1.7 ms |

  Taking a suggestion and editing it went from 4 of 31 keys drawn as pressed, with 4 wrong
  overlays, to 27 of 31 at 20 ms and 26 of 31 at 40 ms, with none. Over 2 000 random sessions
  (5–85 ms, half with a suggestion, half with a right prompt), the old predictor drew 31 642
  wrong overlays and missed 979 times. The new one drew none and missed none, and none over
  100 000 sessions either. The keys still not drawn as pressed are the warm-up, the → that
  takes a suggestion, and the first keys of the tentative epoch after it.

  Left open: a right prompt that has not been seen apart from the text (history recalls a long
  line with the prompt one blank away) reads as more text, so ⌫ inside that line pulls the
  prompt along and misses; the miss is retracted at the echo. An arrow guessed on its own draws
  no cell, so the keystroke → paint meter counts it as unguessed. Tests: predict
  `arrows_move_the_cursor_within_the_input`, `right_is_not_guessed_past_the_typed_text`,
  `a_mispredicted_arrow_is_retracted`, `erase_inside_the_text_pulls_the_rest_left`,
  `erase_is_not_guessed_before_the_input`, `an_echo_that_disagrees_retracts_the_edit`,
  `epochs_end_on_enter_esc_and_the_vertical_arrows`, `a_tentative_miss_does_not_mute`,
  `a_frame_confirms_what_it_shows`, `wide_glyphs_move_by_two_cells`, `typing_meets_a_suggestion`,
  `arrows_are_guessed_only_at_a_line_editor`, `a_right_prompt_is_not_the_text`; editing
  `editing_keys_show_as_pressed_over_a_round_trip`, `random_editing_never_shows_a_wrong_line`.
