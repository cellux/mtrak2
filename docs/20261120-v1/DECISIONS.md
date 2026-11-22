# Decisions

This file records the design decisions that shape the implementation across phases. Decisions remain in force until explicitly superseded; changed decisions are retained and marked as superseded rather than deleted. Progress and resolution history belong in `DIARY.md`, while ordered implementation work belongs in `PHASES.md`.

## Status legend

- `Accepted` — settled; implementation must conform.
- `Open` — not yet settled; `Gating` names the `PHASES.md` phase that must not start until it becomes `Accepted`.
- `Superseded` — retained for history; the `Superseded by` field names the replacement.

All previously open decisions below were accepted as written before implementation began. IDs are stable and are never reused.

---

## Accepted decisions

### D-001 — Component crate boundaries

- Status: Accepted
- Decision: the implementation is split into five components — core composition/typed event model; deterministic scheduler and macro runtime; SMF/metadata I/O; MIDI backend abstraction; TUI and command layer — with dependency direction pointing inward toward the core.
- Rationale: `PLAN.md` §2.7 and §3 (core must not depend on TUI, JACK, or a specific MIDI backend).
- Consequences: core crates must never depend on `midir`, `midly`, or terminal crates.

### D-002 — Typed event model

- Status: Accepted
- Decision: events are typed values, not undifferentiated byte arrays, while retaining compact inline hex editing for common three-byte messages.
- Source: `PLAN.md` §2.3.
- Consequence: see D-023 for the enum/raw-byte rules that implement this.

### D-003 — Standards-valid MIDI containers for tracker data

- Status: Accepted
- Decision: project save writes a standards-valid SMF and carries authoritative tracker data in sequencer-specific meta-events (`FF 7F`), with a rendered ordinary-MIDI fallback marked for discard. Standard export strips tracker metadata.
- Source: `PLAN.md` §2.8.
- Consequence: metadata must be versioned and validated from the start (D-024).

### D-004 — Lua as a controlled extension mechanism

- Status: Accepted
- Decision: user macros are sandboxed Lua modules with no filesystem, network, process, host-object, `os`, `io`, `debug`, `package`, or FFI access; v1 does not require a trust prompt.
- Source: `PLAN.md` §2.5.
- Consequence: Lua never substitutes for core invariants; a macro failure cannot corrupt engine state.

### D-005 — Deterministic, hardware-free testing

- Status: Accepted
- Decision: the scheduler is driven by a fake clock and emits into a mock/collector sink; round-trip, playback-equivalence, and malformed-input tests precede TUI optimization.
- Source: `PLAN.md` §2.7, §3.

### D-006 — Stable pattern IDs

- Status: Accepted
- Decision: a composition holds up to 256 stable pattern IDs (`00`–`FF`); deleting a pattern leaves its slot unused rather than renumbering. Playback/export begin from the user-selected main pattern; multiple possible roots are allowed.
- Source: `PLAN.md` §2.1, §2.8.

### D-007 — Patterns stop at their final row

- Status: Accepted
- Decision: in v1 a pattern instance stops at its last row; it does not auto-loop. Looping/repeats are expressed with child-pattern triggers.
- Source: `PLAN.md` §2.1, §2.10.

### D-008 — Track-local child ownership

- Status: Superseded
- Superseded by: D-032
- Decision: a trigger on a parent track starts a child owned by that track; a later trigger on the same track replaces that instance and its descendants while the parent and its other tracks keep running.
- Source: `PLAN.md` §2.2.
- Consequence: coroutine lifetime is more flexible than pattern ownership (see D-026).
- Superseded because: the parent's final row also tore the child down, which truncated a longer child mid-note. D-032 keeps the ownership back-reference but removes the teardown.

### D-009 — TPB is the single global clock and canonical resolution

- Status: Superseded
- Superseded by: D-034
- Decision: BPM and TPB are composition-wide; TPB is the canonical MIDI resolution and the single scheduler clock; each pattern's LPB must divide TPB exactly and a row lasts `TPB / LPB` ticks. `wait(n)` counts global ticks.
- Source: `PLAN.md` §2.1, §2.4.
- Superseded because: the clock and row-length rules survive in D-034, but `wait(n)` counting ticks makes a macro's timing depend on the file's TPB. D-034 re-denominates waits in beats.

### D-010 — Live settings inheritance

- Status: Accepted
- Decision: pattern-local settings link via `__index` to the global settings module; inheritance is live, not snapshotted; pattern-local values shadow global ones.
- Source: `PLAN.md` §2.6.

### D-011 — Macro ID allocation

- Status: Accepted
- Decision: `00` = setup module (not a composition macro); `01–3F` = built-in; `40–7F` = user; `80–FF` reserved (overlaps MIDI status bytes). Undefined built-ins are reserved and rejected.
- Source: `PLAN.md` §2.5.

