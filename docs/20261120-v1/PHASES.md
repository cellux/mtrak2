# Phases

This file turns the milestone specification in `PLAN.md` into an ordered set of independently implementable and testable phases. Once implementation begins, each phase will be recorded here as a stable unit of work; progress belongs in `DIARY.md`, while enduring design choices belong in `DECISIONS.md`.

## Conventions

- `Status`: `Not started` | `In progress` | `Blocked` | `Done`.
- `Depends on`: phases that must be `Done` first. Phases without a dependency may run in parallel.
- `Prereq decisions`: entries in `DECISIONS.md` that must be `Accepted` before the phase starts. A phase is `Blocked` until they are.
- `Maps to`: the `PLAN.md` §4 milestone this phase implements (milestones are split where they are too coarse to test independently).
- A phase is complete only when every acceptance criterion is covered by an automated test, except where a criterion is explicitly marked manual.

## Phase map

| Phase | Title | Depends on | Maps to |
|-------|-------|-----------|---------|
| P0 | Workspace & tooling foundation | — | M1 |
| P1 | Typed event model | P0 | M2 |
| P2 | Composition structures & settings inheritance | P0, P1 | M2 |
| P3 | Timing & resolution | P1, P2 | M2, M3 |
| P4 | Deterministic scheduler | P3 | M3 |
| P5 | Lua sandbox & macro runtime | P4 | M4 |
| P6 | SMF I/O & tracker metadata | P1–P4 | M5 |
| P7 | MIDI backend layer | P4 | M6 |
| P8 | TUI & command layer | P2–P4, P6 (partial), P7 | M7 |
| P9 | End-to-end validation & hardening | P5–P8 | M8 |

---

## P0 — Workspace & tooling foundation

**Status:** Done
**Depends on:** —
**Prereq decisions:** D-020 (crate names & public APIs)
**Maps to:** M1

**Goal.** Establish the Rust workspace, crate boundaries, and build/test conventions so later phases have stable homes for code.

**Deliverables.**
- Workspace `Cargo.toml` with the five component crates from `PLAN.md` §2.7 (core model, engine/scheduler+macro, MIDI SMF I/O, MIDI backend, TUI/app).
- Pinned toolchain (`rust-toolchain.toml`), edition, and MSRV.
- `rustfmt.toml`, clippy configuration, and documented `cargo` commands as the single test/lint entry point.
- A short `README`/`CONTRIBUTING` recording conventions and the phase/decision workflow.
- Placeholder libs that compile and expose documented empty public APIs.

