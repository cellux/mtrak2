# mtrak v1 remaining gaps implementation plan

## Purpose

This document specifies the work needed to close the remaining gaps between the
current mtrak implementation and the desired v1 workflow, together with the
new editing and Lua-authoring workflow agreed during design exploration. It is
the milestone specification for the implementation. Ordered implementation
phases, durable design decisions, and completed-work history belong in
`PHASES.md`, `DECISIONS.md`, and `DIARY.md` respectively.

## 1. Goals

Implement a coherent tracker/editor workflow in which:

- inline MIDI and macro bytes can be edited directly at nibble granularity;
- complex events can be edited through a command-line event editor;
- intra-row offsets are visible and editable without obscuring ordinary event
  editing;
- Pattern and Code are distinct full-screen workspaces;
- patterns and Lua modules can be selected, created, copied, cut, pasted, and
  edited through keyboard-driven chooser workflows;
- Lua modules have a complete text-editing workflow with simple syntax
  highlighting and per-module undo;
- built-in macros are inspectable Lua modules and reusable through `require`;
- global note-entry settings remain available, while pattern-local settings are
  removed;
- valid MIDI data remains representable without an undifferentiated raw-event
  escape hatch, while malformed input is rejected with a user-visible error;
- all destructive/editor operations are deterministic, testable, and saved as
  part of the composition/project.

The implementation must preserve the existing crate boundaries and remain
hardware-independent in tests.

## 2. Scope and current-model changes

### 2.1 Event representation

Retain typed representations for:

- channel-voice messages;
- sysex events;
- MIDI-file meta-events;
- valid Standard MIDI File Escape events;
- tracker macro calls.

Replace `Event::Raw` with a generic valid wire-MIDI byte-sequence event for
valid live-MIDI forms that do not have a specialized typed representation or
that are too long/variable to be shown inline. The generic event stores wire
bytes only; it has no SMF/wire encoding tag. File-only events remain typed
`MetaEvent` values, including typed unknown-meta variants, and a dedicated
`SmfEscape(Vec<u8>)` event for valid Standard MIDI File Escape events. The SMF
writer serializes wire events into the appropriate file representation. The
generic event must be validated when created or imported; malformed data is not
stored.

Generic wire events that have no faithful SMF representation may be used for
live playback but cause tracker save or standard export to fail with a clear
location-specific error rather than being silently encoded with different
semantics.

The generic byte-sequence event is displayed with the complex-event marker and
is edited through `:event`. It is not a way to preserve malformed input.

A valid three-byte value whose first byte is below `0x80` is a macro call. Valid
MIDI event/status bytes have the high bit set, so ordinary valid MIDI bytes and
macro calls remain distinguishable. The `:event` parser must apply this rule.

`:event` uses wire-MIDI bytes for live MIDI events, including complete `F0`/`F7`
framing for sysex. File-only meta-events use their standard SMF event encoding,
since they have no wire-MIDI equivalent. The parser and serializer must make
this distinction explicit and round-trip each supported form.

### 2.2 Global settings only

Remove pattern-local root, scale, modal-degree, and scale-aware settings from
the model, engine/macro context, user-facing commands, Lua setting APIs, and
project serialization. Retain only composition-global settings used by note
entry, display, and macros. Existing `:root`, `:scale`, and `:mode` commands
continue to modify global settings. Remove the local-resolution layer and
simplify `Composition::effective_settings` and related APIs accordingly.

### 2.3 Import validation

Import is strict. If the SMF container, track framing, or any event encoding is
malformed, loading rejects the entire file and displays an error in the bottom
status line. It must not partially import a file whose structure cannot be
trusted.

Valid MIDI events that lack a specialized mtrak type are represented as generic
valid wire-MIDI byte-sequence events and are not discarded merely because mtrak
has no specialized typed editor for them.

## 3. Pattern-grid event editing

### 3.1 Nibble cursor

Inline cells expose six hexadecimal nibble positions. The normal grid cursor
has both a cell coordinate and a nibble position:

- Left/Right move between nibbles;
- Up/Down move between rows while retaining the nibble position;
- Right from nibble 6 moves to nibble 1 of the next track;
- Left from nibble 1 moves to nibble 6 of the previous track;
- when offsets are visible, the cursor skips the literal `+` separator.

Direct hexadecimal typing replaces the selected nibble immediately and advances
the nibble cursor.

Direct nibble editing applies to empty cells and inline events. Complex or
variable-length events remain marker cells and are edited via `:event`.

