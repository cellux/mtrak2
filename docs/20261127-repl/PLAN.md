# Local networked Fennel REPL

## Purpose

This document specifies a milestone for adding a local, model-friendly HTTP
REPL to mtrak. The REPL evaluates Fennel forms synchronously in the context of
a running mtrak application and exposes a controlled API for inspecting and
mutating the current in-memory project. Ordered implementation phases,
durable design decisions, and completed-work history belong in `PHASES.md`,
`DECISIONS.md`, and `DIARY.md` respectively.

## 1. Goals

Implement a localhost-only networked REPL that:

- is enabled by `--repl`;
- listens on `127.0.0.1:6069` by default;
- accepts an optional `--repl-port PORT` override;
- fails startup clearly if the requested port is unavailable;
- rejects `--repl-port` when `--repl` is not also supplied;
- exposes a minimal HTTP surface consisting of `POST /eval`, `POST /play`,
  `POST /stop`, and `POST /repl/reset`;
- uses JSON request and response bodies so agents can call it directly with
  `curl` and other standard tools;
- maintains one persistent REPL session for the lifetime of the running mtrak
  process;
- evaluates ordinary synchronous Fennel forms, not REPL coroutines;
- lets the model inspect and modify the currently running in-memory project;
- lets the model create, modify, and delete patterns and authored macro
  modules;
- validates each individual macro-source mutation before committing that
  mutation;
- provides structural Fennel source editing through a `replace-form` API while
  preserving all source outside the replaced form;
- shares the restricted Fennel execution constraints of macro code, including
  no filesystem, process, network, debug, or equivalent host access;
- keeps the existing macro execution model and scheduler semantics intact; and
- leaves persistence to the user through the existing save workflow.

The REPL is intended to let an agent build a complete composition incrementally
in one session. It is not a remote-control protocol for playback or file
management.

## 2. Scope and compatibility

The REPL changes the current in-memory `TrackerProject` only. Changes are
marked dirty and are available to the TUI and subsequent renders, but are not
saved automatically. Load, save, playback control, MIDI-port control, and
networked remote access are out of scope.

The server binds only to loopback. It has no authentication layer because this
milestone is explicitly for trusted local development access; binding to an
externally reachable address is not supported.

There is one persistent session. Its Fennel globals live in a dedicated REPL
execution environment and do not leak into macro modules or the host process.
A `local` binding remains scoped to the submitted form. The session exposes a
reset operation that clears session-defined globals without changing project
state.

The REPL uses a canonical `mtrak` namespace. Existing short macro execution
aliases remain available to macro modules. The old `get-setting` and
`set-setting` names are not retained as compatibility aliases; settings use
the namespace API described below.

## 3. HTTP protocol

The initial HTTP surface is:

```http
POST /eval
Content-Type: application/json

{"form":"(mtrak.patterns.list)"}
```

```http
POST /play
Content-Type: application/json

{"pattern":3,"row":16}
```

```http
POST /stop
POST /repl/reset
```

`/eval` executes one form synchronously. `/play` accepts optional `pattern`
and `row` fields; omitted fields mean the current TUI-selected pattern and
configured start row. It validates the requested point before changing
playback, then starts that pattern as a temporary playback root without
changing selected-main project metadata. `/stop` uses the normal stop-and-
release path. `/repl/reset` recreates only the REPL environment and does not
change project, macro runtime, TUI, or playback state.

All responses use JSON and contain a stable structured success or error
result. The response should include, as applicable:

- the returned value when it can be represented as JSON;
- a readable Fennel/Lua printed representation and type metadata for values
  that are not naturally JSON values;
- captured printed output, if the environment permits it; and
- structured diagnostics and source locations for compile or runtime errors.

Request bodies are limited to 64 KiB. Serialized responses are limited to
1 MiB. JSON conversion also enforces fixed depth and node-count limits and
falls back to a printed representation with a diagnostic for cyclic,
pathological, or otherwise non-serializable values. The transport remains
JSON-only; EDN is deferred.

Requests are serialized. A request is dispatched to the application thread,
which owns the TUI state, project, macro source map, and live macro runtime.
The HTTP worker waits for the application-thread result while the normal TUI
loop continues to run. This avoids concurrent mutation of application state.

Evaluation uses the existing Lua memory and instruction safeguards, plus fixed
implementation-defined safe defaults for a wall-clock request timeout, so an
accidental infinite loop cannot block mtrak indefinitely. The timeout and
resource-limit diagnostics must be deterministic and useful to an agent.

## 4. Execution environments

The REPL gets a persistent Fennel/Lua state with:

- the same curated standard library and sandbox restrictions as macro code;
- no `io`, `os`, `debug`, filesystem helpers, process helpers, network
  helpers, FFI, or external module loading;
- the canonical `mtrak` namespace;
- ordinary Fennel evaluation and persistent session globals; and
- no macro coroutine context.

