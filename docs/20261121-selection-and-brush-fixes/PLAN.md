# Selection and Brush Fixes

## Purpose

This plan specifies the behavior and implementation milestones for improving mtrak's pattern-editor brush geometry and nibble/byte-aware selection behavior. It is based on the current implementation in `crates/mtrak-tui/src/app.rs`, `crates/mtrak-tui/src/key.rs`, and `crates/mtrak-tui/src/ui.rs`, and on the agreed interaction rules for editing, selection, and block operations.

## Goals

1. Make the brush represent the current editing cursor's complete affected rectangle.
2. Make selection growth use the configured brush dimensions while respecting byte and cell boundaries.
3. Support selection growth in both positive and negative horizontal and vertical directions.
4. Keep selections safe for byte-oriented copy/paste operations while preserving one-nibble direct editing.
5. Keep the logical cursor independent from brush-origin adjustment at pattern edges.
6. Make all relevant block operations visibly and semantically respect the selection.
7. Preserve the existing pattern-editor key bindings and provide regression coverage for the new behavior.

## Current implementation

The relevant state and behavior are concentrated in the TUI crate:

- `crates/mtrak-tui/src/app.rs`
  - `BrushWidth` and `BrushHeight` define brush presets.
  - `Selection` stores an anchor and head and normalizes them into a rectangle.
  - `affected_rows`, `affected_cols`, and `affected_rect` determine the region used by editing operations.
  - `extend_selection` currently advances the selection head by one nibble/row and updates the cursor.
  - `copy` uses a nibble clipboard, while clear/cut and insert/delete/backspace currently operate through touched event slots/tracks.
  - `after_pattern_change` clears selections after mutations.
- `crates/mtrak-tui/src/key.rs`
  - Ctrl+arrows resize the brush.
  - Shift+arrows extend selection.
  - Unmodified arrows currently move without clearing an existing selection.
- `crates/mtrak-tui/src/ui.rs`
  - Renders selection and affected-region highlighting at cell/track granularity.
- Existing unit tests in `crates/mtrak-tui/src/app.rs` cover current brush, selection, and block-operation behavior and should be extended rather than replaced.

## Agreed interaction model

### Brush dimensions

- Brush width is measured in nibbles and has these context-sensitive presets:
  - `1` nibble;
  - `2` nibbles (one byte);
  - `6` nibbles (one event cell when offsets are hidden);
  - `10` nibbles (one event plus its offset when offsets are visible);
  - full row.
- The width cycle omits `10` when offsets are hidden: `1 -> 2 -> 6 -> full`.
- If offsets are hidden while the brush is `10`, it becomes `6`.
- Brush height has these presets:
  - `1` row;
  - the current pattern LPB;
  - full pattern height.
- The width and height combine into the current editing rectangle.
- The brush must remain fully inside the pattern. Near the right or bottom edge, shift the brush origin as needed instead of clipping the brush. The logical cursor remains at its requested row/track/nibble.
- Offset nibbles participate in horizontal geometry when offsets are visible.

### Cursor, brush, and selection

These are separate concepts:

- The **logical cursor** is always one nibble at one row and remains the location where navigation and direct nibble typing occur.
- The **brush** is the current multi-nibble/multi-row editing rectangle derived from the logical cursor. Its origin may shift at pattern edges without moving the logical cursor.
- The **selection** is anchored at a starting point and has a moving head. The logical cursor follows the selection head, while the anchor remains fixed.

### Selection creation and growth

- The user changes brush size first, then starts a selection with Shift+Arrow.
- The brush dimensions determine the initial selection and the growth increments.
- Positive and negative directions are supported.
- Selection bounds used for byte-oriented operations are always rounded outward to complete-byte boundaries; no operation may expose or copy a sub-byte region.
- A one-nibble brush remains valid for direct nibble editing, but its selection operation rectangle is at least one complete byte wide.
- Horizontal growth is byte-aligned within the current event cell. Once growth leaves the current cell, subsequent growth uses whole cell-width steps. The cell width is six nibbles without offsets and ten nibbles with offsets.
- Vertical growth always uses the original brush height. It does not switch to a cell/LPB step after crossing a row or pattern boundary.
- The logical cursor follows the far edge/head reached by the latest Shift+Arrow. The visible operation selection is rounded to bytes, so the cursor and selection may have different nibble precision.
- Reversing direction shrinks the selection toward its anchor. Once the head reaches the anchor, continuing in the opposite direction grows a selection on that side. The selection remains active until explicitly canceled or replaced by an unmodified action.
- The behavior must reproduce these byte-level examples for a `1x1` brush, where each pair is one byte and the space separates event cells:
  - starting on the first nibble of `AA`: `AA` -> `AABB` -> `AABBCC` -> `AABBCC DDEEFF`;
  - starting on the first nibble of `BB`: `BB` -> `BBCC` -> `AABBCC DDEEFF`;
  - starting on the second nibble of `BB`: `BBCC` -> `AABBCC DDEEFF`.
