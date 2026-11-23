# Implementation diary

## Purpose

This file is an append-only, timestamped log of completed implementation work.
Earlier entries must not be edited. Pending work is tracked in a single
`HANDOFF` section at the end of the file, which is updated before each context
flush so implementation can continue safely.

## 2026-11-23T21:12:16Z — F2 file browser milestone complete

Implemented the filesystem browser and workspace-key shift from `PLAN.md`.

- **Workspace map.** `Screen` now has Help, Browser, Pattern, and Code. F1/F2/F3/
  F4 map to them; header, help, status hints, and existing screen tests were
  updated.
- **Browser state.** Added `mtrak-tui/src/browser.rs` with sorted directory-first
  listings including hidden entries, name-identity selection, a `LineEditor`
  filter, parent/child navigation memory, case-sensitive/case-insensitive
  substring matching, non-overlapping character match spans, symlink
  classification, and four-byte `MThd` detection. The module has no `ratatui`
  dependency.
- **App integration.** F2 initializes once from the loaded file's canonical
  parent or cwd, installs that directory as cwd, and preserves directory,
  selection, and filter on later visits. Up/Down/Page/Home/End move the
  selection, Right/Enter enter directories, Enter opens only `MThd` files, Left
  returns to the remembered child, and typing edits the filter. Filesystem and
  load errors stay in the workspace and are reported on the status line.
- **File commands.** `:r` and browser MIDI opening store canonical absolute
  paths, change cwd to the file parent, and switch to Pattern. `:w` stores a
  canonical absolute path; new relative outputs canonicalize their existing
  parent and never create directories. Plain `:w` stays anchored after the
  browser moves.
- **Rendering.** Added the browser panel with the absolute path, directory
  indicators, selected-row band, filter-match highlighting, and selection
  scrolling. Narrow, empty, long-name, long-path, and long-list cases are
  covered by tests.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 313 tests pass (187 `mtrak-tui`, 46 `mtrak-core`, 44
`mtrak-engine`, 19 `mtrak-midi-io`, 12 end-to-end, 5 `mtrak-midi`, 0 binary).

## HANDOFF

No pending work for this milestone.
