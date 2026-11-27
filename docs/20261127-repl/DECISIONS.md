# Design decisions

## Purpose

This file records the durable design decisions that shape implementation across
multiple phases. Decisions are never deleted: when an answer changes, the old
decision is marked `superseded by Dn` and a new decision is added. Ordered work
belongs in `PHASES.md`, and `DIARY.md` records completed work without replacing
the decisions themselves.

## D1 — REPL compilation shares the module compiler

`FennelCompiler::compile_repl` compiles with the synthetic source name `repl`,
`correlate = true`, `unfriendly = true`, and `error-pinpoint = false`, and
builds `allowedGlobals` from the supplied strings. The option-building code is
shared with the module compiler through `compile_with`.

## D2 — One persistent Lua state with a per-evaluation API table

`ReplSession` owns one Lua 5.4 state. A persistent `globals` table holds
session-defined globals. Each evaluation installs a fresh API table whose
`__index` is `globals`, then evaluates the compiled form with `globals` as its
environment. The API table is discarded after the evaluation; session globals
persist. `allowed_globals` is `SAFE_GLOBALS` plus `mtrak` plus the keys
currently present in `globals`, so a user-defined global becomes an allowed
symbol on the next evaluation.

## D3 — Rejected macro globals are explicit error-returning functions

`emit`, `wait`, `trigger`, and `stop-track` are installed as API functions that
raise a runtime error. They are also added to the allowed-globals list. This
keeps them unavailable to macro-coroutine style code while allowing quoted
forms that mention them to compile. The REPL never runs as a macro coroutine.

## D4 — `replace-form` accepts strings, not quoted forms

Fennel 1.6.1's `quote` special is compile-time only. The compiler documents it
as "Only works in macro/compiler scope" and rejects `(quote (1 2 3))` in a
runtime scope with "lists may only be used at compile time". A runtime REPL
therefore cannot accept `'(...)` quoted forms. `mtrak.macros.replace-form`
takes the prefix and replacement as strings. `value_to_source` renders Lua
values as Fennel source, so host callers may still pass strings directly.

Superseded by: none.

## D5 — HTTP workers never touch `App`

The HTTP listener runs on its own threads. Each request is sent over an
`mpsc` channel to the application thread as a `ReplRequest`; the worker blocks
on a response channel. `handle_request` services the request on the application
thread and returns a typed `ReplResponse`. This serializes evaluation with
keyboard input, TUI edits, and playback ticks.

## D6 — `/eval` stops playback first; `/play` validates before stopping

`handle_request` calls `app.stop_playback_and_release()` before evaluating an
`/eval` form. Playback remains stopped afterwards. `/play` validates the
requested pattern and row, ensures macros are ready, and only then stops and
replaces playback. An invalid `/play` leaves existing playback untouched.

## D7 — REPL mutations mark dirty and clear TUI undo

`App::repl_mark_mutated` sets `dirty`, bumps `project_revision`, and clears all
pattern, structural, and module undo stacks. Pattern mutations additionally
call `repl_after_pattern_change` to sync edits and clamp transient selection
state. Macro mutations refresh the open code editor when the affected module is
open.

## D8 — Macro edits reject unsaved code-editor buffers

`repl_editor_conflict` returns an error when the affected module is the open
code-editor module and its buffer differs from the stored source. Create,
update, delete, and `replace-form` all check this before committing. `reload-all`
checks the open module as well.

## D9 — Loopback-only listener with a fixed default port

The listener binds to `127.0.0.1:6069` by default. `--repl-port PORT` overrides
it and requires `--repl`. Binding failure is a fatal startup error. The service
is dropped with the application, which stops the accept thread and joins it.

## D10 — JSON conversion has depth, node, and size limits

Requests are capped at 64 KiB and responses at 1 MiB. `value_to_json` rejects
cycles and non-serializable values, and enforces `MAX_JSON_DEPTH` (32) and
`MAX_JSON_NODES` (10 000). Non-serializable values fall back to a printed
representation with a diagnostic. Evaluation enforces a 30 s wall clock, a
50 M instruction budget, and a 1 GiB Lua heap limit.

## D11 — `replace-form` takes source strings, not Lua values

`mtrak.macros.replace-form` accepts only string arguments. The REPL Lua state
and the compiler Lua state are separate `mlua` instances; passing a table or
other non-string value from the REPL state into the compiler panics with
"Lua instance passed Value created from a different main Lua state". The REPL
therefore rejects non-string prefix/replacement values with a clear error and
leaves the module untouched. Structural matching still works on lists, table
literals, and map literals; sequence literals (`[...]`) are rejected because
Fennel's parser moves their position metadata into the shared sequence
metatable, so no source span is available.

