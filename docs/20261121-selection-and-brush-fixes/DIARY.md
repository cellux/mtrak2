# Implementation Diary

## Purpose

This file is the append-only, timestamped log of completed implementation work. Earlier entries must never be edited. Pending work is maintained separately in a single `HANDOFF` section at the end of this file and must be updated before every context flush so another agent can continue reliably. The implementing agent will add diary entries and create the active handoff when implementation begins.

---

## 2026-11-21T15:10:00Z — Selection and brush fixes (S0–S5) complete

Implemented the full selection-and-brush-fixes milestone in `mtrak-tui`.
Decisions SB-001…SB-006 were recorded before/while coding.

- **S0 — Brush geometry.** Added `BrushWidth::Ten`, made the width cycle
  offset-aware (`1 -> 2 -> 6 -> full` hidden, `1 -> 2 -> 6 -> 10 -> full`
  visible), and converted `Ten -> Six` when offsets are hidden. Added
  `brush_rect()`, which derives the inclusive brush rectangle from the logical
  cursor and shifts the origin backward at the right/bottom edges so the brush
  stays inside the pattern without moving the cursor.
- **S1 — Selection state and growth.** Replaced the two-point `Selection` with
  explicit geometry (anchor, head, bounds, captured brush dimensions, cell
  width, per-axis direction). `create_selection` builds the initial byte-aligned
  bounds (a one-nibble brush on the second nibble of a byte includes the next
  byte, matching the documented examples). `grow_horizontal` grows by one byte
  inside the anchor cell and by whole cells beyond it; `grow_vertical` grows by
  the captured brush height. `shrink_horizontal`/`shrink_vertical` implement
  reversal toward the anchor. Every unmodified navigation action and every
  brush resize now clears the active selection.
- **S2 — Byte-aligned copy/cut/clear.** Added `operation_rect()` (selection
  bounds, or the brush rounded outward to bytes) and `clear_rect()`, which
  empties complete cells and zeroes only the selected data/argument nibbles in
  partial cells, emptying the cell when the status/type byte is selected.
  Copy/Cut/Clear/Paste reject rectangles that intersect non-inline events with a
  clear message.
- **S3 — Structural clipboard.** `NibbleClipboard` now stores its source cell
  width, origin cell, and origin nibble, and exposes `mapped_columns`, which
  maps event nibbles to event columns and offset nibbles to offset columns
  (skipping offsets when the destination hides them). In-rectangle blanks paste
  as explicit zeroes. Paste with a selection uses the selection top-left as its
  origin and clips to the selection without stretching; paste without a
  selection keeps the logical-cursor origin.
- **S4 — Cell-wide row shifts.** Insert/Delete/Backspace expand an active
  selection (or the brush) to whole event cells, update the visible selection,
  and leave it active after the shift.
- **S5 — Rendering and coverage.** `ui.rs` now styles the exact byte-aligned
  selection, the exact nibble/row brush rectangle, and the one-nibble cursor in
  composited layers using `Rect::contains`, including the ten-nibble
  offset-visible geometry.

Verification: `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test --workspace` passes (88 `mtrak-tui` unit tests, including 27 new
selection/brush/clipboard tests, plus the existing core/engine/midi-io/e2e
suites).

## HANDOFF

All phases S0–S5 of `docs/20261121-selection-and-brush-fixes/PLAN.md` are
implemented and verified. No pending work remains for this milestone. The
worktree contains only the implementation and the updated `docs/` files; the
`docs/20261121-bugs/` folder is a separate, unrelated milestone.