**Acceptance criteria.**
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` pass on the empty workspace.
- The commands above are documented in `README`/`CONTRIBUTING` and pass locally on a trivial change.
- Crate dependency direction matches `PLAN.md` §3: the core model/engine crates do not depend on TUI, JACK, `midir`, or `midly`.
- Documented public API stubs exist for each crate (returning `unimplemented`/empty where appropriate).

---

## P1 — Typed event model

**Status:** Done
**Depends on:** P0
**Prereq decisions:** D-023 (event enum & raw-byte rules), D-031 (explicit `00` vs omitted)
**Maps to:** M2

**Goal.** Implement the typed event value used by every other component, replacing the Go prototype's undifferentiated byte arrays.

**Deliverables.**
- Typed event enum covering channel voice messages, system-common and system-exclusive messages, SMF meta-events, tempo, time signature, imported metadata, and the tracker macro call.
- Per-field presence flags that distinguish omitted/inherited, explicit `00`, and supplied values.
- Raw-byte preservation where the model permits, with a documented precedence between typed fields and raw bytes.
- Classification of "inline three-byte" vs "complex" events, plus the uniform macro form `macro-id arg1 arg2`.
- Validation that rejects/does not emit incomplete events.

**Acceptance criteria.**
- Unit tests parse and render every supported message class.
- A test proves omitted vs explicit `00` are distinguishable and that incomplete events are rejected at emit time.
- Raw bytes round-trip byte-for-byte for imported events, including meta-events without a specialized representation.
- Macro events always expose exactly two argument bytes.
- Zero-dependency on any MIDI backend or terminal crate.

---

## P2 — Composition structures & settings inheritance

**Status:** Done
**Depends on:** P0, P1
**Prereq decisions:** D-029 (initial limits)
**Maps to:** M2

**Goal.** Implement composition, patterns, tracks, slots, stable IDs, and live settings inheritance.

**Deliverables.**
- Composition with up to 256 stable pattern-ID slots (`00`–`FF`), global BPM/TPB, selected main pattern, and support for multiple possible main patterns.
- Pattern with per-pattern LPB, any number of tracks, and `N` event rows/slots; one event per slot.
- Track/row resizing that preserves existing content and appends/removes empty cells at the end.
- Global settings module plus per-pattern local module whose `__index` links live to the global one; shadowing semantics.
- Built-in scales (major, natural minor, harmonic minor, melodic minor, pentatonic, whole tone, octatonic) and user-defined 12-bit pitch-class masks with the documented aliases.
- New-project defaults per `PLAN.md` §2.10.

**Acceptance criteria.**
- Deleting a pattern leaves its ID unused and does not renumber other patterns.
- Changing a global setting immediately affects running patterns that have not overridden it; a local override shadows and can be cleared.
- Row/track resize round-trips content and appends/removes at the end only.
- New-project defaults exactly match `PLAN.md` §2.10 (BPM 120, TPB 24, LPB 4, root C4/60, major, degree 0, scale-aware on, pattern `00`, 64 rows, 16 tracks).
- Scale name/alias lookup and pitch-class masks match `PLAN.md` §2.10.

---

## P3 — Timing & resolution

**Status:** Done
**Depends on:** P1, P2
**Prereq decisions:** D-021 (TPB common-resolution & rescaling), D-029 (initial limits)
**Maps to:** M2, M3

**Goal.** Make timing values, the TPB resolution model, and off-grid anchors exact and reversible.

**Deliverables.**
- Invariant enforcement that each pattern's LPB divides TPB exactly; a row occupies `TPB / LPB` global ticks.
- Automatic choice/increase of TPB to a common multiple of all pattern LPBs and imported MIDI resolutions.
- Event storage as a row anchor plus an explicit integer intra-row offset in the global TPB tick domain.
- A documented TPB-rescaling procedure that keeps stored offsets exact when TPB changes (including LPB removal and overflow/limit behavior).
- Tempo and time-signature events as first-class timed events; tempo affects playback from the event position onward; time signatures are preserved but need not drive the primary display.

**Acceptance criteria.**
- Property tests confirm LPB divisibility and correct row tick length for representative and adversarial LPB/TPB pairs.
- A test proves TPB selection/increase yields a common multiple across mixed LPBs and unusual MIDI divisions.
- Offsets survive a TPB rescale exactly, and the rescale is idempotent when TPB already divides.
- Overflow/MIDI-division-limit behavior is tested at the boundary.
- Tempo-change timing is verified from the event position onward; time signatures round-trip unchanged.

---

## P4 — Deterministic scheduler

**Status:** Done
**Depends on:** P3
**Prereq decisions:** D-022 (simultaneous-event ordering), D-026 (coroutine ownership/cancellation), D-029 (initial limits), D-030 (trigger mechanism)
**Maps to:** M3

**Goal.** A deterministic tick-driven engine that emits timestamped events to a collector, with no MIDI hardware required.

**Deliverables.**
- Single global TPB tick clock shared by all tracks, pattern instances, and macro coroutines.
- Pattern instances with track-local child ownership: a trigger on a parent track starts a child owned by that track; a later trigger on the same track replaces the prior instance and its descendants; the parent keeps running.
- Natural-end stopping (no auto-loop) per `PLAN.md` §2.2.
- Tempo-driven timing from P3.
- Coroutine scheduling hook (resume once per engine tick), implemented in P5.
- Event emission into a mock/collector sink with a deterministic timestamp.

**Acceptance criteria.**
- Golden tests of the timestamped event stream (fake clock) for nested patterns with differing LPB, global BPM/TPB, and one shared clock.
- Replacement, descendant, and natural-end semantics match `PLAN.md` §2.2.
- `stop_track(true)` cancels all coroutines and child instances on the track including the caller; `stop_track(false)` preserves the caller — verified for descendants and for coroutines that outlive their replaced instance.
- Simultaneous-event ordering is deterministic and matches D-022.
- Repeated runs on identical input produce byte-identical event streams.

---

## P5 — Lua sandbox & macro runtime

**Status:** Done
**Depends on:** P4
**Prereq decisions:** D-025 (Lua module packaging), D-026 (coroutine ownership/cancellation/limits), D-029 (initial limits)
**Maps to:** M4

**Goal.** Embed `mlua` in a strict sandbox and run macros as deterministically scheduled coroutines.

**Deliverables.**
- Strict sandbox: no filesystem, network, process, unrestricted host objects, `os`, `io`, `debug`, `package`, or FFI.
- Module loading and the fixed entry point `run(arg1, arg2, context)`.
- Built-in macro `01` (trigger pattern `arg1`) and `02` (Q8.8 tempo shorthand); undefined built-ins reserved and rejected; user macros `40`–`7F`; `00` reserved for the setup module.
- Injected API: emit MIDI/custom events; `wait(n)` in global ticks; trigger child patterns; `stop_track(include_current)`; read/change permitted global/pattern settings; register project-local TUI commands.
- Setup module `00` executed automatically at project load; project-specific `:` commands registered.
- Practical instruction/time and allocation limits with bounded, isolated failure behavior.

**Acceptance criteria.**
- Tests confirm the sandbox blocks filesystem, network, process, `os`, `io`, `debug`, `package`, and FFI access.
- Built-ins `01`/`02` behave as specified; undefined built-in IDs are rejected; user IDs `40`–`7F` load and run.
- `wait(n)` counts global ticks and is independent of the invoking pattern's LPB.
- A failing/over-budget macro cannot corrupt engine state; errors are reported and bounded.
- Setup module auto-runs and its registered commands appear only in the TUI, per D-025.

---

## P6 — SMF I/O & tracker metadata

**Status:** Done
**Depends on:** P1, P2, P3, P4
**Prereq decisions:** D-024 (metadata schema/versioning & fallback marker), D-025 (Lua module packaging)
**Maps to:** M5

**Goal.** Read and write Standard MIDI Files through `midly`, with tracker project metadata carried in standards-valid containers.

**Deliverables.**
- Project save: standards-valid SMF storing authoritative tracker structure (patterns, macros, Lua source, settings, custom state) in sequencer-specific meta-events (`FF 7F`), plus a rendered ordinary-MIDI fallback explicitly marked so the loader discards it.
- Standard export: renders macros and custom behavior into ordinary MIDI, strips tracker metadata, starts at the selected main pattern, ends at that pattern's end.
- Import: accepts standard `.mid`, preserves standard events and file metadata losslessly where the model permits, chooses a canonical resolution automatically, creates one main pattern, splits source tracks to avoid one-slot collisions, and preserves tempo/time-signature events.
- Versioned, validated metadata schema with a documented chunking scheme for large payloads.

**Acceptance criteria.**
- Project save/load round-trips without duplicating rendered fallback events.
- Standard export is usable without mtrak metadata and renders macros/custom events.
- Import→export playback equivalence holds even when track layout differs.
- Malformed/untrusted project data is rejected or repaired deterministically.
- Metadata schema version and fallback marker match D-024; large metadata (many patterns + Lua sources) round-trips via the chunking scheme.

---

## P7 — MIDI backend layer

**Status:** Done
**Depends on:** P4
**Prereq decisions:** D-028 (output scheduling / port reconnect / error handling)
**Maps to:** M6

**Goal.** A portable backend abstraction over `midir` with a single globally selected output port.

**Deliverables.**
- Backend trait isolating `midir` behind project types; device enumeration and one globally selected output port.
- Real-time emission of scheduler output through the selected port (timestamped where the backend allows).
- Documented reconnection/failure behavior.
- An input-ingestion boundary that can later record incoming messages into patterns/tracks with quantization and off-grid offsets (recording deferred).

**Acceptance criteria.**
- A mock backend proves the engine emits the expected stream without hardware.
- Port selection is global and observable in the UI layer.
- Disconnect/reconnect and backend errors follow D-028 and do not poison engine state.
- The core model/engine crates have no `midir` dependency.
- The input boundary exists and is unit-tested with a synthetic message stream (recording not required in v1).

---

## P8 — TUI & command layer

**Status:** Done
**Depends on:** P2, P3, P4, P7 (and P6 for `read`/`write`/`export`)
**Prereq decisions:** D-027 (complex-event editor layout & keys)
**Maps to:** M7

**Goal.** The tracker interface and colon-command layer, with the keymap isolated from the core engine.

**Deliverables.**
- Tracker grid with status header and bordered grid; four-hex-digit rows, two-hex-digit track headers, six-character inline messages with `·` for empty nibbles; vertical/horizontal scrolling; distinct styling for cursor, brush, selection, playback row, beat rows, and note-entry.
- Block editing: resizable brush and rectangular selection; insert/delete/backspace-delete/zero; cut/copy/paste; undo/redo capped at 64; the default key bindings from `PLAN.md` §2.10.
- Inline hex editing for common three-byte messages and a full complex-event editor usable for ordinary messages too.
- Note entry: scale-aware and chromatic mappings, root/scale/modal-degree conversion, clamping to 127, audition through the selected output port.
- Command layer: retained commands (`read`/`write`/`export`/`bpm`/`tpb`/`lpb`/`root`/`scale`+aliases/`mode`/`rows`/`tracks`) with decimal and `0x`/`0X` integer syntax; pattern, MIDI-port, custom-scale, event, and macro/setup commands; bottom-line prompt and error diagnostics; project-defined Lua commands (TUI-only).
- Pattern selection/creation/deletion with stable numeric IDs; playback controls (`Space`, `s`).

**Acceptance criteria.**
- Unit tests for keymap→command mapping, brush cycling, block operations, undo/redo (including the 64-cap), hex parsing, and note-degree→MIDI mapping with clamping.
- The complex-event editor round-trips every event type through edit→render, and is reachable for common messages.
- Retained command semantics are unchanged and documented; invalid values error without mutating project state.
- Project-defined Lua commands are TUI-only and never leak into exported files.
- Manual: default key bindings from `PLAN.md` §2.10 all work as documented.

---

## P9 — End-to-end validation & hardening

**Status:** Done
**Depends on:** P5, P6, P7, P8
**Prereq decisions:** —
**Maps to:** M8

**Goal.** Prove the `PLAN.md` §5 validation targets with automated tests and harden untrusted-input handling.

**Deliverables.**
- End-to-end tests for project save/load, standard export, MIDI import, macro rendering, tempo maps, track-local pattern replacement, and malformed/untrusted project data.
- Reference MIDI fixtures and a documented fixture strategy.
- Usability hardening pass against `PLAN.md` §2.10.

**Acceptance criteria.**
- Every bullet in `PLAN.md` §5 is demonstrated by at least one automated test running without MIDI hardware.
- Malformed/untrusted project and MIDI inputs cannot panic or corrupt state.
- Fixtures are checked in and the round-trip/equivalence suite is reproducible with the documented `cargo test` command.
