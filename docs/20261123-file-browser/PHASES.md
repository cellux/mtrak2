# Implementation phases

## Purpose

This file turns `PLAN.md` into an ordered set of independently testable
implementation phases. Each phase defines bounded work, dependencies, and
validation criteria. Completed phases are not rewritten; progress belongs in
`DIARY.md`, while durable design decisions belong in `DECISIONS.md`.

## Phase P0 — Workspace key map and screen enum

Depends on: none.

Work:

- Change `Screen` to Help, Browser, Pattern, Code and document the F1/F2/F3/F4
  mapping.
- Change `Action`/`key::lookup` and the global function-key dispatch so that F2
  opens the browser, F3 keeps Pattern, and F4 opens Code.
- Update header, help, and status labels plus the existing screen-switch tests.

Validation: keymap tests for F1–F4, screen-switch tests, and existing Pattern
and Code behavior unchanged.

## Phase P1 — Browser state module

Depends on: P0.

Work:

- Add `crates/mtrak-tui/src/browser.rs`, independent of `ratatui`.
- Model the absolute directory, sorted entries, selected entry identity, filter
  editor, and parent/child navigation memory.
- Implement directory-first alphabetical sorting, hidden-entry inclusion,
  symlink classification, case-sensitive/case-insensitive substring matching,
  match-span calculation, selection clamping, and filter editing.
- Implement directory entry/parent return and the four-byte `MThd` check.

Validation: browser unit tests for sorting, filtering, case rules, Unicode
spans, filter editing, empty/no-match lists, selection clamping, navigation
memory, symlinks, and MIDI header detection.

## Phase P2 — App integration and filesystem effects

Depends on: P1.

Work:

- Add the lazily initialized browser to `App`, with first-use anchoring at the
  loaded file's canonical parent or the process current directory.
- Preserve directory, selection, and filter across F2 re-entry.
- Route Up/Down, Left/Right, Enter, filter editing, Esc, PageUp/PageDown,
  Home/End, and `:` through the browser workspace.
- Change the browser directory with `set_current_dir`, reporting failures in
  the status line.
- Open MIDI files through `App::load`, and make `load` store canonical paths,
  set cwd to the file parent, and switch to Pattern.
- Make `save` store canonical absolute paths, canonicalizing an existing parent
  for new relative output paths without creating directories.

Validation: app integration tests for first-use state, F2 re-entry,
`MThd`/non-MIDI Enter behavior, malformed MIDI errors, directory navigation,
filter/command-mode interaction, `:r`, relative `:w`, plain `:w`, canonical
paths, and missing output directories.

## Phase P3 — Rendering and verification

Depends on: P2.

Work:

- Draw F2 as a full workspace with the absolute directory path, directory
  indicators, selected-row style, matched-substring highlights, and scrolling.
- Handle narrow terminals, empty directories, no-match filters, long paths and
  names, and lists longer than the viewport without panicking.
- Update help/status text and add browser rendering tests.
- Run the full verification gate and record the result in `DIARY.md`.

Validation: UI tests for path/labels/selection/highlighting and edge fixtures;
`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, and `cargo test --workspace` all pass.
