# PLAY/REC/EDIT mode revamp plan

## 1. Purpose

Revamp mtrak's pattern-screen interaction around three explicit musical modes:

- **PLAY**: play notes from the keyboard without changing pattern data.
- **REC**: play notes and record them into the selected track, supporting both
  stopped step recording and live recording while the pattern plays.
- **EDIT**: edit pattern cells nibble by nibble.

The implementation must preserve the existing tracker editing features, command
layer, undo/redo, MIDI output abstraction, and deterministic playback while
making the mode and cursor behavior explicit and testable.

## 2. Goals and non-goals

### Goals

1. Replace the current Normal/Note interaction with PLAY, REC, and EDIT modes
   on the Pattern screen.
2. Make PLAY the default mode for new and loaded application sessions.
3. Make the active mode visible in the TUI header.
4. Use the current scale for keyboard note mapping, with chromatic represented
   as an ordinary built-in scale rather than a separate chromatic toggle.
5. Support reliable step recording, live recording, track selection during
   recording, and recording-step configuration.
6. Keep playback, recording, and editing behavior testable without MIDI
   hardware.

### Non-goals

- MIDI input recording from an external device.
- Quantization during recording. Live events retain their captured offsets;
  selection-based quantization may be a later feature.
- A status-bar note notification. This was considered and explicitly dropped.
- Persisting the recording step in project files.

## 3. User interaction specification

### 3.1 Modes and switching

The Pattern screen has three musical modes:

- `Ctrl-P` enters PLAY.
- `Ctrl-R` enters REC.
- `Ctrl-E` enters EDIT.

Switching modes preserves the cursor coordinates and selected track. Any active
partial `CellEdit` is finalized using the existing blank-to-zero/finalization
rules before the mode changes. The cursor's row, track, and nibble are separate
from brush sizing and should not be rewritten merely because PLAY/REC uses
cell-wide horizontal navigation.

The current mode is displayed in the header. The default mode is PLAY.

Existing command and chooser modes remain transient UI modes layered over the
musical modes. Entering command mode remembers the prior musical mode; leaving
it restores that mode.

### 3.2 PLAY mode

- Keyboard notes use the composition's selected scale. Non-chromatic scales
  use the existing scale-degree keyboard mapping; the chromatic scale uses the
  existing piano layout (`z`, `s`, `x`, `d`, ...).
- A note sends a MIDI NoteOn immediately and does not mutate the pattern or
  advance the cursor.
- Channel and velocity are inherited independently from the selected track,
  starting at the current cell and scanning toward earlier rows:
  - channel: nearest preceding channel-voice event with a channel;
  - velocity: nearest preceding NoteOn/NoteOff event with a second data byte.
- If no value is found, channel defaults to `0` and velocity to `100`.
- PLAY has the same cell-wide horizontal track navigation as REC: Left/Right
  and Tab/Shift-Tab select the previous/next track. Other navigation retains
  its normal meaning when playback is stopped.
- Pattern editing is disabled in PLAY: no nibble entry, block operations,
  selections, clipboard operations, track insertion/deletion, or brush
  resizing. Undo and redo remain available.

### 3.3 REC mode

REC uses the same note mapping, immediate MIDI NoteOn behavior, inheritance
rules, and cell-wide horizontal track navigation as PLAY. Pattern editing is
disabled in REC as in PLAY, except that undo and redo remain available.

#### Stopped step recording

When playback is stopped:

- a keyboard note replaces the selected track's current cell;
- the event is stored with offset `0`;
- the cursor advances downward by the current recording step;
- a step of `0` leaves the cursor row unchanged;
- advancing past the final row clamps to the final row;
- the newly written event is available to inheritance lookup for subsequent
  notes.

#### Live recording

When playback is active:

- REC forces follow behavior and the logical cursor row follows the playhead;
- the selected track remains the recording destination while the row follows
  playback;
- Left/Right and Tab/Shift-Tab change only the selected track;
- each note replaces the selected track's cell at the current playhead row;
- the event's offset records the current position within that row;
- no quantization is applied;
- the note is sent immediately and is marked as already emitted when added to
  the running event stream, so it is not sent twice;
- if the replaced cell has a pending old event, that old event is removed from
  the pending stream;
- the new event is incorporated into the future playback stream.

