# Diary

This file is an append-only record of completed implementation work. Every future entry will include a timestamp, and earlier entries must not be edited. Pending work will be maintained in a single `HANDOFF` section at the end of the file so another agent can resume after a context flush.

## 2026-11-20T21:40:05Z — Phases P0–P4 complete

Implemented the workspace foundation, the typed event model, composition and
settings, timing/resolution, and the deterministic scheduler. All open
decisions (D-020…D-031) were accepted as written before coding began.

- **P0 — Workspace & tooling.** Six-crate workspace (`mtrak-core`,
  `mtrak-engine`, `mtrak-midi-io`, `mtrak-midi`, `mtrak-tui`, `mtrak` binary),
  `rust-toolchain.toml` (1.85), `rustfmt.toml`, workspace lints, CI workflow,
  `README.md`, `CONTRIBUTING.md`. Dependency direction verified: core/engine do
  not depend on `midir`, `midly`, or terminal crates.
- **P1 — Typed event model** (`mtrak-core::event`). Typed channel/system/meta
  variants with `Option<u8>` presence flags, raw-byte preservation, macro form
  `id arg1 arg2`, event classification, and SMF body encode/decode. 12 tests.
- **P2 — Composition & settings** (`mtrak-core::composition`, `settings`).
  Stable pattern IDs, rows×tracks grid, per-pattern LPB, live settings
  inheritance, built-in + user scales, note parsing/formatting, note-entry
  degrees. 15 tests.
- **P3 — Timing** (`mtrak-core::timing`). Q8.8 BPM, TPB/LPB divisibility,
  common-resolution TPB raising with exact offset rescaling, MIDI division
  limit, and a piecewise tempo map. 14 tests.
- **P4 — Scheduler** (`mtrak-engine::scheduler`, `macros`). Single TPB clock,
  track-local child ownership and replacement, natural-end teardown,
  deterministic simultaneous-event ordering, built-in trigger/tempo macros, and
  a macro-coroutine abstraction with `stop_track(true/false)` semantics. 10
  tests.