### D-012 — Uniform macro event shape

- Status: Accepted
- Decision: every macro event is `macro-id arg1 arg2`, both arguments raw bytes; meaningless arguments are ignored rather than padded deliberately.
- Source: `PLAN.md` §2.5.

### D-013 — One global MIDI output port

- Status: Accepted
- Decision: the v1 application has a single globally selected MIDI output port; automatic JACK wiring is out of scope; core must not bake in JACK assumptions.
- Source: `PLAN.md` §2.7.

### D-014 — MIDI input deferred, boundary exposed

- Status: Accepted
- Decision: real-time output is v1; input/recording is deferred, but the engine exposes an event-ingestion boundary that can later record with quantization and off-grid offsets.
- Source: `PLAN.md` §2.7.

### D-015 — New-project defaults

- Status: Accepted
- Decision: new projects use BPM 120, TPB 24, LPB 4, root C4/60, major scale, degree 0, scale-aware entry on, one pattern (`00`) with 64 rows and 16 tracks.
- Source: `PLAN.md` §2.10.

### D-016 — Undo history cap

- Status: Accepted
- Decision: mutating block operations are undoable; the undo history is capped at 64 actions.
- Source: `PLAN.md` §2.10.

### D-017 — Retained command semantics

- Status: Accepted
- Decision: the Go prototype's `read/write`, `bpm`, `tpb`, `lpb`, `root`, `scale`, `mode`, `rows`, and `tracks` commands keep their semantics; new features add commands without changing them; project-defined Lua commands are TUI-only.
- Source: `PLAN.md` §2.9, §2.10.

---

## Decisions resolved from initial proposals

### D-020 — Crate names & public APIs

- Status: Accepted
- Question: exact workspace member names, crate names, and the public API surface each crate exposes.
- Resolution: five members named for their roles (e.g. `mtrak-core`, `mtrak-engine`, `mtrak-midi-io`, `mtrak-midi`, `mtrak-tui`) plus a thin binary; core/engine expose only owned types and traits, with `midir`/`midly` confined to the I/O and backend crates behind traits.
- Must be resolved before: P0 scaffolding.

### D-021 — Automatic TPB resolution and rescaling

- Status: Accepted
- Question: the algorithm that chooses/increases TPB to a common multiple of all pattern LPBs and imported MIDI resolutions; behavior on LPB removal; overflow and the MIDI division limit; how stored offsets survive a TPB change.
- Resolution: store every timed event as `(row_anchor, intra_row_offset)` in global ticks; raise TPB to `lcm(existing_tpb, new_lpb)` whenever a new LPB is introduced; on any TPB change, multiply every stored offset by `new_tpb / old_tpb` (exact because the old TPB divides the new one); treat introduction-only increases as monotonic and reject/limit changes that would exceed the MIDI division limit, reporting a command error instead of silently quantizing.
- Risk if unresolved: it changes the event storage model, so P1/P3 cannot be finalized.

### D-022 — Simultaneous-event ordering

- Status: Accepted
- Question: deterministic ordering for events at the same tick, and for imported metadata at the same timestamp.
- Resolution: stable tie-break by (tick, track index, slot index, event kind) with an explicit documented precedence for tempo/time-signature versus channel messages; imported events keep source order within a timestamp.
- Implementation note (P4): within a tick the engine orders by precedence class first — tempo, time-signature, other meta, sysex, channel/raw, macro — then by instance creation order, track, row, and offset. Tempo therefore takes effect before channel messages at the same tick.
- Must satisfy: byte-identical repeated runs (P4 acceptance).

### D-023 — Typed event enum & raw-byte preservation

- Status: Accepted
- Question: the full enum shape, which fields are typed vs raw, and the precedence between typed fields and preserved raw bytes.
- Resolution: one enum with variants per message class plus a `Raw`/meta fallback variant; each byte field is `Option<u8>` (presence flag) so omitted ≠ explicit `00`; typed fields take precedence when set, with raw bytes retained for lossless import/export of unrecognized content.
- Related: D-031 (editor representation of explicit `00`).

### D-024 — Tracker metadata schema, versioning & fallback marker

- Status: Accepted
- Question: the `FF 7F` payload schema, version field, validation rules, chunking for large payloads, and the concrete marker identifying the rendered fallback.
- Resolution: a versioned, length-prefixed container with typed sections (settings, patterns, Lua sources, custom state); a reserved chunk type for Lua sources keyed by module ID; a dedicated marker event that the loader recognizes and discards when authoritative metadata is present; reject unknown major versions and validate all lengths before allocation.
- Implementation note (P6): the `FF 7F` payload begins with magic `MTRAK` and a kind byte (`01` metadata chunk, `02` fallback marker). A metadata chunk carries a version byte, a big-endian `u16` index and count, then the payload; the blob is a sequence of `tag u8 + u32 length + body` sections. Chunks are split at 60 000 bytes and reassembled strictly (count/index/version validated before concatenation).
- Related: D-025 (Lua packaging).

