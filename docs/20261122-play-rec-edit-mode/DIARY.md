# Implementation diary

## Purpose

This file is an append-only, timestamped log of completed implementation work.
Earlier entries must not be edited. Pending work will be tracked in a single
`HANDOFF` section at the end of the file, which must be updated before each
context flush so another agent can resume the work.

## 2026-11-22T19:36:00Z — PLAY/REC/EDIT milestone phases P0–P5 complete

Implemented the full PLAY/REC/EDIT revamp from `PLAN.md` across `mtrak-core`,
`mtrak-engine`, `mtrak-midi-io`, and `mtrak-tui`. Decisions PR-001…PR-010 were
recorded before coding.

- **P0 — Chromatic scale and settings.** Added `chromatic` as a built-in
  `0x0FFF` scale and made it the default. Removed `Settings::scale_aware`;
  `degree_to_note` now always maps through the selected scale's modal
  intervals (chromatic reduces to semitones). Removed `SettingChange::ScaleAware`
  from the macro protocol, scheduler, and Lua context/read/write API, and
  removed the obsolete settings byte from project metadata. Added
  `Settings::is_chromatic`.
- **P1 — Mode model, keymap, header.** Replaced `Mode::Normal`/`Mode::Note`
  with `Play`/`Rec`/`Edit`; PLAY is the default. Added a single `set_mode`
  helper that finalizes a partial `CellEdit` once, preserves cursor
  row/track/nibble, and drops selection state for PLAY/REC. Added `Ctrl-P`,
  `Ctrl-R`, `Ctrl-E`, moved Redo to `Ctrl-Y`, moved the playback start row to
  uppercase `S`, removed `Ctrl-N`, `Ctrl-F`, `N`, `:follow`, the follow flag,
  and the chromatic toggle. Added session `record_step` (default 1),
  `Alt+0..9`, and `:step N`. The header renders the active mode.
- **P2 — PLAY and REC notes.** Added shared scale-aware note resolution
  (piano layout for chromatic), independent backward channel/velocity scans
  with `0`/`100` fallbacks, PLAY audition without mutation or movement,
  stopped-REC replacement with offset `0`, recording-step advance/clamp, and
  one undo step per recorded note. PLAY/REC use cell-wide Left/Right and
  Tab/Shift-Tab track selection while retaining the nibble; editing actions
  are gated except undo/redo.
- **P3 — Live REC.** REC follows the playhead while playing, writes the note at
  the playhead cell with `play_tick - row_start` as its offset, auditions it
  immediately, and rebuilds the future stream with `play_next` strictly after
  the current tick. This drops replaced pending events and macro descendants
  without provenance and never resends the auditioned note. Undo/redo while
  playing also rebuild the future stream.
- **P4 — Controls and command pause.** `Esc` stops active playback before any
  context-sensitive cancel; `S` works in every musical mode; Space still
  toggles in every mode. Command mode captures tick/micros/start row/current
  row, sends All Notes Off, pauses, and on success or cancellation restores the
  musical mode and resumes from the same tick against a freshly rendered
  stream. Failed commands stay paused; stopped entry never starts playback.
- **P5 — Rendering and verification.** Follow rendering is derived from the
  mode. Help/status text documents the new bindings and `:step`. Updated the
  end-to-end TUI scenario to use REC and the chromatic default, and added
  rendering tests for the header modes and mode-derived follow.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 284 tests (158 `mtrak-tui`, 46 `mtrak-core`, 44
`mtrak-engine`, 19 `mtrak-midi-io`, 12 end-to-end, 5 `mtrak-midi`).

## 2026-11-22T19:45:31Z — Start-row marker in the pattern gutter

Added a visible playback start-row indicator to the pattern grid. The start
row uses the same `▶` glyph as the playhead in the existing spacer column one
to the right of the playhead marker, styled yellow (and kept on the playing-row
band when both markers coincide). The gutter width is unchanged. Added a UI
test that checks the marker column, its absence on other rows, and both markers
appearing side by side when the playhead reaches the start row.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 285 tests.

## 2026-11-22T19:49:49Z — Start-row marker moved into the playhead column

Moved the playback start-row `▶` marker into the same gutter column as the
playhead marker, relying on color to distinguish them: yellow for the
configured start row, white on the play band for the live playhead. When the
playhead reaches the start row, the playhead marker takes precedence. The
spacer column stays blank and the gutter width is unchanged. Updated the UI
test to assert the shared column, the marker colors, and the playhead
precedence.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 285 tests.

## 2026-11-22T19:55:06Z — Header mode badge is four characters wide

Reduced the header mode field from a fixed six-character pad to four, so the
stylized `PLAY`, `REC`, and `EDIT` badges no longer carry unnecessary trailing
highlight width. `CHOOSE` naturally exceeds the field; `CMD` pads to four.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean; the affected UI
test passes.

## 2026-11-22T20:01:13Z — Session keyboard transpose (`F7`/`F8`, `:transpose`)

Added a session-local keyboard transposition offset in semitones (default `0`,
range `-127..=127`) used by PLAY audition and REC recording. `F7` and `F8`
decrease/increase it by one octave and saturate at the range limits;
`:transpose N` sets an absolute value and `:transpose` reports the current one.
The offset is applied after scale-degree resolution, so recorded notes store
the transposed pitch, and resulting notes clamp to `0..=127`. It is not
serialized and does not mark the project dirty. The header shows `TRANS +N`
when non-zero, and help/status text mentions the keys and command.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 287 tests.

## 2026-11-22T20:02:16Z — Pattern LPB in the header

Added the current pattern's lines-per-beat after its ID in the header, e.g.
`PAT 00  LPB 4`. Updated the header UI test to assert the field.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 287 tests.

## 2026-11-22T20:15:20Z — `.` clear available in PLAY and REC

Made the `.` clear action available in all three musical modes. In EDIT it
keeps the existing byte-aligned selection/brush semantics. In PLAY and REC it
clears the whole event cell at the logical cursor regardless of the preserved
nibble coordinate, records one undo step, preserves the cursor, and rebuilds
the future playback stream when playback is running so a cleared pending event
cannot sound. Recorded the change as PR-011, superseding the `.` portion of
PR-010, and added tests for the cell-wide clear, undo, and pending-event
removal.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` 289 tests.

## HANDOFF

**State:** The PLAY/REC/EDIT milestone is implemented and green, including a
yellow start-row marker, a session keyboard transpose (`F7`/`F8`,
`:transpose`), pattern LPB in the header, and the `.` cell clear in every
musical mode. No pending work is known.

**Durable decisions:** PR-001…PR-010 in `DECISIONS.md`.

**Validation commands:**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

**Known deliberate limitations:**

- Live recording and command resume rebuild the complete future event stream
  instead of reconciling individual events (PR-006, PR-007). This is the
  documented initial approach; optimize only if latency becomes disruptive.
- Project metadata no longer contains `scale_aware`; no migration is provided
  because no pre-milestone project files need preserving.
- Manual TUI checks still apply for the visible mode labels, playhead-following
  cursor, track switching during live REC, command pause/resume, and `Esc`
  behavior; automated tests cover the equivalent state transitions.