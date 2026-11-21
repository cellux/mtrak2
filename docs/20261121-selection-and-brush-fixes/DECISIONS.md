# Design Decisions

## Purpose

This file records durable design decisions that shape implementation across
multiple phases. Decisions are separate from the ordered work in `PHASES.md`
and the progress history in `DIARY.md`. Decisions are never deleted: when an
answer changes, the old decision is marked `superseded by Dn` and a new
decision is added. The implementing agent will add decisions as they are
resolved during analysis and implementation.

---

### SB-001 — The brush, the logical cursor, and the selection are separate

- Status: Accepted
- Decision: the logical cursor is one nibble at one row and is the only place
  direct typing and navigation act. The brush is a multi-nibble/multi-row
  rectangle derived from the cursor whose origin shifts backward at the right
  and bottom pattern edges without moving the cursor. The selection is an
  anchored range whose logical head follows the cursor and whose operation
  bounds are byte-aligned.
- Consequence: geometry helpers return the brush rectangle and the selection
  bounds independently; the cursor is never clipped or moved to keep the brush
  inside the pattern.
- Source: `PLAN.md` "Cursor, brush, and selection".

### SB-002 — The brush width preset set is offset-dependent

- Status: Accepted
- Decision: brush widths are `1`, `2`, `6`, `10` (offsets visible only), and
  full-row. The cycle omits `10` when offsets are hidden, and hiding offsets
  while the brush is `10` converts it to `6`. A cell is six event nibbles
  without offsets and ten total nibbles with offsets.
- Consequence: `BrushWidth` gains a `Ten` variant and its grow/shrink cycle is
  parameterized by offset visibility.
- Source: `PLAN.md` "Brush dimensions".

### SB-003 — Selection growth is byte-aligned in-cell and cell-aligned beyond

- Status: Accepted
- Decision: the first Shift+Arrow creates the selection as the byte-rounded
  brush rectangle (a one-nibble brush on the second nibble of a byte includes
  the following byte). Later presses grow the extended side by one byte while
  it stays in the anchor cell and by whole event cells once it leaves that
  cell; vertical growth always advances by the captured brush height. Reversing
  a direction first shrinks the extended side toward the anchor and only starts
  growing on the opposite side on a later press after reaching the anchor. The
  logical cursor follows the far edge reached by each press.
- Consequence: the documented `1x1` examples are reproduced exactly, including
  the second-nibble `BB` case that starts at `BBCC`. A one-nibble brush still
  selects at least one complete byte.
- Source: `PLAN.md` "Selection creation and growth".

### SB-004 — Byte-precise operations reject opaque events

- Status: Accepted
- Decision: Copy, Cut, Clear, and Paste refuse to operate on a rectangle that
  intersects a non-inline event (`SysEx`, meta, SMF Escape, or wire data) and
  report a clear message. Whole-cell Insert/Delete/Backspace still move such
  cells because they never reinterpret event bytes.
- Consequence: opaque payloads can no longer be silently truncated by a
  byte-precise block operation.
- Source: `PLAN.md` "Block-operation semantics".

### SB-005 — The clipboard is structural and blanks are meaningful

- Status: Accepted
- Decision: the clipboard stores source cell width, offset inclusion, and a
  dense rectangle of nibble values. Blank positions inside the rectangle paste
  as explicit zero nibbles; coordinates outside the rectangle, or outside a
  clipped paste, are left unchanged. On paste, event nibbles map to event
  columns and offset nibbles map to offset columns, so a copy with offsets
  hidden and a paste with offsets visible never spills event data into offset
  columns. Paste uses the selection top-left as its origin and clips to the
  selection rectangle without stretching or repeating.
- Consequence: `NibbleClipboard` no longer treats `None` as "skip" for
  in-rectangle positions, and paste performs a structural column mapping
  instead of a linear one.
- Source: `PLAN.md` "Block-operation semantics".

### SB-006 — Row shifts expand to whole cells visibly

- Status: Accepted
- Decision: Insert, Delete, and Backspace first expand an active selection (or,
  without one, the brush rectangle) to whole event-cell/track units. When a
  selection exists, the stored selection is updated to the expanded rectangle
  and remains active after the shift so the user can see the operated region.
- Consequence: the conventional cell-wide row-shift behavior is preserved even
  with a sub-cell selection.
- Source: `PLAN.md` "Block-operation semantics".
