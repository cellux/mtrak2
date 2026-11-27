# Implementation phases

## Purpose

This file turns `PLAN.md`, the milestone specification, into an ordered set of
implementable and independently testable phases. A phase is completed once and
is not subsequently rewritten; implementation progress belongs in the
append-only `DIARY.md`, while design decisions that remain in force across
phases belong in `DECISIONS.md`.

## Phase 1 — Compiler asset and build update

- Check the official standalone Fennel compiler into
  `crates/mtrak-engine/assets/fennel.lua` (Fennel 1.6.1).
- Document the asset version and provenance in
  `crates/mtrak-engine/assets/README.md`.
- Add `scripts/update-fennel.sh` and the `make update-fennel
  FENNEL_VERSION=...` target. The script must require the version, download
  `fennel-${FENNEL_VERSION}.lua`, and replace the asset only after a successful
  download.
- Embed the asset with `include_str!` so normal builds never access the network
  and never need an installed Fennel command.

Verification: `scripts/update-fennel.sh` behavior is covered by an integration
test (`crates/mtrak/tests/update_fennel.rs`) for the missing-version,
failed-download, and successful-download paths.

## Phase 2 — Compiler integration

- Add `crates/mtrak-engine/src/fennel.rs` with `FennelCompiler`, `CompileError`,
  `FENNEL_VERSION`, `source_name`, and `fennel_global_name`.
- Load the embedded compiler in a separate Lua state and restrict it: stub
  `io.open`, remove `os` and `debug`, and clear `package` search paths and
  searchers so `include` and `import-macros` cannot read files or external
  modules.
- Compile with Fennel's strict global validation (`allowedGlobals`), source
  correlation (`correlate = true`), the synthetic name `module NN.fnl`, and
  quiet, escape-free diagnostics.

Verification: `fennel::tests` covers initialization, version, correlated output,
mangling, rejected globals, setup-only names, parse errors, line numbers,
compile-time macros, and blocked external access.

## Phase 3 — Runtime migration

- Replace the Lua-source runtime with `MacroRuntime`/`MacroError`/
  `MacroLimits` and `RuntimeMacroFactory` in `crates/mtrak-engine/src/runtime.rs`.
- Compile every module before evaluating it and before caching it; remove the
  loading marker on every outcome.
- Bind the kebab-case API names under both their Fennel and mangled spellings.
- Keep `LuaMacro` as the Lua execution structure, and keep coroutine,
  instruction, memory, and live-coroutine limits unchanged.

Verification: `runtime::tests` covers execution, waits, API calls, settings,
RNG, `require` caching/cycles/lazy loading, setup registration, sandbox
rejection, compile-failure cache cleanliness, the default template, and the
built-in `01`/`02` macros.

## Phase 4 — Source migration

- Rewrite built-in macros `01` and `02` as `src/builtins/01.fnl` and `02.fnl`.
- Rewrite `MODULE_TEMPLATE` as Fennel.
- Rewrite the embedded macro test sources in the engine, MIDI I/O, and
  end-to-end tests as Fennel.

Verification: the whole workspace test suite passes.

## Phase 5 — Editor, highlighter, and naming

- Adapt `crates/mtrak-tui/src/code.rs` to Fennel lexical forms (comments,
  strings/keywords, numbers, symbols, special forms, delimiters).
- Rename public source-language names to neutral ones (`ProjectCommand`,
  `reload_macros`, `ReloadMacros`, `MacroRuntimeSource`) and update comments and
  README text that claimed macros were Lua source.

Verification: `code::tests` and the Code-screen UI tests pass.

## Phase 6 — Documentation and verification

- Record decisions in `DECISIONS.md` and completed work in `DIARY.md`.
- Run the acceptance gate: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`.
