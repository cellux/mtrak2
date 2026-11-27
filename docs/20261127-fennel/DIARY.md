# Implementation diary

## Purpose

This file is the append-only, timestamped log of completed implementation work.
Earlier entries are never edited. Pending work is maintained in one `HANDOFF`
section at the end of the file and updated before every context flush so another
agent can resume accurately. Ordered work belongs in `PHASES.md`, and durable
design choices belong in `DECISIONS.md`.

## 2026-11-27 — Phase 1: compiler asset and build update

- Checked in the official standalone Fennel 1.6.1 compiler as
  `crates/mtrak-engine/assets/fennel.lua` and documented it in
  `crates/mtrak-engine/assets/README.md`.
- Added `scripts/update-fennel.sh` and the `make update-fennel
  FENNEL_VERSION=...` target. The script requires `FENNEL_VERSION`, downloads
  the official release, and replaces the asset only on success.
- Added `crates/mtrak/tests/update_fennel.rs` covering the missing-version,
  failed-download, and successful-download paths using `file://` URLs so the
  tests never touch the network.

## 2026-11-27 — Phase 2: compiler integration

- Added `crates/mtrak-engine/src/fennel.rs`: `FennelCompiler`, `CompileError`,
  `FENNEL_VERSION`, `source_name`, and `fennel_global_name`.
- The compiler state is locked down after loading the embedded compiler (stub
  `io.open`, no `os`/`debug`, empty package paths and searchers), and compiles
  with `allowedGlobals`, `correlate`, `unfriendly`, and `error-pinpoint = false`.
- `fennel::tests` verifies initialization, version, correlated output, mangling,
  rejected globals, setup-only names, parse errors, line numbers, compile-time
  macros, and blocked `include`/`import-macros`.

## 2026-11-27 — Phase 3: runtime migration

- Replaced `lua.rs` with `runtime.rs`: `MacroRuntime`, `MacroError`,
  `MacroLimits`, `RuntimeMacroFactory`; `LuaMacro` keeps its Lua-execution role.
- `ensure_module` now compiles before evaluating and before caching, and removes
  the loading marker on every outcome. Kebab-case API names are bound under both
  their Fennel and mangled spellings.
- `runtime::tests` was rewritten in Fennel and extended with sandbox rejection,
  compile-failure cache cleanliness, line numbers, the default template, and
  mangled kebab-case bindings.

## 2026-11-27 — Phase 4: source migration

- Rewrote built-in macros `01`/`02` as `src/builtins/01.fnl`/`02.fnl` and
  `MODULE_TEMPLATE` as Fennel; removed the `.lua` built-ins.
- Rewrote the embedded macro test sources in `runtime.rs`, `smf.rs`, `e2e.rs`,
  and `metadata.rs` as Fennel.

## 2026-11-27 — Phase 5: editor, highlighter, and naming

- Reworked `crates/mtrak-tui/src/code.rs` for Fennel lexical forms and renamed
  `TokenKind::Function` to `TokenKind::Symbol`.
- Renamed `LuaCommand` to `ProjectCommand`, `reload_lua`/`ensure_lua` to
  `reload_macros`/`ensure_macros`, `Action::ReloadLua` to
  `Action::ReloadMacros`, and `LuaSource` to `MacroRuntimeSource`; updated
  comments, status messages, and README text.

## 2026-11-27 — Phase 6: verification

- `cargo fmt --all -- --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo test --workspace` passes: 329 tests (12 end-to-end, 3 update-target,
  46 core, 56 engine, 5 MIDI, 19 MIDI I/O, 188 TUI).
- No macro source in the active codebase remains Lua; the networked/AI REPL was
  not implemented.

## HANDOFF

All phases in `PHASES.md` are complete. No pending work remains for this
milestone. The future networked/AI REPL remains explicitly out of scope; when it
is started, it can reuse `MacroRuntime` and `FennelCompiler` as the compilation
and execution boundary.