- At pattern boundaries, selection rectangles are constrained to valid rows and nibble columns. Brush-origin adjustment must not move the logical cursor.

### Selection lifecycle and navigation

- Shift+Arrow starts or extends a selection.
- Every unmodified pattern-navigation action clears the active selection before moving normally:
  - Left, Right, Up, Down;
  - PageUp, PageDown;
  - Home, End;
  - TopLeft, BottomRight;
  - NextTrack and PrevTrack.
- Changing brush width or height cancels the active selection; brush changes are intended to happen before selection.
- Escape cancels a selection without mutating data.
- Existing key bindings remain unchanged and this behavior applies only to the pattern workspace.

### Block-operation semantics

All operations use the current selection rather than the entire touched event cell, except for the explicitly cell-oriented row-shifting operations. When no selection is active, commands derive their input region from the brush:

- **Copy, Cut, and Clear** round the brush rectangle outward to complete-byte boundaries. With a `1x1` brush, either nibble of `AA` therefore operates on `AA`.
- **Copy, Paste, Cut, and Clear** operate on the exact byte-aligned selection rectangle when a selection is active. Direct nibble editing remains independent of selection.
- If a byte-precise operation intersects a non-inline event such as SysEx, meta, or wire data, it is rejected with a clear message rather than silently corrupting or removing opaque event data. Whole-cell Insert/Delete/Backspace may still move such cells.
- **Paste without a selection** retains the current exact logical-cursor origin behavior and is not constrained by the brush rectangle.
- **Paste with a selection** uses the selection's top-left as its origin, writes the clipboard at its native dimensions, and clips writes to the selected rectangle. It does not stretch or repeat the clipboard. Paste is mutating and clears the selection afterward.
- Clipboard data maps structurally across offset-visibility modes: event bytes always map to event-byte columns, copied offset bytes map to offset columns, and hidden offsets do not redefine track/cell boundaries. Incompatible or unavailable destination portions are clipped, so copying with offsets hidden and pasting with offsets visible cannot spill event data into offset columns.
- **Copy** preserves the selection.
- **Cut** removes the exact selected bytes and then clears the selection. If the selection covers a complete cell in the current geometry, the cell is emptied normally. If it covers only part of a cell, selected data/argument bytes become explicit `00` values while unselected bytes are preserved; selecting the status/type byte empties the event cell.
- **Clear** follows the same complete-cell versus partial-cell rule as Cut.
- Partial Clear/Cut writes explicit `00` values into selected data/argument bytes; it does not create persistent undefined event data. Clearing a status/type byte empties the affected event cell.
- Clipboard blanks inside the copied rectangle are meaningful zero content: pasting them writes explicit `00` values. They are distinct from coordinates outside the clipboard or outside a clipped paste, which are left unchanged.
- **Insert, Delete, and Backspace** are cell-wide row-shifting operations. With an active selection, they expand the visible selection to whole event-cell/track units on behalf of the user. With no selection, they expand the brush rectangle to the containing whole event cells. The selection visibly expands when one exists, and the expanded rectangle is then used for the operation. This preserves the conventional cell-wide behavior expected for row shifts. These operations leave the expanded selection active after execution so the user can see what was operated on.
- Other mutating operations leave the cursor at the resulting position according to the existing operation-specific semantics unless new tests require a correction.

### UI rendering

- Selection highlighting shows the exact byte-aligned selection rather than every complete event cell touched by it.
- The brush highlights its exact nibble/row rectangle.
- The logical cursor is rendered as a one-nibble marker and always remains visible on top of brush and selection styling.
- Brush and selection backgrounds are composited so their overlap remains distinguishable and the user can see the cursor inside the current selection.
- Offset-visible rendering uses the ten-nibble cell geometry.