The `mtrak` namespace is a normal mutable table. A user may replace it or an
alias deliberately; the implementation does not promise that it is
immutable. The host retains the original API bindings in the evaluation
context as needed to service the current form. Values returned by the host API
are ordinary snapshot tables, not live mutable host objects; all project
changes go through validated API calls.

The namespace contents are context-dependent:

- macro modules receive execution-safe macro APIs and the existing top-level
  aliases;
- the REPL receives the synchronous project-inspection and project-mutation
  APIs below;
- project-mutating APIs are not available to macro coroutine environments;
- `emit`, `wait`, `trigger`, and `stop-track` are rejected in the initial REPL
  because it does not run as a macro coroutine;
- live REPL MIDI output and self-modifying compositions are deferred.

## 5. Canonical REPL API

### 5.1 Patterns

Pattern IDs are explicit stable IDs. The API uses the existing composition
constraints and errors, including the valid ID range, occupied slots, the
selected-main deletion restriction, LPB/TPB compatibility, and row/track
limits.

The initial low-level namespace is:

```fennel
(mtrak.patterns.list)
(mtrak.patterns.get id)
(mtrak.patterns.create id {:lpb 4 :rows 64 :tracks 16})
(mtrak.patterns.delete id)
(mtrak.patterns.set-name id "bass")
(mtrak.patterns.set-lpb id 4)
(mtrak.patterns.resize id {:rows 128 :tracks 8})
(mtrak.patterns.get-slot id row track)
(mtrak.patterns.set-slot id row track event offset)
```

Operations are immediate and non-transactional. A failed form may leave
mutations made before the failure in place. REPL changes are marked dirty but
do not create entries in the TUI undo stacks.

Slots and events use structured Fennel tables rather than compact hexadecimal
strings. The representation must cover empty, channel, macro, sysex, meta,
SMF-escape, and wire events, including inherited channel/data fields and the
slot offset. `mtrak.events` also provides constructors for common valid event
tables; plain structured tables remain accepted by `set-slot`.

### 5.2 Macro modules

The macro namespace operates on the setup module `00` and user modules in
`40`–`7F`. Built-in modules in `01`–`3F` remain read-only. Module IDs are
always explicit; creation never allocates an ID automatically.

The initial namespace is:

```fennel
(mtrak.macros.list)
(mtrak.macros.get id)
(mtrak.macros.create id source)
(mtrak.macros.update id source)
(mtrak.macros.delete id)
(mtrak.macros.replace-form id prefix replacement)
(mtrak.macros.reload-all)
```

Module `00` is mandatory and cannot be deleted. Creation requires an unused
explicit ID in `40`–`7F`; built-ins and reserved IDs are rejected. Deleting a
module is allowed even if another module may require it; dependent failures
follow the existing lazy-loading behavior.

`replace-form` recursively searches the complete Fennel syntax tree and
requires exactly one match. A zero-match or multiple-match result is an error.
The prefix and replacement are supplied as Fennel forms/data, normally using
quoted forms. The replacement may be completely different from the matched
form. All source outside the matched span is preserved; the replacement span
may be rendered afresh. There is no `replace-all-forms` operation in this
milestone.

Every create, update, delete, and structural replacement validates the changed
module before committing that individual operation. Updating module `00`
runs setup again so command registrations reflect its source. Dependencies are
not eagerly revalidated for each individual mutation; dependent failures are
reported when loaded or used.

`reload-all` builds a fresh runtime from all current module sources and swaps
it in only after complete validation succeeds. On failure, the currently
working runtime remains active. It rejects when the current TUI code-editor
buffer has unsaved changes, so verification never silently checks a stale
module map. Read-only macro inspection may still return the last committed
project source. This gives the model an explicit end-of-task verification
operation without making ordinary mutations transactional.

Structural editing should reuse the embedded Fennel parser and source metadata
rather than introducing a second parser in Rust. A small hybrid helper may
perform AST matching and source-span replacement in the compiler/runtime Lua
state, while host code remains responsible for safe source-map updates and
normal validation.

### 5.3 Settings

Settings are exposed through a callable namespace table:

```fennel
(mtrak.settings)          ; complete settings table
(mtrak.settings :root)    ; one setting
(mtrak.settings.get :root)
(mtrak.settings.set :root 64)
```

The `__call` form is read-only shorthand. The explicit `get` and `set` forms
are the canonical operations for code that should be clear about mutation.
Setting values immediately changes the current in-memory project and marks it
dirty.

### 5.4 Project state

Expose composition-level inspection and mutation through `mtrak.project`,
including at least:

- BPM;
- TPB and timing-related values as inspection data;
- deterministic random seed;
- selected main pattern;
- project dirty status and other useful read-only state.

No TPB-changing operation is exposed in this milestone. LPB changes remain
under `mtrak.patterns.set-lpb`, which applies the existing timing rules. No
project load/save function is included. Playback is controlled only by the
HTTP `/play` and `/stop` endpoints, not by Fennel project functions.

### 5.5 Session control

Expose `mtrak.repl.reset` to clear persistent session-defined globals while
leaving the project, macro sources, and active runtime unchanged.

