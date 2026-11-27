# Design decisions

## Purpose

This file records durable design decisions that shape implementation across
multiple phases. Decisions are never deleted: when an answer changes, the old
decision is marked `superseded by Dn` and a new decision is added. Ordered work
belongs in `PHASES.md`, and `DIARY.md` records that a decision was resolved
without duplicating the decision itself.

## D-101 — Fennel is the sole macro source language

Macros are authored only in Fennel; Lua 5.4 remains the execution VM. There is
no Lua fallback and no source-language compatibility mode: a Lua source file is
simply invalid Fennel. There are no released projects whose Lua source must be
preserved, so this is a source-language migration rather than a compatibility
migration. `LuaCommand` and other Lua-oriented public names are made neutral
(`ProjectCommand`), while `LuaMacro` keeps its Lua-execution identity.

## D-102 — The compiler runs in a separate, restricted Lua state

`FennelCompiler` owns its own Lua state, loads the embedded standalone compiler
there, and never shares the macro execution state. Before any user source is
compiled, `io.open` is replaced with a function that always fails, `os` and
`debug` are removed, and `package.path`, `package.cpath`, `package.searchers`,
and `package.loaders` are cleared. `include` and `import-macros` therefore
cannot read files or load external modules; compile-time macros are allowed only
when their definitions appear in the source being compiled. This preserves the
no-filesystem, no-network, and no-process guarantees and cannot be bypassed by
Fennel syntax because the generated Lua is still evaluated in the existing
restricted module environment.

## D-103 — Per-module allowlist and Fennel global mangling

Compilation uses Fennel's strict global validation with an explicit allowlist of
the curated standard-library globals plus the mtrak API. The allowlist is
module-aware: `register-command` is available only for module `00`. Fennel
leaves valid Lua identifiers alone but rewrites other global names as
`__fnl_global__...`; the execution environment binds every kebab-case API name
under both its Fennel spelling and its mangled spelling (via
`fennel_global_name`), so kebab-case names are authoritative. No snake-case
compatibility aliases are kept.

## D-104 — Source correlation and error classification

Every compilation enables Fennel source correlation and uses the synthetic name
`module NN.fnl`, so generated Lua line numbers match the Fennel lines shown in
the Code screen and runtime diagnostics point back at the authored source. A
Fennel compilation failure is a load/validation error (`MacroError::Compile`)
that leaves no cached module and no stale loading marker; generated Lua
execution failures remain ordinary module/runtime errors (`MacroError::Lua`).

## D-105 — The compiler asset is checked in and embedded

The official standalone compiler lives at
`crates/mtrak-engine/assets/fennel.lua` (Fennel 1.6.1) and is embedded with
`include_str!`. Normal builds never access the network and never require an
installed Fennel command. `scripts/update-fennel.sh`, invoked by `make
update-fennel FENNEL_VERSION=...`, downloads
`https://fennel-lang.org/downloads/fennel-${FENNEL_VERSION}.lua` and replaces
the asset only after a successful download. The reported compiler version is
asserted against `FENNEL_VERSION` in `fennel.rs`.

## D-106 — No new compiler resource-limit mechanism

The selected Fennel compiler exposes no supported compilation-resource bound, so
this milestone does not add a host-side compiler limit. Runaway generated Lua is
still bounded by the existing instruction budget, memory limit, and live
coroutine limit during macro execution.