- `.` clears every whole event slot touched by the selected nibble range, or
  by the current brush range when no selection exists, as one undoable
  operation.
- Grid `Ctrl-X` copies the exact selected nibble range and then clears every
  whole event slot touched by that range.
- `Esc` cancels the current selection without mutating cell contents.

### 3.2 Nibble-level block editing

Horizontal brush width, selection, clipboard, and clear operations address
hexadecimal nibbles rather than only whole event cells. Insert, Delete, and
Backspace remain whole-slot structural operations.
A horizontal selection has a row range and a nibble-column range across the
track grid; six event nibbles belong to each normal cell, and visible offset
nibbles participate in the same horizontal address space while the literal `+`
is not selectable. Brush widths are measured in nibbles and cycle through `1`,
`2`, `6`, and the full displayed pattern width. Copy/cut/paste preserve the
exact nibble alignment, even
when the origin or destination is inside an event. The clipboard therefore
stores a rectangular nibble matrix plus whether offset nibbles are present,
rather than only cloning whole `Slot` values.

Nibble-level copy/paste uses the same temporary-buffer and normalization
rules as direct editing. When offsets are hidden, copy/paste affects event
nibbles only and leaves destination offsets unchanged. When offsets are visible,
the selected offset nibbles are included and replace the corresponding
destination offset nibbles. Insert/Delete/Backspace remain structural row-shift
operations over whole event slots touched by the nibble range and never shift
nibbles across cell boundaries. `.` clears every whole event slot touched by the
nibble selection
or brush; it never creates a partially cleared event.

### 3.3 Temporary edit buffers

Each actively edited cell has a six-nibble event buffer and, when offset
visibility is enabled, a four-nibble offset buffer. These buffers are the
source of truth while they contain blanks or an intermediate value that cannot
yet be represented as a valid event.

- Empty-cell buffers begin visually blank.
- Existing inline events populate the buffer from their event bytes, leaving
  non-applicable positions blank where appropriate.
- After each keystroke, conversion to a valid typed event is attempted and the
  underlying slot is updated when possible.
- Blank positions remain visible during editing.
- When the user leaves the cell, remaining event blanks are filled with zeroes.
- If the zero-filled six-digit value is valid, the final event is committed.
- If normalization produces an invalid/reserved value, the slot is cleared to
  empty rather than inventing an event.
- One contiguous editing session for one cell is one undo operation.
- Invalid/incomplete intermediate input is retained in the editor buffer rather
  than corrupting the committed composition.

The status byte determines how many data bytes are semantically applicable.
For one-data-byte channel messages such as Program Change and Channel Pressure,
the fifth and sixth grid nibbles are preserved only in the active edit buffer
but ignored when converting the event. If the status byte changes to a
two-data-byte message during that same editing session, those preserved nibbles
become active data. Once the session ends, ignored nibbles are not serialized
or restored. Explicit zero values must remain distinct from blank/omitted
positions.

### 3.4 `:event` command editor

Replace the current standalone modal `EventEditor` workflow with command mode.
Pressing `Enter` on a non-empty current cell opens command mode with a
pre-populated command such as:

```text
:event 90643F
```

The command cursor is positioned on the first event nibble. Command mode
supports standard line editing: Left/Right and Home/End cursor movement,
insertion at the cursor, Backspace/Delete at the cursor, and the existing
Enter/Esc behavior. The user may edit the command using this line editor. The
`:event` payload accepts
hexadecimal bytes separated by whitespace, for example:

```text
:event 90 64 3F
:event F0 7D 01 02 F7
:event FF 51 03 07 A1 20
```

Channel and system/sysex events use wire-MIDI byte representation. Meta-events
and SMF Escape events use their SMF event representation.

Only whitespace is accepted as a separator. On Enter:

- valid channel, sysex, meta, SMF Escape, macro, or generic valid wire-MIDI
data is parsed;
- exactly three bytes beginning below `0x80` become a macro call;
- other valid MIDI status forms become their typed or generic event form;
- invalid, empty, odd-nibble, or malformed input is rejected;
- the slot remains unchanged on rejection;
- command mode remains open;
- the command is rendered in red/failed state so the user can continue editing;
- Esc abandons the command without changing the slot.

A successful command commits one cell-level undo operation and returns to the
previous workspace/mode. Pressing `Enter` on an empty cell does nothing; to
populate an empty cell with a full byte sequence, the user enters `:event`
manually while the cursor is on that cell.

## 4. Offset editing

