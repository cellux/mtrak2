# Fennel macro runtime migration

## Purpose

This document specifies the migration of mtrak's macro authoring and loading
workflow from Lua source to Fennel source while retaining Lua 5.4 as the
execution runtime. It is the milestone specification for implementation.
Ordered implementation phases, durable design decisions, and completed-work
history belong in `PHASES.md`, `DECISIONS.md`, and `DIARY.md` respectively.

## 1. Goals

Implement a Fennel-first macro system in which:

- Fennel is the sole supported macro source language;
- the standalone Fennel compiler is checked into the repository as a static
  asset and embedded into the mtrak executable;
- every module is compiled from Fennel to Lua before it is evaluated;
- generated Lua executes in the existing sandboxed Lua 5.4 runtime, preserving
  the current macro API capabilities and semantics (with idiomatic Fennel
  spellings), coroutine behavior, module semantics, resource limits,
  deterministic RNG behavior, and setup-module behavior;
- all current built-in macros, the default module template, macro tests, and
  macro-facing documentation are rewritten in Fennel;
- the Code screen and syntax highlighter are suitable for Fennel source;
- a Makefile target can replace the checked-in compiler asset with the official
  standalone `fennel.lua` for a caller-specified `FENNEL_VERSION`;
- the future networked/AI-driven REPL remains out of scope, while the runtime
  remains suitable for that later work.

The implementation must preserve the existing crate boundaries and remain
hardware-independent in tests.

## 2. Existing behavior and compatibility target

Module sources are currently stored as an ID-keyed `BTreeMap<u8, String>` in
project data, with module `00` as the editable setup/library module and user
modules in `40`–`7F`. Built-in sources are embedded with `include_str!` and are
read-only. There is no source filename or extension in the module-storage
model.

The engine currently evaluates each source directly in a per-module sandboxed
environment. The environment exposes the existing API (`emit`, `emit_sysex`,
`emit_meta`, `trigger`, `stop_track`, settings access, `wait`, `require`, and
registration APIs where permitted) plus the curated standard library and
seeded random functions. Module return values, numeric module IDs, lazy loading,
cache invalidation, circular-dependency detection, setup execution, and macro
coroutine creation must continue to work the same way after compilation.

This is a deliberate source-language migration rather than a compatibility
migration. There are no released mtrak projects or user-authored MIDs whose
Lua source must be preserved. Lua remains an implementation/runtime language,
not an accepted macro source language.

## 3. Fennel compiler asset and build update

Add a checked-in standalone Fennel compiler asset, obtained from the official
Fennel release distribution. Embed it into the executable at compile time,
using the existing Rust/Lua embedding approach rather than requiring a runtime
Fennel installation or filesystem access.

Add a Makefile target, for example:

```sh
make update-fennel FENNEL_VERSION=1.5.0
```

The target shall:

- require `FENNEL_VERSION`;
- construct the official release URL
  `https://fennel-lang.org/downloads/fennel-${FENNEL_VERSION}.lua`;
- download the standalone `fennel.lua` asset to a temporary file;
- replace the checked-in compiler asset in the source tree only after the
  download succeeds;
- fail clearly if the variable is missing or the download fails.

The target need not be idempotent and may always replace the current asset.
The checked-in asset is the build input and must be included in source control;
normal builds must not access the network.

The asset location and download command must work with the repository's
existing Makefile conventions and should avoid adding a runtime dependency on
an external executable. The initial checked-in asset should use the latest
stable Fennel release available when this milestone is implemented, and its
reported compiler version should be covered by a test or documented alongside
the asset.

## 4. Runtime compilation architecture

Keep the existing Lua 5.4 execution state and module sandbox as the authority
for macro execution. Introduce a separate compiler Lua state used only to load
the embedded Fennel compiler and compile source strings.

The compiler integration should:

1. create and initialize the compiler state from the embedded `fennel.lua`;
2. invoke the compiler's string-compilation API for each module source, with a
   synthetic source name such as `module 40.fnl` and source-correlation enabled;
3. return generated Lua source or a structured runtime error containing the
   module ID, Fennel source name/line information, and useful diagnostic text;
4. evaluate the generated Lua source in the existing module environment through
   the same `ensure_module` path and the same cache/loading rules.

The compiler state must not expose the macro module environment, injected mtrak
API, project state, or running coroutine state. Compiled output must still be
evaluated only in the existing restricted environment, so Fennel syntax cannot
bypass the current sandbox. Compiler initialization and compilation failures
must not leave partially cached modules or stale loading markers.

Compilation uses Fennel's strict global validation with an explicit allowlist
of only the curated standard-library globals and mtrak API names. The allowlist
is module-aware: setup-only names such as `register-command` are available only
for module `00`; forbidden names such as `os`, `io`, `debug`, `package`, and
filesystem/network/process helpers are rejected during compilation rather than
being deferred to runtime. Fennel's global-name mangling must be accounted for
when installing the allowlisted names into the execution environment.

Fennel-facing mtrak API names use kebab-case where Fennel permits it. The
primary authored surface is:

- `emit`, `emit-sysex`, `emit-meta`, `trigger`, and `stop-track`;
- `get-setting` and `set-setting`;
- `wait` and numeric-ID `require`;
- `random-generator` plus the curated `math.random` API;
- setup-only `register-command` in module `00`.

