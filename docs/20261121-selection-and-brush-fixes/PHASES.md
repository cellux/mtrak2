# Implementation Phases

## Purpose

This file turns `PLAN.md`, the milestone specification, into an ordered set of
implementable and independently testable phases. Each completed phase is final
and should not be rewritten; implementation progress belongs in `DIARY.md`,
while design decisions that remain in force across phases belong in
`DECISIONS.md`.

The phases below implement the selection and brush fixes on top of the
`mtrak-tui` pattern editor. They assume the geometry lives in
`crates/mtrak-tui/src/app.rs`, the clipboard in
`crates/mtrak-tui/src/clipboard.rs`, the edit buffers in
`crates/mtrak-tui/src/edit.rs`, and the rendering in
`crates/mtrak-tui/src/ui.rs`.

## Phase S0 — Brush geometry with edge shifting and offset-aware presets

Dependencies: none.

Work:

- Add the offset-aware `10`-nibble brush-width preset and make the width cycle
  depend on offset visibility (`1 -> 2 -> 6 -> full` when offsets are hidden,
  `1 -> 2 -> 6 -> 10 -> full` when they are visible).
- Convert an active `10`-nibble brush to `6` when offsets are hidden.
- Introduce a `brush_rect()` geometry helper that derives the inclusive
  nibble/row rectangle from the logical cursor and the current brush
  dimensions, shifting the origin backward at the right and bottom pattern
  edges instead of clipping the brush, while leaving the logical cursor
  untouched.
- Keep the existing `affected_rows`/`affected_cols`/`affected_rect` helpers as
  thin compatibility shims over the new geometry.

Validation: unit tests for the width presets with offsets hidden/visible,
`10 -> 6` conversion, right/bottom edge shifting with a fixed cursor, and the
brush rectangle staying inside the pattern.

## Phase S1 — Selection state and deterministic growth

Dependencies: S0.

Work:

- Replace the two-point `Selection` with explicit geometry that retains the
  anchor, the logical head, the per-axis growth direction, the brush dimensions
  captured at selection start, the cell width, and the byte-aligned operation
  bounds.
- Implement horizontal growth as byte-aligned increments inside the anchor
  event cell and whole-cell increments once growth leaves that cell, including
  both directions and reversal through the anchor.
- Implement vertical growth using the captured brush height for every step.
- Derive the operation bounds from the anchor and head, snapping to whole cells
  when the head leaves the anchor cell and rounding the horizontal edges
  outward to complete bytes.
- Move the logical cursor to the far head reached by each Shift+Arrow.
- Clear the active selection before every unmodified navigation action
  (arrows, PageUp/PageDown, Home/End, TopLeft/BottomRight, NextTrack/PrevTrack)
  and on every brush resize.

Validation: unit tests for the initial selection of each brush size, the
documented `AA`/`BB` horizontal examples, in-cell and whole-cell growth,
positive/negative directions, reversal through the anchor, vertical growth by
one/LPB/full height, and selection clearing on navigation and brush changes.

## Phase S2 — Byte-aligned copy, cut, and clear

Dependencies: S1.

Work:

- Round the no-selection brush rectangle outward to complete-byte boundaries for
  Copy, Cut, and Clear.
- Use the exact byte-aligned selection bounds when a selection is active.
- Reject byte-precise Copy, Cut, Clear, and Paste operations that intersect a
  non-inline event (`SysEx`, meta, SMF Escape, or wire data) with a clear
  status message instead of silently corrupting the cell.
- Implement the complete-cell versus partial-cell rule for Clear and Cut:
  complete cells empty normally; partial cells zero only the selected
  data/argument bytes; selecting the status/type byte empties the event cell.
- Keep Copy from clearing the selection; clear the selection after Cut.

Validation: unit tests for no-selection byte rounding, byte-aligned selection
operations, unselected-byte preservation, zeroing of selected data bytes,
status-byte emptying, non-inline rejection, and the Cut/Copy lifecycle.

## Phase S3 — Structural clipboard and paste into a selection

Dependencies: S2.

Work:

- Treat blanks inside a copied rectangle as meaningful zero content on paste,
  while leaving coordinates outside the clipboard (or outside a clipped paste)
  unchanged.
- Store the clipboard with enough structure (source cell width, offset
  inclusion, and per-column values) to map event bytes to event-byte columns
  and offset bytes to offset columns across offset-visibility changes.
- Clip, rather than stretch or repeat, the clipboard when it does not fit.
- Paste at the selection's top-left when a selection is active, clipping writes
  to the selected rectangle and clearing the selection afterward; keep the
  existing exact logical-cursor origin for paste without a selection.

Validation: unit tests for blank-as-zero paste, offset-visibility mapping in
both directions, native-size clipping, selection-origin paste, and the
no-selection cursor-origin paste.

## Phase S4 — Cell-wide row-shift expansion

Dependencies: S1.

Work:

- Add an explicit cell-wide expansion step for Insert, Delete, and Backspace
  that expands an active selection (or, without one, the brush rectangle) to
  whole event-cell/track units before the row shift.
- Make the expanded rectangle visible by updating the stored selection before
  the operation and leaving it active afterward.

Validation: unit tests for expanded Insert/Delete/Backspace with and without a
selection, and for the visible selection remaining active after the shift.

## Phase S5 — Composited rendering and regression coverage

Dependencies: S0–S4.

Work:

- Render the exact byte-aligned selection, the exact nibble/row brush
  rectangle, and the one-nibble logical cursor in composited layers, including
  the ten-nibble offset-visible geometry, so the cursor remains visible on top
  of brush and selection styling.
- Add regression coverage for the existing pattern block and clipboard
  behavior, and run the formatting, lint, and full workspace test gates.

Validation: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` all pass.