Verification: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace` all pass. 51 tests
total.

Toolchain note: the container had `rustc`/`cargo` 1.85.1 but no `rustfmt` or
`clippy` component and no root. Installed a user-local rustup toolchain
(`~/.cargo/bin`) to run the gates; no crate changes depend on that.

## 2026-11-20T21:56:35Z — Phases P5–P7 complete

- **P5 — Lua sandbox & macro runtime** (`mtrak-engine::lua`, optional `lua`
  feature, `mlua` 0.10 vendored Lua 5.4). Modules compile against an empty
  `_ENV` exposing only the injected API (`emit`, `emit_sysex`, `emit_meta`,
  `wait`, `trigger`, `stop_track`, `get_setting`, `set_setting`) plus a curated
  standard library; `register_command` is available only to the setup module.
  Macros run as `Thread` coroutines resumed once per tick. Enforced limits:
  Lua heap limit, a per-resume instruction budget via an instruction hook, and
  a live-coroutine cap. Undefined/reserved macro IDs and failing macros become
  engine diagnostics; `MacroAction::{SetSetting, Diagnostic}` were added and
  settings changes are visible to later reads within the same resume. 10 tests.
- **P6 — SMF I/O & tracker metadata** (`mtrak-midi-io::metadata`, `smf`).
  Versioned, chunked metadata in `FF 7F` sequencer-specific events with a
  distinct fallback marker; project save writes metadata plus a marked rendered
  fallback; standard export renders macros via the scheduler and strips both.
  Reading uses `midly` (running status); writing uses the core event codec.
  Import chooses a lossless one-tick-per-row grid (falling back to a coarser
  divisor only past the row cap) and splits colliding events into extra tracks.
  Composition gained `empty`, `place_pattern`, `set_tpb_raw`, and
  `set_selected_main_raw` for loading. 9 tests.
- **P7 — MIDI backend layer** (`mtrak-midi::backend`, `midir_backend`).
  `MidiBackend`/`MidiOutput` traits, a recording `MockBackend`, an
  `OutputManager` with global port selection, send-failure drop + reconnect
  (D-028), an `EngineSink` integrating with `mtrak_engine::scheduler::Sink`, and
  an `InputSource` ingestion boundary. The real `midir` backend sits behind the
  `midir-backend` feature. 5 tests.

Verification: `cargo fmt --all -- --check` clean; clippy `-D warnings` clean for
default, `mtrak-engine --features lua`, and `mtrak-midi --features midir-backend`;
`cargo test --workspace` 66 tests, `mtrak-engine --features lua` 20 tests,
`mtrak-midi --features midir-backend` 5 tests. CI updated with a `midir` job that
installs `libasound2-dev`.

## 2026-11-20T22:11:07Z — Phase P8 complete

- **P8 — TUI & command layer** (`mtrak-tui`, `mtrak` binary). Terminal-independent
  application state with a 27-test suite, plus a `ratatui` 0.28 drawing layer and
  a `crossterm` event/playback loop.
  - `key.rs`: `Key`/`Code` and the default keymap from `PLAN.md` §2.10, tested
    key-by-key.
  - `undo.rs`: capped snapshot undo/redo; the 64-cap is tested.
  - `clipboard.rs`: rectangular block copy/paste.
  - `editor.rs`: the full event editor; every byte-representable event type
    round-trips through edit→commit.
  - `command.rs`: colon-command parser accepting decimal and `0x`/`0X`, with the
    retained aliases; invalid values error without mutating state.
  - `app.rs`: cursor/brush/selection, insert/delete/backspace-delete/clear,
    cut/copy/paste, scale-aware and chromatic note entry with clamping and
    audition, `:read`/`:write`/`:export` file I/O, pattern management, MIDI port
    commands, and wall-clock playback that streams due events to the MIDI output.
    Project-defined Lua commands are held as display-only `LuaCommand`s.
  - `ui.rs`: header, bordered grid with row/track hex and `◈` complex markers,
    cursor/selection/playback styling, and the bottom-line command/editor prompt.
  - Binary: loads `FILE` (project or MIDI import), selects the first MIDI port,
    builds a Lua macro source and setup commands when the `lua` feature is on,
    and runs the TUI. Default features enable `lua` and `midir`.

Verification: `cargo fmt --all -- --check` clean; `cargo clippy --workspace
--all-targets -- -D warnings` clean; `cargo test --workspace` 102 tests;
`mtrak --version` prints `mtrak 0.1.0`.

Dependency note: `ratatui` 0.28.1 transitively pulled `instability` 0.3.14
(rustc 1.88); `Cargo.lock` pins `instability` to 0.3.3 (darling 0.20) to keep the
1.85 MSRV. CI installs `libasound2-dev` because the binary enables `midir`.

## 2026-11-20T22:14:37Z — CI workflow removed

Removed the GitHub Actions workflow (`.github/workflows/ci.yml`) at the
maintainer's request. Verification is now entirely local:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The default `midir` feature still needs system ALSA headers (`libasound2-dev`
on Debian/Ubuntu) to build. `PHASES.md` P0 and P9 wording was updated to refer
to the documented `cargo` commands instead of a CI job.

## 2026-11-20T22:21:43Z — Phase P9 complete

- **P9 — End-to-end validation & hardening** (`crates/mtrak/tests/e2e.rs`, 12
  tests). Covers every `PLAN.md` §5 target without MIDI hardware:
  - checked-in fixtures (`simple.mid`, `multitrack.mid`, `running_status.mid`)
    under `crates/mtrak/tests/fixtures/`, embedded with `include_bytes!`;
  - deterministic nested-pattern rendering with per-pattern LPB and one clock;
  - track-local child replacement;
  - project save/load without fallback duplication, and persistence of
    settings inheritance, Lua modules and setup source;
  - standard export that renders a Lua macro and contains no metadata, plus
    export→import→export playback equivalence;
  - sandbox denial of host globals and bounded termination of a runaway macro;
  - a TUI session (command-line tempo, note entry, pattern creation, save/load);
  - hardening: arbitrary random bytes, bit-flipped and truncated fixtures, and
    random metadata blobs never panic.
- **Resync hook.** `ui::run_with` invokes a per-frame callback, and the binary
  rebuilds the Lua macro source and setup commands whenever `:read` bumps the
  project revision. This closes the earlier mid-session Lua limitation.
- **UI styling.** Cursor uses a distinct colour in note mode, and the brush /
  affected region now has its own highlight beneath the selection highlight.

Final verification: `cargo fmt --all -- --check` clean; `cargo clippy
--workspace --all-targets -- -D warnings` clean; `cargo test --workspace` 114
tests; `mtrak --version` and `--help` work.

## HANDOFF

**State:** All phases P0–P9 are done and green (114 tests). The workspace has a
single `initial commit`; subsequent work is staged/uncommitted.

**Validation:** `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace` all pass. The
default `midir` feature needs `libasound2-dev` to build.

**Optional future work (not required for v1):**
- MIDI input/recording on top of `mtrak_midi::InputSource`.
- A dedicated scale editor and richer palette/theme support.
- SMPTE (timecode) import currently maps to a fixed TPB of 24.
- `:tpb` is intentionally monotonic-increase only (decision D-021).