### 4.1 Representation

A slot offset is an integer number of global ticks from the start of its row:

```text
absolute_tick = row_start_tick + offset
```

It is not a percentage or a power-of-two subdivision. Valid offsets satisfy:

```text
0 <= offset < TPB / LPB
```

Display offsets as four fixed-width hexadecimal digits. In offset-visible
mode, a cell is rendered as:

```text
XXYYZZ+AABB
```

including `+0000` for zero offset.

### 4.2 Visibility

Lowercase `o` toggles offset visibility for the Pattern screen as a single
screen-wide layout. Visibility is UI-only and is reset for a newly selected
pattern. Toggling visibility cancels the current nibble selection and resets
the nibble cursor to the current event cell. The offset column is not
serialized.

When hidden, the cursor exposes six event nibbles. When visible, it exposes
six event nibbles followed by four offset nibbles, skipping `+`.

### 4.3 Validation

While editing an offset:

- valid values update the slot immediately;
- values outside the current row’s valid range are retained in the temporary
  cell buffer and displayed in red;
- leaving the cell clamps an invalid offset to the final valid tick in the row;
- the final clamped value is committed as part of the cell-level undo session.

## 5. Workspaces and navigation

Add three top-level screens:

- `F1` — Help;
- `F2` — Pattern;
- `F3` — Code.

The Help screen is intentionally empty in this milestone. It is a placeholder
for future built-in documentation.

Pattern and Code screens show a top status row containing respectively:

```text
PAT XX
MOD XX
```

alongside the relevant global status information.

Screen switching first finalizes the active Pattern cell using the normal
cell-leave behavior: event blanks are zero-filled, invalid normalized events
are cleared, and invalid offsets are clamped. It then preserves each
workspace’s state without discarding committed edits. Command mode and chooser
mode remain modal over the active workspace.

## 6. Pattern/module chooser

On the Pattern and Code screens, `Ctrl-Enter` opens a centered 16×16 chooser.
The chooser represents IDs `00`–`FF` and supports:

- cursor-key navigation;
- typing hexadecimal digits such as `4f` to jump directly to cell `4F`;
- Enter to accept the highlighted cell;
- Esc to cancel without changing the current item;
- visual indication of filled/defined cells.

### 6.1 Pattern chooser

An empty pattern cell is created automatically with the existing defaults:
LPB `4`, 64 rows, and 16 tracks. The new pattern receives the selected stable
ID and becomes current. The main pattern remains protected from cut/delete.

### 6.2 Code chooser

The Code chooser shows all IDs, with ranges/status distinguished:

- `00`: editable project/library module, always initialized in every newly
  created or imported composition from the minimal template;
- `01`–`3F`: built-in modules, read-only;
- `40`–`7F`: user modules, lazily created on first use from the minimal Lua
  template;
- `80`–`FF`: reserved/unavailable.

User modules in empty `40`–`7F` cells are created on selection. Module `00`
is always present and may then be extended as a library/setup module. Undefined
or reserved built-in-range cells remain unavailable unless a built-in exists.

### 6.3 Clipboard operations

Chooser clipboard operations are type-specific:

- `Ctrl-C` copies the selected pattern/module;
- `Ctrl-X` copies and clears it;
- `Ctrl-V` replaces the selected destination while preserving the destination
  ID;
- patterns paste only to patterns;
- modules paste only to modules;
- built-in modules and the protected main pattern cannot be cut or overwritten.

Pattern/module copying and cut/paste are undoable in the corresponding
Pattern-chooser or Code-chooser structural history.

## 7. Code screen and Lua workflow

### 7.1 Text editor

The Code screen is a complete multiline Lua editor with simple lexical syntax
highlighting. It supports:

- arrow-key cursor movement;
- printable character insertion;
- Enter for new lines;
- Backspace/Delete;
- Tab indentation;
- viewport scrolling;
- `Ctrl-Z`/`Ctrl-R` undo/redo;
- module selection through the chooser;
- `F5` to explicitly validate/reload the current project Lua runtime;
- automatic persistence in the tracker project save format.

Lua source edits are not reloaded on every keystroke. Playback, rendering, and
export lazily validate/reload the runtime immediately before use, and `F5`
provides an explicit reload path while remaining in Code mode. Invalid source
remains editable and produces a clear diagnostic rather than being discarded.
If validation fails, playback/render/export is rejected and the last
successfully loaded runtime remains installed; stale behavior is never used
silently for a requested operation.