### D-025 — Lua module packaging inside meta-events

- Status: Accepted
- Question: how Lua module sources and the setup module are named, stored, and retrieved.
- Resolution: source-keyed chunks referenced by macro ID; setup module stored under ID `00`; a stable `name(source) = macro-id` mapping; sources are UTF-8 and validated at load; project-defined TUI commands are registered only by executing the setup module and are never serialized into export.
- Implementation note (P5/P6): `LuaRuntime::load_module(id, source)` loads `40`–`7F`; `run_setup(source)` executes module `00` and records `register_command` names/descriptions (handlers are retained only in the runtime). Sources are serialized as UTF-8 in the `TAG_LUA` metadata section and are not written to standard export.
- Related: D-024.

### D-026 — Coroutine ownership, cancellation & resource limits

- Status: Accepted
- Question: how cancellation is delivered, the scope of `stop_track`, and the deterministic resource limits.
- Resolution: each coroutine is owned by the `(pattern instance, track)` that created it; `stop_track(true)` cancels all coroutines and child instances on the track including the caller, `stop_track(false)` cancels the others while keeping the caller; replacement of a pattern instance does not implicitly cancel already-running coroutines; limits are a per-macro instruction budget per tick, a total live-coroutine cap per track, and a per-macro allocation cap, all enforced deterministically.
- Implementation note (P4): a macro's requested wait is clamped to at least one tick so a zero wait cannot livelock a single tick; pattern-instance removal (replacement, natural end, or root teardown) does not cancel running coroutines, which stop only when they finish or via `stop_track`. The instruction/allocation limits are enforced in P5.
- Must satisfy: P4/P5 cancellation and bounded-failure criteria.

### D-027 — Complex-event editor layout & keys

- Status: Accepted
- Question: the exact layout and keyboard interaction of the full event editor, and how it coexists with inline hex editing.
- Resolution: a modal overlay opened by a documented key on a marker cell; field-by-field navigation with presence toggles to set/clear optional bytes; an inline hex mode for the three-byte subset that never loses presence information; round-trip invariant enforced by tests.
- Related: D-031.

### D-028 — MIDI output scheduling, reconnection & errors

- Status: Accepted
- Question: how scheduler output is scheduled to the port, and how disconnects/errors are handled.
- Resolution: the backend accepts timestamped events and the engine tolerates a missing/disconnected port by dropping output while keeping the timeline running; reconnection re-selects the last port; errors surface as UI diagnostics, never as engine panics or state corruption.
- Implementation note (P7): `OutputManager` returns `Ok` and drops output when no port is open, and on a send failure it closes the connection, records `last_error`, and returns the error; `reconnect()` re-opens the remembered selection. `EngineSink` ignores events with no live-MIDI representation (meta-events, macros).

### D-029 — Initial limits

- Status: Accepted
- Question: initial limits for patterns, rows, tracks, event size, and Lua memory/time.
- Resolution: patterns fixed at 256 slots (D-006); rows/tracks start at the §2.10 defaults with configurable maxima validated at command time; a maximum serialized event size enforced by the metadata chunker (D-024); Lua limits per D-026.
- Must satisfy: P2/P3 boundary tests and bounded malformed-input handling.

### D-030 — Trigger mechanism unification

- Status: Accepted
- Question: whether the "trigger event on a parent track" (`PLAN.md` §2.2) and macro `01` "trigger the pattern" (`PLAN.md` §2.5) are one mechanism or two.
- Resolution: one mechanism — the track-trigger is implemented by macro `01`, so composing a trigger writes macro `01` with `arg1 = pattern ID`; `arg2` is reserved.
- Risk if unresolved: two competing trigger paths would produce divergent ownership semantics.

### D-031 — Explicit `00` vs omitted in the inline editor

- Status: Accepted
- Question: how the inline six-character hex editor distinguishes an omitted byte from an explicitly supplied `00`, given `·` denotes an empty nibble.
- Resolution: the inline editor exposes only the three-byte message fields; a distinct visual/kinetic affordance (e.g. an explicit-clear vs set-to-zero input) toggles presence, and the complex editor (D-027) can always set presence unambiguously; tests assert the distinction survives edit→render→reload.
- Related: D-023.

### D-032 — Child patterns outlive their parent

- Status: Accepted
- Question: when a trigger's child pattern runs longer than the parent that started it, should the parent's final row tear the child down?
- Resolution: no. A child runs to its own final row; the parent reaching its last row removes only the parent instance. Ownership remains a back-reference on the child, used for replacement and explicit stops, but no longer implies teardown. A replacement or an explicit `stop_track` still removes the whole subtree.
- Source: found while comparing mtrak's playback against fluidsynth's own player; `PLAN.md` §2.2.
- Supersedes: D-008's teardown behaviour.
- Consequence: playback must cover the whole timeline, not just the parent's rows, and the end-of-playback release fires only when nothing is left to sound.