## D12 — Fennel global mangling is mirrored in the sandbox

Fennel rewrites globals that are not valid Lua identifiers to
`__fnl_global__<escaped>` (each non-alphanumeric byte becomes `_XX`). The REPL
therefore:

- installs rejected macro globals (`emit`, `wait`, `trigger`, `stop-track`)
  under both their raw and mangled names, so `(stop-track ...)` reports the
  friendly "not available in the REPL" error; and
- unmangles session globals when building the compiler allowlist, so
  hyphenated globals such as `(global inc-it ...)` remain callable in later
  evaluations.

## D13 — Resource-limit errors keep their own kind

Lua hook errors lose their Rust type when they cross the Lua boundary, so the
hook records the limit in a side cell before raising. The session prefers that
recorded error, which keeps `timeout`, `instruction_limit`, and `memory_limit`
distinct from `runtime` in JSON responses.

## D14 — Pattern mutations bump the revision and clear undo

`repl_after_pattern_change` calls `repl_mark_mutated` after syncing the editor,
so REPL pattern edits mark the project dirty, bump `project_revision`, and clear
the TUI undo stacks, matching the documented D7 behaviour for every mutation.

## D15 — Mutations are atomic

Pattern creation and resize apply the requested geometry to a clone and commit
only when every axis is accepted, so a rejected size never leaves a partial
pattern. Event constructors and `set-slot` validate MIDI ranges (channel 0–15,
data bytes 0–127) before mutating.

## D11. Error diagnostics carry no duplicated prefix

`mlua` renders `RuntimeError` as `runtime error: {message}` and wraps errors
raised by Rust callbacks in `CallbackError`, whose `Display` also includes a
callback traceback. The REPL unwraps `CallbackError` and uses the bare
`RuntimeError` message, and the engine's `compile_error` strips the same prefix.
Diagnostics therefore read `runtime error: unknown setting nope` and
`compile error: repl:1:1: ...`, never `runtime error: runtime error: ...`.

## D12. The HTTP listener bounds every resource a client can consume

A loopback listener is still reachable by a local misbehaving client, so the
server caps request headers at 16 KiB, rejects malformed request lines and
`Content-Length` values, and applies 10 s read and write timeouts. A rejected
connection drains pending bytes before closing so the client receives the JSON
error instead of a connection reset. Request bodies remain capped at 64 KiB and
responses at 1 MiB.

## D13. Numeric API arguments reject silent truncation

`set-bpm`, `set-seed`, `:scale`, and `sysex` bytes validate their ranges instead
of coercing. Whole floats are accepted where Lua turns large literals into
numbers; fractional values are rejected with "must be a whole number". Seeds
above 2^53 must be supplied as hex strings to avoid precision loss.

## D14. Diagnostics never expose Lua internals

Compile and runtime errors are reported as the innermost Fennel diagnostic.
Callback wrappers are unwrapped and any `stack traceback:` suffix is removed, so
clients see the message and position without internal `fennel.lua` paths.

## D15. Timed-out requests are cancelled, not deferred

The HTTP worker waits 35 s for the application loop. If it gives up, it sets the
request's cancellation flag; `drain_requests` skips such requests so a client
that already saw a timeout cannot mutate the project later. The application
loop's own 30 s evaluation timeout keeps a single request well inside the
worker's window.

## D16. `/status` and `/help` extend the plan's four routes

The plan listed `/eval`, `/play`, `/stop`, and `/repl/reset`. Two read-only
routes were added because the original surface made state unobservable:
`/eval` stops playback before evaluating (D6), so `mtrak.project.playing` could
never report `true`. `/status` reads playback and project state without
touching the transport, and `/help` lists the routes and namespaces. Neither
mutates the project. Running a macro is still not possible; that would need a
separate design for macro coroutines and is out of scope for this milestone.

## D17. Read-only discovery helpers never raise

`patterns.exists` and `macros.exists` return booleans instead of raising for
missing IDs, so clients can branch without `pcall`. `settings.keys`,
`events.kinds`, `events.types`, `repl.info`, and `help` expose the surface
without reading source. `macros.validate` compiles a module source and discards
the generated Lua, so it checks syntax and identifiers without storing or
running anything.

## D18. Batch writes are atomic

`patterns.set-slots` applies every requested slot to a clone and commits only
when all indices are valid. A single bad index leaves the pattern untouched,
matching the atomicity rule already applied to `create` and `resize`.