The highlighter need only cover practical lexical categories such as keywords,
function names, numbers, strings, comments, and punctuation. Full semantic
analysis is not required.

### 7.2 Module source rules

Built-in macros must be migrated from Rust implementations to Lua modules while
preserving their existing IDs and behavior, initially including:

- `01`: pattern trigger using argument 1;
- `02`: Q8.8 tempo shorthand.

Their embedded Lua source is inspectable but read-only. Module `00` is editable
and acts as a project/library module. User modules `40`–`7F` are initialized on
first use from this minimal valid module template:

```lua
local M = {}

function M.run(arg1, arg2, ctx)
end

return M
```

The template exposes the required `run` entry point while returning a module
table for `require`. A module used as a macro must expose a callable `run`
field; modules used only as libraries may return any value.

Module `00` is initialized in every new composition and its setup side effects
run once when the project Lua runtime is built, before normal module execution.
Its returned value is also available through `require(0x00)`. Project storage
should keep module `00` in the same ID-keyed module-source map as user modules
and remove
the separate `setup_source` field.

Every Lua module may return any Lua value. `require(0xNN)` returns exactly the
cached value returned by that module; it is not restricted to tables. Module
`00` may therefore return a library value while also performing setup side
effects such as registering project-local TUI commands. Those commands and
their handlers remain TUI-only runtime state; the module source is persisted,
but registered command state is not exported as standalone MIDI data.

Modules may import other modules with numeric hexadecimal IDs:

```lua
local util = require(0x00)
```

`require` supports built-in and user modules, caches loaded modules, and reports
circular dependencies or unavailable IDs as deterministic diagnostics. The
sandbox restrictions and resource limits remain in force.

## 8. Undo model

Use separate content-edit histories per stable pattern ID and per stable module
ID. Pattern and Code histories are independent. Switching items preserves that
item’s history. Deleting or replacing an item discards its associated content
history.

The Pattern chooser and Code chooser each have a separate structural undo stack.
While a chooser is open, `Ctrl-Z`/`Ctrl-R` undo and redo chooser operations such
as creation, deletion, cut, and paste. A chooser paste may replace the
(destination) item and discard that item’s content history; the chooser
structural history restores the prior destination state if undone. The source
item’s content history is unchanged by copy/paste and records the clear caused
by a true cut.

A single cell’s contiguous nibble/offset edit is one undo action. Text editing
operations in a module use the module’s own history. Chooser operations must
never accidentally undo an unrelated item or cross between Pattern and Code
structural histories.

## 9. Commands and compatibility

Retain existing commands for global settings, file operations, pattern geometry,
MIDI ports, scale registry, and project save/export. Extend command parsing with
`:event` as specified above. Remove user-facing pattern-local settings commands
and any serialization of pattern-local settings.

No prior tracker project files need migration because the current project format
has not been released. The new serialization format should simply omit
pattern-local settings.

## 10. Testing and acceptance criteria

Add unit and integration tests covering at least:

- nibble cursor movement, including track wrapping and offset `+` skipping;
- empty-cell buffers, blank-to-zero finalization, immediate valid-event updates,
  and cell-level undo;
- nibble-level selection, brush operations, clipboard alignment, whole-slot
  clearing, and non-splitting structural shifts;
- valid and invalid `:event` parsing, whitespace separators, macro detection,
  rejection without closing command mode, and red error state;
- `.` clearing and `Esc` selection cancellation with undo behavior;
- complex/generic event round-tripping through the command editor;
- offset range validation, red invalid display, leave-cell clamping, and
  four-digit formatting;
- screen-wide offset visibility and reset on pattern switch;
- F1/F2/F3 workspace switching and chooser keyboard behavior;
- chooser creation, filled-cell rendering, hexadecimal jumps, Esc cancellation,
  and protected/reserved IDs;
- pattern and module type-specific copy/cut/paste with destination IDs;
- per-pattern/per-module undo isolation and deletion/replacement cleanup;
- Lua text editing, syntax highlighting spans, lazy templates, built-in
  read-only behavior, `require`, dependency errors, and built-in macro behavior;
- strict import rejection for malformed SMF containers/events and a clear
  bottom-line error;
- valid generic MIDI byte-sequence preservation without `Event::Raw`;
- project save/load persistence of patterns, offsets, module source, and global
  settings;
- standard export without tracker-only metadata or project UI state.

The implementation is complete when these behaviors are available in the TUI,
all supported project data round-trips, and the full workspace test suite passes
without MIDI hardware.