### D-033 — Bound on simultaneously running patterns

- Status: Accepted
- Question: what stops a trigger that starts a pattern which triggers another, ad infinitum — a self-trigger or a cycle?
- Resolution: the engine refuses to start or trigger past `DEFAULT_MAX_INSTANCES` (1024, configurable through `set_max_instances`), and reports the truncation through `overflowed()` and a diagnostic.
- Source: a probe during the same comparison; a self-triggering pattern hung `Engine::run` indefinitely.
- Rationale: the tick bound cannot help, because the runaway happens inside a single tick — `step` loops without advancing the tick, so `max_tick` never fires. With D-032 in force the instance bound is also the only bound on total instances, since a parent's end no longer releases its children.
- Consequence: a recursive composition is truncated and reported rather than hanging; ordinary compositions stay far below the bound.

### D-034 — Macro waits count beats, not ticks

- Status: Accepted
- Question: what unit should `wait(n)` use, given that TPB varies by an order of magnitude between files (24 in a fresh project, 960 in some imported ones)?
- Resolution: `wait(n)` counts beats. It is converted to ticks as `round(n * TPB)` and clamped to at least one tick. Fractional beats are allowed, so `wait(0.25)` is a sixteenth note in 4/4. The surviving rules of D-009 are unchanged: TPB is still the single scheduler clock, each pattern's LPB still divides TPB, and a row still lasts `TPB / LPB` ticks.
- Source: comparing mtrak's playback against fluidsynth's own player; a TPB=960 import made tick-denominated waits 40x denser than a TPB=24 project.
- Rationale: TPB is an export resolution, not a musical quantity. A tick-denominated wait makes the same macro source behave differently in a TPB=24 and a TPB=960 project — `wait(4)` is 83 ms in one and 2 ms in the other. Beats are TPB-independent and, unlike rows, also independent of the invoking pattern's LPB.
- Consequence: the injected `wait` API is denominated in beats while the tick timeline remains the clock. A macro that used `wait(1)` to mean "the next tick" now waits a beat and must be rewritten.

### D-035 — TPB is normalised on import when the row grid allows it

- Status: Accepted
- Question: should a standard MIDI file's PPQ be kept as the project TPB, even at 480 or 960?
- Resolution: reduce it. The import divides the TPB by the largest factor `k` that divides every event tick, taking the largest reachable resolution at or below `TARGET_TPB = 96`; when none is reachable it takes the smallest reachable resolution at or above `MIN_TPB = 24`; when neither exists it keeps the file's TPB. All ticks are scaled by `1/k`.
- Source: the same comparison; TPB=480 and 960 imports drive the engine clock 20-40x faster than a TPB=24 project.
- Rationale: `k` dividing every event tick is exactly the row-boundary constraint — an event's row is `tick / row_ticks` and its offset is `tick % row_ticks`, and both scale by `1/k`, so no event leaves the row it started on. Reducing further would push events off their rows or move them in time. The engine's step cost is `O(instances x tracks)` per tick, so a dense clock multiplies the cost of every running pattern and every tick-granular macro.
- Consequence: foreign files at TPB=480 or 960 import at 96 when their events allow it; files whose events sit on odd ticks keep their original resolution. mtrak's own saved projects are unaffected, because they round-trip through the metadata chunk rather than the standard import.

### D-036 — Randomness is seeded by the composition and deterministic

- Status: Accepted
- Question: macros need randomness, but the engine promises byte-identical runs and a render must be a pure function of the saved project. Stock `math.random` is unseeded and breaks both.
- Resolution: the composition carries a `u64` seed, settable with `:seed`, and macros draw from deterministic SplitMix64 series. `math.random` is shadowed with a version that draws from the composition-seeded global series; `math.randomseed` is refused. `RandomGenerator(seed)` creates an independent generator a macro owns, drawn with `rng:next(...)` or `rng(...)`, with the same argument conventions as `math.random`. The seed is saved in the project metadata and reseeded at every render.
- Source: comparing mtrak's playback against fluidsynth's own player; the determinism hole was found while assessing pre-render versus run-time macro execution.
- Rationale: a pure render makes export and playback provably equivalent, makes re-render-and-splice a valid way to audition edits mid-song, and keeps "byte-identical runs" meaningful. Splitting the API lets a macro choose between composition-wide variation (`math.random`) and its own reproducible series (`RandomGenerator`).
- Consequence: macros can no longer reach an unseeded RNG. A render depends on the seed, so changing it changes the audio even though the events are unchanged. `RandomGenerator` ignores the composition seed by design, so a macro that wants to follow `:seed` passes `ctx.seed` to it.
