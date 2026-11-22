# Implementation phases

## Purpose

This file turns `PLAN.md` into an ordered set of independently implementable and
testable phases. Each phase defines its own scope, implementation work,
acceptance criteria, and verification. Completed phases are not rewritten;
progress belongs in `DIARY.md` and durable choices in `DECISIONS.md`.

## Phase P0 — Chromatic scale and settings simplification

Dependencies: none.

Work:

- Add `chromatic` (`0x0FFF`) to the built-in scales and make it the default
  scale for `Settings::new`.
- Remove `Settings::scale_aware`; make `degree_to_note` always use the selected
  scale's modal intervals, which already reduce to semitones for chromatic.
- Remove `SettingChange::ScaleAware` from the engine macro protocol, scheduler,
  Lua context/read/write API, and `apply_change`.
- Stop serializing the obsolete settings byte in project metadata and stop
  reading it.
- Update the settings, metadata, scheduler, and Lua tests for the new default
  and the removed flag.
- Add a `Settings::is_chromatic` helper used by the TUI note map choice.

Validation: unit tests for the chromatic built-in, the chromatic default,
degree mapping in scale and chromatic modes, metadata round-tripping without
the flag, and Lua settings no longer exposing `scale_aware`.

## Phase P1 — Musical mode model, keymap, and header

Dependencies: P0.

Work:

- Replace `Mode::Normal` and `Mode::Note` with `Mode::Play`, `Mode::Rec`, and
  `Mode::Edit`; keep `Command` and `Chooser` transient. Default to PLAY.
- Add a single mode-transition helper that finalizes the active `CellEdit`
  once, clears selection state when leaving EDIT, preserves cursor row/track/
  nibble, and applies follow when entering PLAY/REC during playback.
- Add `Ctrl-P`, `Ctrl-R`, and `Ctrl-E` bindings; move Redo to `Ctrl-Y`; bind
  uppercase `S` to the start row; remove `Ctrl-N`, `Ctrl-F`, `N`, and the old
  lowercase-`s` start-row binding.
- Remove the follow state, `ToggleFollow`, `ToggleChromatic`, `ToggleNoteMode`,
  and `:follow`.
- Add Alt+digit actions for the session recording step and a `:step N`
  command.
- Render the active musical mode in the header and update help/status text.
- Update keymap, command, UI, and app tests for the removed and moved
  bindings.

Validation: keymap/command parsing tests; header rendering tests; mode-switch
tests proving coordinate preservation, edit finalization, and restored prior
mode after command/chooser.

## Phase P2 — PLAY and REC note behavior

Dependencies: P1.

Work:

- Split the old `note_key` into shared note resolution plus PLAY audition and
  REC write paths.
- Use the selected scale's map; default chromatic input uses the existing piano
  layout.
- Implement independent backward channel and velocity inheritance on the
  selected track with `0`/`100` fallbacks.
- PLAY sends NoteOn and never mutates the pattern or advances the cursor.
- Stopped REC replaces the cell with offset `0`, advances by `record_step`,
  clamps at the final row, and records one undo step per note.
- Gate PLAY/REC editing through the action whitelist from PR-010 and make
  Left/Right select tracks cell-wise.
- Disable nibble typing and nibble Backspace outside EDIT.

Validation: unit tests for scale and chromatic mapping, inheritance cases,
PLAY non-mutation, step REC replacement/offset/advance/clamp/step zero,
undo-per-note, cell-wide track navigation, nibble preservation, and disabled
editing actions.

## Phase P3 — Live recording and stream reconciliation

Dependencies: P2.

Work:

- While REC is active during playback, lock the cursor row to the playhead and
  replace the playhead cell with a note at the current intra-row offset.
- Capture `play_tick - row_start` as the offset and validate it against the
  row length.
- Audition the note immediately, record one undo step, and rebuild the future
  event stream from the composition with `play_next` positioned strictly after
  the current tick.
- Preserve the current tick, fractional microsecond state, playhead row, and
  end tick across the rebuild.
- Ensure the auditioned note is not sent twice and a replaced pending event is
  absent from the rebuilt stream.

Validation: unit tests measuring sent MIDI messages across a live note, the
written slot offset, the rebuilt stream's next event, removal of a replaced
future event, and undo behavior.

## Phase P4 — Playback controls and command-mode pause/resume

Dependencies: P1, P3.

Work:

- Make `Esc` stop active playback before its context-sensitive behavior.
- Make `S` set the start row in all musical modes.
- Implement the command-mode playback pause: capture the playback position and
  state, send All Notes Off, and pause; resume on successful execution or
  cancellation by rebuilding the future stream against the current project.
- Keep playback paused after a failed command, and never start playback when
  command mode was entered while stopped.
- Preserve fractional timing state and the previous musical mode.
- Update the help text, command list, and `:step` handling.

Validation: tests for `Esc` in the playing and stopped states, `S` in every
mode, pause on entering command mode, resume after success and cancellation,
rebuild reflection of project changes, stopped-entry non-start, and failed
command pause retention.

## Phase P5 — Rendering, end-to-end coverage, and verification

Dependencies: P0–P4.

Work:

- Render follow from the mode rather than the removed flag.
- Show the active musical mode in the header and refresh the pattern status
  hint and help screen for the new bindings and removed controls.
- Update the end-to-end TUI scenario to use `Ctrl-R` and the chromatic default,
  and add coverage for the PLAY/REC/EDIT transition surfaces.
- Run the full verification gate and record the result in `DIARY.md`.

Validation: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` all pass.