### 3.4 EDIT mode

- EDIT retains nibble-by-nibble cell editing and existing event/offset edit
  buffers.
- EDIT does not use recording-step movement; its effective step is always `0`.
- EDIT always has follow behavior off.
- Normal nibble navigation, structural block operations, selections, brush
  sizing, event editing, undo/redo, and offset visibility remain available.

### 3.5 Follow and cursor behavior

The user-configurable follow feature is removed:

- remove `:follow` and `Ctrl-F`;
- PLAY and REC always follow playback;
- EDIT never follows playback.

In PLAY/REC during playback, the logical cursor row moves with the playhead;
the selected track and nibble coordinate remain user-controlled. Switching from
EDIT to PLAY or REC during playback therefore brings the cursor row to the
current playhead while preserving the selected track/nibble. Switching to EDIT
leaves the cursor at the current row and disables follow.

When playback stops, the cursor remains at the stopping playhead row. Playback
itself is not started or stopped by entering a musical mode.

### 3.6 Playback controls

- Space retains its existing toggle-playback behavior in all three musical
  modes.
- Uppercase `S` sets the configured playback start row in PLAY, REC, and EDIT.
  Lowercase `s` remains available to the existing chromatic piano keyboard
  layout.
- `Esc` stops active playback. If playback is not active, it retains its
  current context-sensitive behavior (selection cancellation, command cancel,
  chooser cancel, etc.). Stopping playback retains the existing note-release
  safety behavior.
- Switching modes does not otherwise alter playback.

### 3.7 Recording step

The recording step is an application-session setting measured in pattern rows:

- default: `1`;
- `Alt+0` through `Alt+9` set values `0` through `9`;
- `:step N` accepts any nonnegative integer `N`;
- the setting is not serialized in the project;
- the setting affects stopped REC step recording only. EDIT always behaves as
  step `0`, and PLAY never advances due to note audition.

### 3.8 Scale and note-entry settings

- Add `chromatic` as a built-in 12-note scale.
- Make chromatic the default scale for new projects.
- Remove the separate `scale_aware` setting and the `Ctrl-N`/`N` mode/toggle
  behavior.
- Use the existing scale-degree keyboard mapping for non-chromatic scales;
  use the existing chromatic piano layout when the selected scale is
  chromatic.
- No project-file migration is required because there are no existing project
  files to preserve.
- Preserve root, modal degree, built-in scale selection, and user-defined
  scale support through the existing settings and `:scale` command mechanisms.

### 3.9 Command-mode playback pause

Entering command mode while playback is active pauses playback and sends
All Notes Off. The pause preserves the exact playback tick, fractional timing
state, playhead row, selected track, and musical mode. On command completion or
cancellation, playback automatically resumes from that tick using the project
state after the command. If playback was not active on entry, command mode does
not start it.

## 4. Implementation approach

### 4.1 TUI state and key dispatch

- Replace or refactor `Mode::Normal` and `Mode::Note` into explicit musical
  modes while retaining transient Command and Chooser modes.
- Add session state for the recording step and for command-mode playback pause
  and resume.
- Centralize mode transitions so partial edit buffers are finalized exactly
  once and PLAY/REC follow/cursor rules are applied consistently.
- Update key dispatch for `Ctrl-P`, `Ctrl-R`, `Ctrl-E`, `Alt+digit`, `Esc`, and
  the moved Redo binding (`Ctrl-Y`). Remove obsolete note/chromatic and follow
  bindings.
- Resolve `Ctrl-R`'s former Redo binding by moving Redo to `Ctrl-Y`.
- Bind uppercase `S` as the start-row control in PLAY/REC/EDIT, leaving
  lowercase `s` available to the chromatic piano keyboard.

### 4.2 Note lookup, recording, and cell mutation

- Generalize the existing note-entry path into a PLAY/REC note handler.
- Implement independent backward inheritance lookup over the selected track.
- Keep step recording and live recording as separate paths so step events use
  offset `0` while live events use the playback row offset.
- Record each step-recorded or live-recorded note as one independent undo
  step, and ensure replacing a cell updates the dirty state and edit buffers
  correctly.
- Ensure cell-wide PLAY/REC navigation does not destroy the nibble coordinate
  used when returning to EDIT.

### 4.3 Playback and live event stream

