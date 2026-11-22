# Design decisions

## Purpose

This file records the enduring design decisions that shape the implementation
of the milestones in `PLAN.md` and `PHASES.md`. Decisions have stable
identifiers, remain in force across phases until explicitly superseded, and are
never deleted; changed decisions are marked as superseded and replaced by a new
decision.

---

### PR-001 — Musical modes are first-class; command and chooser stay transient

- Status: Accepted
- Decision: the Pattern screen has exactly three musical modes, `Play`, `Rec`,
  and `Edit`. `Command` and `Chooser` remain transient UI modes layered over
  them. `Mode::Normal` and `Mode::Note` are removed.
- Consequence: key dispatch asks the musical mode what is allowed, while
  command/chooser handling stays in front of it. The prior musical mode is
  remembered while a transient mode is open.
- Source: `PLAN.md` §3.1, §4.1.

### PR-002 — One mode-transition helper finalizes edits and preserves coordinates

- Status: Accepted
- Decision: every musical-mode transition goes through one helper that
  finalizes an active `CellEdit` exactly once (using the existing
  blank-to-zero/finalization rules), clears selection state that the new mode
  cannot use, and preserves the cursor row/track/nibble unless the new mode
  follows playback.
- Consequence: switching modes never creates an unintended mutation, and
  PLAY/REC's cell-wide horizontal navigation cannot destroy the nibble
  coordinate used when returning to EDIT.
- Source: `PLAN.md` §3.1, §4.1, §6.

### PR-003 — Follow is derived from the musical mode

- Status: Accepted
- Decision: the user-configurable follow flag, `Ctrl-F`, and `:follow` are
  removed. PLAY and REC follow the playhead whenever playback is running; EDIT
  never follows. Follow means the logical cursor row tracks the playhead, not
  merely that the view scrolls.
- Consequence: `App` has no persistent follow state. Follow behavior is a
  read-only function of `mode` plus `playing`; entering PLAY/REC during
  playback snaps the cursor row to the playhead, and stopping playback leaves
  the cursor at the stopping row.
- Source: `PLAN.md` §3.5, §4.5.

### PR-004 — Chromatic is an ordinary built-in scale; `scale_aware` is removed

- Status: Accepted
- Decision: `chromatic` becomes a built-in 12-note scale and the default for
  new projects. The independent `scale_aware` setting is removed from the
  model, metadata, scheduler setting changes, and Lua settings API.
- Consequence: `Settings::degree_to_note` always maps a degree through the
  selected (modal) scale. The chromatic scale's intervals are the twelve
  semitones, so chromatic entry falls out of the same mapping. The TUI chooses
  the piano key map when the selected scale is chromatic and the scale-degree
  map otherwise.
- Source: `PLAN.md` §3.8, §4.4.

### PR-005 — Channel and velocity inheritance are independent backward scans

- Status: Accepted
- Decision: a new PLAY/REC note inherits its channel from the nearest
  channel-voice event with a channel, and its velocity from the nearest
  NoteOn/NoteOff event with a second data byte. Each scan starts at the current
  cell and walks toward earlier rows on the selected track only. Missing
  values fall back to channel `0` and velocity `100`.
- Consequence: a control-change event can supply a channel without forcing its
  data byte to be read as a velocity, and a note-off can supply a velocity.
- Source: `PLAN.md` §3.2, §4.2.

### PR-006 — Live recording reconciles by rebuilding the future stream

- Status: Accepted
- Decision: after a live pattern mutation, discard the not-yet-emitted tail of
  the running event stream and re-render the composition. Keep the current
  playback tick and fractional microsecond state. The rebuilt stream starts
  strictly after the current tick, because every event at or before that tick
  has already been sent; the immediately auditioned note is therefore already
  emitted and is never sent twice. Replaced pending events and macro-generated
  descendants disappear simply because they are absent from the new render.
- Consequence: no event provenance is required. This is deliberately a full
  future-stream rebuild; it is optimized only if measurement shows that live
  recording latency is disruptive.
- Source: `PLAN.md` §3.3, §4.3, §6.

### PR-007 — Command mode pauses and resumes playback around the tick

- Status: Accepted
- Decision: entering command mode while playback is active sends All Notes Off
  and pauses playback, capturing the exact tick, fractional microsecond state,
  playhead row, start row, end tick, and musical mode. On successful command
  execution or cancellation, playback resumes from that tick against a freshly
  rendered future stream. Command mode entered while stopped never starts
  playback. A failed command keeps command mode open and playback paused.
- Consequence: playback-affecting commands are reflected on resume without
  losing the current position. The resumed stream is rebuilt with the same
  strictly-after-current-tick rule as PR-006 to avoid resending due events.
- Source: `PLAN.md` §3.9, §4.3.

### PR-008 — The recording step is session-local and EDIT is always step zero

- Status: Accepted
- Decision: the recording step is a nonnegative integer row count, default `1`,
  stored on `App`, set by `Alt+0`..`Alt+9` and `:step N`, and never serialized.
  It advances stopped step recording only. EDIT's effective step is always `0`;
  PLAY never advances.
- Consequence: `:step` accepts values greater than nine even though the key
  bindings stop at nine. Step `0` leaves the cursor row unchanged, and an
  advance past the final row clamps to it.
- Source: `PLAN.md` §3.7, §4.4.

### PR-009 — Control keys move: Redo to `Ctrl-Y`, start row to `S`, `Esc` stops

- Status: Accepted
- Decision: `Ctrl-R` enters REC, so Redo moves to `Ctrl-Y`. Uppercase `S` sets
  the playback start row in all three musical modes, leaving lowercase `s`
  available to the chromatic piano layout. `Esc` stops active playback before
  any context-sensitive cancellation behavior.
- Consequence: the pattern keymap no longer binds `s` to the start row, and
  `Esc` handling precedes selection/command/chooser cancellation while
  playback is running. Lowercase `s` does nothing in EDIT.
- Source: `PLAN.md` §3.6, §4.1.

### PR-010 — PLAY and REC whitelist actions instead of duplicating dispatch

- Status: Accepted
- Decision: `App::apply` receives every mapped action, but in PLAY and REC only
  navigation, playback controls, mode switching, command entry, and undo/redo
  are honored. Nibble typing, structural edits, selections, brush changes,
  clipboard operations, offset toggling, track insertion/deletion, and event
  editing are ignored. Left/Right become cell-wide track selection in PLAY and
  REC.
- Consequence: editing cannot leak through a newly added action; a test asserts
  the gate for every edit action. The nibble coordinate is retained even though
  horizontal movement is cell-wide.
- Source: `PLAN.md` §3.2, §3.3, §3.4, §4.1.