The implementation may retain internal Rust/Lua snake-case names where useful,
but generated global-name mangling and execution-environment bindings must make
the kebab-case Fennel names authoritative. Compatibility aliases are not
required because there are no existing Fennel projects.

Do not introduce a new host-side compiler resource-limit mechanism merely for
this milestone. If the selected Fennel compiler already exposes supported
configuration for bounding compilation resources, use that configuration and
test its diagnostics; otherwise, resource limits remain those already enforced
for generated Lua during macro execution.

Compilation must preserve mtrak's no-filesystem, no-network, and no-process
access guarantees. The compiler state therefore uses a restricted compiler
environment and must not permit filesystem-backed `include`, module search,
external macro imports, or equivalent host access. Compile-time macros are
allowed only when their definitions are present in the source being compiled;
they may not be loaded from files or external modules. This restriction is
intentional and may be loosened in a later milestone only after a safe design
exists.

Compilation occurs before module evaluation and before a module is cached.
Enable Fennel source correlation for every compilation and preserve the
synthetic module source name through the generated chunk, so runtime diagnostics
map back to the Fennel lines shown in the Code screen rather than only to
compiler-generated Lua lines. A Fennel compilation error is a load/validation
error; generated Lua execution errors remain ordinary module/runtime errors.
Since Lua fallback is explicitly out of scope, a Lua source file is simply
invalid Fennel source.

Use neutral names for the public combined integration, such as
`MacroRuntime` and `MacroError`, because it owns both Fennel compilation and
Lua execution. Keep implementation-specific names explicit: the compiler
integration may be a `FennelCompiler`, while coroutine/runtime implementation
structures may remain `LuaMacro` or otherwise identify their Lua execution
role. Other public source-language names such as `LuaCommand` should likewise
be made neutral where practical. Rename the current Lua-oriented public
integration where practical;
there are no released-project compatibility constraints. Do not change
unrelated scheduler or MIDI behavior.

## 5. Source migration

Rewrite the following sources as idiomatic Fennel while preserving behavior:

- built-in macro `01`, which triggers the pattern named by argument 1;
- built-in macro `02`, which converts the Q8.8 BPM arguments to a Set Tempo
  meta-event;
- the default module template used by module `00` and lazily-created user
  modules;
- all embedded examples and macro test fixtures currently expressed as Lua.

The Fennel forms must continue to produce the module shapes currently
supported by the loader: a callable module function, a table containing a
callable `run`, or a setup/library return value. `require` must return the
exact cached value produced by the compiled module.

Update source comments, crate/module documentation, README text, milestone
references, and existing plan/decision wording that claims macros are Lua
source or that the Code screen is a Lua editor. Preserve historical documents
as history where appropriate; the new milestone documents should describe the
Fennel design and supersede obsolete current-state claims rather than
rewriting completed historical diary entries.

## 6. Code editor and authoring workflow

Retain the existing hand-written multiline editor and per-module editing
workflow, but adapt its lexical highlighter and documentation for Fennel:

- recognize Fennel's delimiter-heavy syntax and common lexical forms;
- highlight Fennel comments, strings, numbers, symbols/keywords, and
  punctuation/delimiters without breaking ordinary editing operations;
- ensure the default template and built-in sources shown in the Code screen are
  valid, readable Fennel;
- keep module selection, read-only built-ins, persistence, per-module undo,
  `F5` reload, and validation-before-playback behavior unchanged.

Syntax highlighting is presentation only; compilation remains the authoritative
validation of source correctness.

## 7. Testing and acceptance criteria

Add or update automated tests covering at least:

- compiler initialization from the embedded asset and compilation of a simple
  Fennel module;
- successful execution of a compiled Fennel macro through the existing
  scheduler, including `wait`, API calls, and module return values;
- built-in macro `01` and `02` behavior after migration;
- `require` caching, circular dependency errors, setup-module registration, and
  lazy module loading after compilation;
- compiler diagnostics for malformed Fennel, rejected globals, forbidden
  compile-time access, source-correlated module names/line numbers, and absence
  of partial cache state;
- any resource-limit configuration natively supported by the selected Fennel
  compiler, if present, plus sandbox restrictions and
  instruction/memory/live-coroutine limits still applying to generated Lua;
- deterministic seeded randomness and render behavior;
- default module creation/persistence and Fennel editor/highlighter behavior;
- Makefile update-target behavior, including missing-version and failed
  download handling, using a testable command structure where practical;
- full workspace integration behavior without MIDI hardware.

Acceptance requires:

- `cargo fmt --all -- --check` passes;
- `cargo clippy --workspace --all-targets -- -D warnings` passes;
- `cargo test --workspace` passes;
- a normal build contains the compiler asset and does not require a network
  connection or an installed Fennel command;
- no macro source in the active codebase is required to remain Lua;
- the future networked AI REPL is not implemented as part of this milestone.

## 8. Non-goals and constraints

This milestone does not implement the localhost HTTP/networked REPL, AI model
integration, remote code execution protocol, or new macro capabilities beyond
the idiomatic Fennel spelling of the existing API. It does not
change Lua 5.4's role as the execution VM, the sandbox policy, module IDs,
project serialization shape, scheduler semantics, or MIDI behavior except as
needed to replace direct source evaluation with Fennel compilation.

The compiler asset update is intentionally a source-tree replacement workflow,
not a dependency-resolution system. The version selected by
`FENNEL_VERSION` is reflected by the resulting checked-in `fennel.lua`; normal
builds consume that file without downloading anything.
