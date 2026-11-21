# Implementation phases

## Purpose

This file turns `PLAN.md` into an ordered sequence of independently testable
implementation phases. Each phase defines a bounded milestone, its
implementation work, validation criteria, and dependencies. Phases are
completed in order and are not rewritten afterward; progress and completed-work
history belong in `DIARY.md`, while durable cross-phase design decisions belong
in `DECISIONS.md`.

## Phase G0 — Event model: generic wire events and SMF Escape

Dependencies: none.

Work:

- Replace `Event::Raw(Vec<u8>)` with `Event::Wire(Vec<u8>)`, a validated
  generic live wire-MIDI byte sequence, and add `Event::SmfEscape(Vec<u8>)`
  for valid Standard MIDI File Escape (`F7`) events.
- Add validation and a byte-sequence parser/serializer for every supported
  `:event` form: channel, sysex (`F0`/`F7` framing), meta (SMF encoding),
  SMF Escape, macro (exactly three bytes below `0x80`), and generic valid
  wire MIDI.
- Make the SMF writer encode wire events into a faithful file representation
  or fail with a location-specific error; `SmfEscape` is file-only.
- Update metadata serialization to the new variants.

Validation: unit tests for validation, parsing, and round-tripping of every
form; malformed input rejected without storage.

## Phase G1 — Global-only settings and module source map

Dependencies: G0.

Work:

- Remove pattern-local root/scale/modal-degree/scale-aware settings from the
  model, engine context, commands, Lua API, and serialization. Keep only
  composition-global settings; simplify `Composition::effective_settings`.
- Replace `TrackerProject::setup_source` with a single ID-keyed module-source
  map that always contains module `00`.

Validation: composition/settings/metadata tests; `:root`, `:scale`, `:mode`
still mutate global settings.

## Phase G2 — Lua built-in modules, `require`, and module `00`

Dependencies: G1.

Work:

- Migrate built-in macros `01` (pattern trigger) and `02` (Q8.8 tempo
  shorthand) from Rust to inspectable, read-only Lua modules.
- Add a user module template; module `00` is editable and its setup side
  effects run once before normal execution.
- Implement numeric-hex `require` with caching, circular-dependency and
  unavailable-ID diagnostics, and non-table return values.

Validation: engine tests for built-in behavior, `require`, setup side effects,
and diagnostics.

## Phase G3 — Nibble grid editing

Dependencies: G0.

Work:

- Six-nibble cursor with track wrapping; temporary six-nibble event buffers
  with immediate conversion, blank-to-zero finalization, and cell-level undo.
- Nibble-level brush, selection, clipboard alignment, `.` whole-slot clearing,
  `Esc` cancellation, and whole-slot structural shifts.
- Replace the standalone modal event editor with `:event` in command mode.

Validation: TUI tests for cursor movement, buffers, selection, clipboard, and
command parsing.

## Phase G4 — Offset editing and visibility

Dependencies: G3.

Work:

- Four-digit offset buffers, range validation, red invalid display, leave-cell
  clamping, and screen-wide `o` visibility that resets on pattern switch.

Validation: TUI tests for formatting, clamping, and visibility reset.

## Phase G5 — Workspaces and chooser

Dependencies: G3, G4.

Work:

- `F1` Help, `F2` Pattern, `F3` Code screens; screen switching finalizes the
  active cell.
- Centered 16×16 chooser with hexadecimal jumps, filled/defined rendering,
  protected/reserved IDs, type-specific copy/cut/paste, and creation defaults.

Validation: TUI tests for switching, chooser navigation, and clipboard rules.

## Phase G6 — Code screen and Lua text editor

Dependencies: G2, G5.

Work:

- Multiline Lua editor with cursor movement, insertion, newlines,
  Backspace/Delete, Tab, viewport scrolling, per-module undo, and `F5`
  validate/reload.
- Practical lexical syntax highlighting.

Validation: TUI tests for editing, highlighting spans, and reload diagnostics.

## Phase G7 — Per-item undo model

Dependencies: G3, G5, G6.

Work:

- Separate content-edit histories per pattern ID and per module ID.
- Separate structural histories for the Pattern and Code choosers; deletion
  and replacement discard the replaced item's content history.

Validation: TUI tests for isolation and cleanup.

## Phase G8 — Strict import, commands, and end-to-end validation

Dependencies: all prior phases.

Work:

- Reject malformed SMF containers/events with a clear bottom-line error and no
  partial import.
- Extend command parsing with `:event`; remove pattern-local commands.
- Round-trip project data and verify export contains no tracker-only metadata
  or project UI state.
- Update `DIARY.md`.

Validation: full workspace test suite green; `cargo fmt`, `cargo clippy`, and
`cargo test` pass.