- Expose or retain the current integer/fractional playback position needed to
  compute the current row offset.
- After a live pattern mutation, preserve the current playback tick, discard
  all not-yet-emitted events, and rebuild the complete future stream from the
  updated composition. This removes replaced direct events and any dependent
  macro-generated descendants without requiring initial event provenance.
- Mark the immediately auditioned event as already emitted so rebuilding does
  not send it twice.
- Define deterministic ordering for events at the same tick.
- Preserve command-mode pause state and rebuild the future stream after
  commands that change playback data or settings.
- Use full future-stream rebuilds initially; optimize only if measurement shows
  live-recording latency is disruptive.

### 4.4 Settings and serialization

- Replace the `scale_aware` representation with the selected `Scale`, adding
  the chromatic built-in and making it the new default.
- Update project metadata serialization/deserialization to omit the obsolete
  flag. No backward migration path is required because no existing project
  files exist.
- Keep recording step session-local and out of project metadata.
- Remove follow persistence/commands if any serialized or command-layer state
  exists.

### 4.5 Rendering and help text

- Display PLAY/REC/EDIT in the header.
- Render the cursor/playhead according to mode-specific follow behavior.
- Update help text and command completion/documentation for the new bindings,
  `:step`, removed follow/chromatic controls, and command-mode pause behavior.

## 5. Testing and acceptance

All acceptance tests must run without MIDI hardware using the existing mock MIDI
backend or event collectors.

### Mode and navigation

- PLAY is the default mode and the header displays each selected mode.
- Ctrl-P/Ctrl-R/Ctrl-E switch modes and preserve cursor/track/nibble state.
- Partial nibble edits are finalized on mode changes.
- PLAY/REC horizontal controls select tracks cell-wise; EDIT retains nibble
  navigation.
- PLAY and REC follow with the cursor row locked to the playhead while playing;
  EDIT does not.
- Switching modes does not start/stop playback; stopping leaves the cursor at
  the stopping row.

### Note behavior and inheritance

- PLAY sends the expected NoteOn and does not mutate or advance the pattern.
- PLAY/REC use the selected scale, including the default chromatic scale, and
  chromatic input uses the existing piano key layout.
- Channel and velocity are independently inherited from the nearest eligible
  event at or before the current row, with 0/100 fallbacks.
- Step REC replaces a cell, uses offset 0, advances by the configured step, and
  clamps at the final row; step 0 does not advance. Each recorded note is one
  independent undo step.
- Live REC replaces the playhead cell, captures the intra-row offset, updates
  the running future stream, removes replaced pending events, and does not
  duplicate the immediately sent note.

### Controls and settings

- Space and uppercase-`S` start-row behavior work in all musical modes.
- Esc stops active playback and retains existing behavior when inactive.
- Alt+0..9 and `:step` set the session-local recording step, including values
  greater than 9; the default is 1 and it is not serialized.
- `:follow`, Ctrl-F, Ctrl-N, and chromatic toggle behavior are removed.
- PLAY/REC editing actions are disabled except for undo and redo.
- Redo works through Ctrl-Y.
- Chromatic is a selectable built-in scale and the default for new projects.

### Command mode and playback

- Entering command mode pauses active playback at the exact tick and sends
  All Notes Off.
- Successful command execution and cancellation restore the previous musical
  mode and automatically resume playback from the paused tick.
- The resumed stream reflects playback-affecting project changes.
- Command mode entered while stopped does not start playback.

### Verification commands

Run the project's standard checks after implementation:

```text
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Manual TUI verification should cover visible mode labels, playhead-following
cursor behavior, track switching during live REC, command-mode pause/resume,
and Esc behavior in active and inactive playback states.

## 6. Constraints and implementation risks

- The current playback implementation snapshots `render()` into a flat
  `Vec<TimedEvent>` without cell provenance. Live replacement and insertion
  therefore require an explicit provenance/reconciliation design rather than a
  simple append.
- Command-mode pause/resume must preserve fractional wall-clock progress, not
  just the integer row.
- Pattern mutations while a rendered macro stream is active must not corrupt
  scheduler ownership or duplicate macro output.
- There are no existing project files requiring migration from the old
  scale-aware setting.
- Cell edit finalization must not create an unintended mutation when merely
  switching modes or entering a transient command/chooser mode.
