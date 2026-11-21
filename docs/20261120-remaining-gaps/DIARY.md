# Implementation diary

## Purpose

This file is an append-only timestamped log of completed implementation work,
resolved decisions, and meaningful milestones. Earlier entries must never be
edited. Pending work is tracked in one `HANDOFF` section at the end of the file;
it should be updated before each context flush so another implementing agent
can resume accurately. Ordered work belongs in `PHASES.md`, and durable design
decisions belong in `DECISIONS.md`.

## 2026-11-21T10:45:00Z — Remaining-gaps phases G0–G8 complete

Implemented the full remaining-gaps milestone across `mtrak-core`,
`mtrak-engine`, `mtrak-midi-io`, and `mtrak-tui`. Decisions RG-001…RG-007 were
recorded before/while coding.

- **G0 — Event model.** Removed `Event::Raw`; added validated
  `Event::Wire(Vec<u8>)` and file-only `Event::SmfEscape(Vec<u8>)`. Added
  `Event::from_event_bytes`/`to_event_bytes` for the `:event` forms, wire
  validation, faithful wire→SMF conversion, and location-specific encode
  errors. Updated metadata and SMF I/O to the new variants.
- **G1 — Global settings.** Removed the pattern-local settings layer and
  `Setting<T>`; `Settings` is now one concrete composition-global value.
  `Composition::effective_settings` returns the global settings.
- **G2 — Lua modules.** Migrated built-in macros `01`/`02` to
  `mtrak-core/src/builtins/NN.lua` (`include_str!`), added module `00` setup
  side effects, numeric-hex `require` with caching and circular detection, and
  the single ID-keyed module map. The scheduler routes built-ins through Lua
  when available and otherwise uses a native fallback.
- **G3 — Nibble editing.** Added `edit.rs` (six-nibble event buffers, four-nibble
  offset buffers, immediate conversion, blank-to-zero finalization, reserved
  clearing), a nibble cursor with track wrapping, and a nibble clipboard.
  Replaced the modal event editor with the line-edited `:event` command.
- **G4 — Offsets.** Four-digit formatting, range validation with red invalid
  display, leave-cell clamping, and screen-wide `o` visibility that resets on
  pattern switch.
- **G5 — Workspaces & chooser.** `F1`/`F2`/`F3` screens, a centered 16×16
  chooser with hexadecimal jumps, lazy pattern/module creation, protected
  main pattern and read-only built-ins, and type-specific copy/cut/paste.
- **G6 — Code screen.** Multiline Lua editor (`code.rs`) with cursor movement,
  insertion, newlines, Backspace/Delete, Tab, viewport scrolling, per-module
  undo, `F5` reload, and lexical highlighting.
- **G7 — Undo.** Per-pattern and per-module content histories plus separate
  Pattern/Code chooser structural histories; replacement discards the replaced
  item's content history.
- **G8 — Strict import & commands.** Added SMF container/track-framing
  validation with clear errors, `:event` parsing, and removed pattern-local
  commands.

Verification: `cargo fmt --all -- --check` clean; `cargo clippy --workspace
--all-targets -- -D warnings` clean; `cargo test --workspace` 160 tests;
`cargo clippy -p mtrak-engine --features lua` and
`cargo test -p mtrak-engine --features lua` (25 tests) clean.

## 2026-11-21T11:30:00Z — Built-in macros are Lua-only; `lua` feature removed

Follow-up to the G0–G8 milestone above, which supersedes its note about a
native scheduler fallback and its `--features lua` verification commands.

- Removed `Engine::native_builtin`. Built-in macros `01`/`02` now have a single
  implementation: the Lua modules in `crates/mtrak-core/src/builtins/`. When a
  caller installs a factory that provides them, that factory runs them;
  otherwise the engine lazily creates its own built-in `LuaRuntime` (not Rust
  code). The engine keeps one implementation, and scheduler tests that use
  custom Rust factories still get built-ins through the internal runtime.
- Made Lua mandatory: `mlua` is a normal dependency of `mtrak-engine`, the
  `lua` feature is gone from `mtrak-engine` and the `mtrak` binary, and
  `lua_support` in the binary is unconditional. `midir` remains the only
  optional feature.

Verification: `cargo fmt --all -- --check` clean; `cargo clippy --workspace
--all-targets -- -D warnings` clean; `cargo test --workspace` 160 tests;
`mtrak --version` prints `mtrak 0.1.0`.

## HANDOFF

**State:** Remaining-gaps phases G0–G8 are implemented and green (160 tests).
Lua is a mandatory dependency; built-in macros are Lua-only. The workspace
builds with the default `midir` feature and `mtrak --version` prints
`mtrak 0.1.0`.

**Validation commands:**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The default `midir` feature needs `libasound2-dev` to build. Lua (vendored
5.4) always builds and needs a C compiler.

**Known deliberate limitations:**

- The Code editor and highlighter are hand-written (RG-007); no external
  ratatui-0.28-compatible Lua editor exists without an MSRV-breaking upgrade.
- Built-in macros are Lua-only (RG-005, amended); the engine creates its own
  built-in runtime only when the caller's factory does not provide them.
- Project metadata format version is unchanged (`1`); the format has not been
  released, so no migration is provided.