## 6. Runtime and application integration

Add a REPL service owned by the application startup path. The HTTP listener
runs off the TUI thread, but it never directly borrows or mutates `App`. It
sends an evaluation request over a channel and waits for a typed response.
The application event loop services requests on its own thread, preserving
ordering with keyboard actions, TUI edits, and playback ticks.

When an `/eval` request is accepted, the application uses the normal stop-and-
release path before evaluation. Playback remains stopped after evaluation,
whether the form succeeds or fails. This avoids late MIDI events while the
application thread is occupied by synchronous evaluation. `/play` validates
its requested point before stopping/replacing any current playback and starts a
fresh render only after macro preparation succeeds. Invalid `/play` requests
leave existing playback unchanged.

The REPL host API must update the same `TrackerProject`, dirty flag, project
revision, and macro source/runtime references used by the TUI. It must not
record TUI undo entries. Every successful REPL project mutation clears all TUI
undo history for safety. It should use existing core composition validation
rather than duplicating geometry or timing rules.

REPL mutations also keep transient TUI state coherent: deleting the current
pattern selects the main pattern, resizing clamps cursor/selection state, and
pattern mutations clear partial cell edits and transient selection/brush state.
Macro operations reject changes while the current code-editor buffer differs
from its stored module source, identifying the affected module and instructing
the user to finish or cancel the editor change. Successful macro mutations
refresh the editor when necessary.

While playback is active, a valid REPL mutation is applied to the current
in-memory project after playback has been stopped. The initial milestone does
not attempt to alter an already-rendered event stream; `/play` provides the
explicit way for the agent to audition a new render.

Macro source updates must invalidate the relevant runtime cache. Setup-module
updates must refresh registered project commands. Runtime errors must not
leave stale loading markers or accidentally publish an invalid replacement.

Add CLI parsing for:

```text
mtrak [--repl] [--repl-port PORT] [FILE]
```

while preserving existing file-opening and help/version behavior. The default
port is `6069`; an unavailable requested port is a fatal startup error.
`--repl-port` without `--repl` is an argument error. The listener binds only to
`127.0.0.1` and is shut down with the application.

## 7. Testing and acceptance criteria

Add tests covering at least:

- CLI parsing, default port, override, invalid option combinations, loopback
  binding, and startup failure on an occupied port;
- HTTP request parsing, JSON responses, malformed requests, unknown routes,
  `/play`, `/stop`, `/repl/reset`, structured compile/runtime errors, request
  serialization, loopback binding, and lifecycle shutdown;
- persistent session globals, local scope behavior, session reset, output and
  opaque-value formatting, 64 KiB request and 1 MiB response limits, cyclic or
  deeply nested value handling, request timeouts, instruction limits, and
  memory limits;
- sandbox parity between REPL and macro execution environments;
- pattern listing, explicit-ID creation/deletion, geometry/name/LPB changes,
  slot reads/writes, structured event validation, event constructors, timing
  errors, and selected-main deletion protection;
- macro list/get/create/update/delete behavior, explicit ID validation,
  built-in read-only behavior, per-operation validation, setup refresh,
  dependent-module behavior, and invalid-source preservation;
- recursive `replace-form` matching, quoted prefix/replacement forms, zero and
  multiple match diagnostics, source formatting preservation outside the
  replaced form, replacement compilation, and line/source diagnostics;
- safe `reload-all` success and failure behavior, including preservation of the
  previously active runtime after a failed full reload;
- callable settings behavior, project metadata operations, and the absence of
  any TPB-changing API;
- dirty-state updates, code-editor conflict rejection, TUI selection refresh,
  transient-edit cleanup, and clearing all TUI undo history for REPL
  mutations;
- REPL operation while the TUI and playback loop are active, normal
  stop-and-release before `/eval`, playback remaining stopped afterward, and
  `/play` validation/start behavior without MIDI hardware requirements;
- an end-to-end agent-like composition workflow that creates patterns,
  populates structured events, creates macros, edits a macro structurally, and
  finishes with `reload-all`.

Acceptance requires the existing workspace formatting, lint, and test gates to
continue passing, and normal builds to remain hardware-independent. No network
access is required beyond the local loopback listener, and no remote binding,
load/save endpoint, playback-control Fennel API, live REPL coroutine, direct
MIDI emit, transaction system, or self-modifying composition behavior is
implemented in this milestone.

## 8. Non-goals and future work

The following are explicitly deferred:

- remote or authenticated network access;
- EDN transport;
- multiple sessions;
- asynchronous or streaming evaluation;
- REPL coroutine scheduling, `wait`, live `emit`, `trigger`, and
  `stop-track`;
- self-modifying macro compositions during an active render;
- automatic persistence, project loading, and project saving;
- additional playback or MIDI-port control beyond the `/play` and `/stop`
  endpoints;
- bulk/high-level pattern transformations;
- `replace-all-forms`;
- transactionality across one submitted form; and
- exposing project mutation APIs inside macro coroutine environments.