## Proposed implementation approach

1. Separate logical cursor coordinates from affected brush-origin coordinates in the geometry helpers. Add reusable rectangle calculations that shift the origin backward at right/bottom boundaries while preserving cursor state.
2. Make width presets derive from the current cell geometry: six event nibbles without offsets, ten total nibbles with offsets, full-row behavior, and conversion from `10` to `6` when offsets are hidden.
3. Introduce explicit selection geometry/state sufficient to retain the anchor, logical head, direction, brush dimensions at selection start, and byte-aligned operation bounds. Keep logical head precision separate from the operation rectangle so reversing and cursor rendering remain correct.
4. Implement deterministic horizontal growth: use byte-aligned increments while the active growth remains within the anchor cell, then use whole cell-width increments after crossing the cell boundary. Implement vertical growth using the original brush height throughout. Handle both directions and reversal through the anchor.
5. Add byte-alignment helpers for selection bounds. Ensure copy, paste, cut, and clear preserve unselected bytes in partially selected event cells.
6. Rework the nibble clipboard path so exact byte-aligned selection rectangles can be copied and pasted without implicitly expanding to complete event slots. Represent blank positions inside the copied rectangle as explicit zero values on paste, while distinguishing them from coordinates outside the clipboard or outside a clipped paste. Store clipboard geometry structurally enough to map event bytes and offsets across visibility modes without allowing event data to spill into offset columns. For partial Clear/Cut, write zeroes into selected data/argument bytes, empty a cell when its status/type byte is selected, and keep the resulting composition canonical and serializable. For paste into a selection, clip writes to the selected rectangle and clear the selection after mutation.
7. Add an explicit cell-wide expansion step for Insert, Delete, and Backspace. Update the visible selection before the row shift and retain the existing row-shifting semantics for the expanded track range.
8. Update action dispatch so every unmodified pattern-navigation action and every brush resize clears selections before applying its normal behavior.
9. Update UI rendering to style exact selected byte ranges, exact brush ranges, and the one-nibble cursor in composited layers, including offset-visible geometry.
10. Preserve unrelated screens and existing operation behavior where it does not conflict with these rules.

## Verification strategy

Add unit tests in `crates/mtrak-tui/src/app.rs` (and key tests where useful) for:

- width presets with offsets hidden and visible, including `10 -> 6` when offsets are hidden;
- brush-origin shifting at right and bottom edges while the logical cursor stays fixed;
- logical cursor, brush rectangle, and selection rectangle remaining distinct;
- initial selections for each brush width/height;
- the documented horizontal examples from the first/second nibble of `AA` and `BB`;
- horizontal growth within a cell and whole-cell growth after crossing a cell boundary;
- horizontal growth from positions inside a byte and inside a cell;
- byte-aligned bounds and preservation of unselected bytes in partial-cell operations;
- rejection and messaging for byte-precise operations intersecting non-inline events;
- no-selection brush rounding for Copy, Cut, and Clear;
- no-selection cell expansion for Insert, Delete, and Backspace;
- partial Clear/Cut writing zeroes into selected data/argument bytes;
- blank clipboard positions pasting as zeroes while outside/clipped positions remain unchanged;
- structural clipboard mapping when offset visibility changes between Copy and Paste;
- clearing a status/type byte emptying its event cell;
- saved projects and standard exports remaining canonical and consistent after partial operations;
- vertical growth by one, LPB, and full-height brush sizes;
- positive and negative selection directions;
- reversal toward and through the anchor;
- clearing on every unmodified navigation action and on brush resizing;
- Escape, Copy, Paste, Cut, and Clear selection lifecycle;
- paste at selection top-left with native-size clipping to the selection;
- visible cell expansion for Insert, Delete, and Backspace;
- regression coverage for existing pattern block and clipboard behavior.

Run the existing Rust test suite and formatting/lint checks after implementation.

## Resolved review points

The plan now explicitly resolves the review questions about horizontal selection state, cell-boundary growth, operation granularity, paste into an active selection, offset-dependent width presets, composited UI rendering, navigation cancellation, and reversal through the selection anchor.
