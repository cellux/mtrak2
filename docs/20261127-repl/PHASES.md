# Implementation phases

## Purpose

This file turns `PLAN.md`, the milestone specification, into an ordered set of
implementable and independently testable phases. A phase is completed once and
is not subsequently rewritten; implementation progress belongs in the
append-only `DIARY.md`, while design decisions that remain in force across
phases belong in `DECISIONS.md`.

## Phase 1 — Compiler support for REPL forms

- Add `FennelCompiler::compile_repl(source, allowed_globals)`.
- Compile with the synthetic source name `repl`, `correlate = true`,
  `unfriendly = true`, and `error-pinpoint = false`.
- Build the `allowedGlobals` Lua table from the supplied strings.
- Share the option-building code with the existing module compiler.

Verification: `fennel::tests` compiles a REPL form, checks the `repl` source
name in diagnostics, and confirms a supplied allowlist is honoured.

## Phase 2 — REPL crate, session, and JSON protocol

- Add `crates/mtrak-repl` with `ReplSession`, `ReplRequest`, `ReplResponse`,
  JSON conversion, and the HTTP listener.
- Create one persistent Lua 5.4 state with the curated standard library and the
  same sandbox restrictions as macro code.
- Install the per-evaluation API through `Lua::scope` so host callbacks can
  borrow the application without `'static` pointers.
- Enforce the 64 KiB request, 1 MiB response, 30 s wall-clock, 50 M
  instruction, and 1 GiB memory limits. JSON conversion has depth and node
  limits and falls back to a printed representation.
- Implement `POST /eval`, `POST /play`, `POST /stop`, and `POST /repl/reset`
  with JSON bodies and stable structured responses.
- Reject unknown routes, malformed JSON, and non-loopback bindings.

Verification: unit tests for JSON round-tripping, request limits, error
responses, session persistence, and local reset; an HTTP integration test that
binds an ephemeral loopback port and exercises all four routes.

## Phase 3 — Pattern and event API

- Add `mtrak.patterns.list/get/create/delete/set-name/set-lpb/resize`.
- Add `mtrak.patterns.get-slot/set-slot`.
- Add structured Lua representations for empty, channel, macro, sysex, meta,
  SMF-escape, and wire events, including inherited fields and slot offsets.
- Add `mtrak.events` constructors for common valid event tables.
- Reuse `Composition` validation for IDs, geometry, timing, and selected-main
  deletion protection.

Verification: pattern lifecycle, slot round-tripping, event validation, and
timing/selected-main error tests.

## Phase 4 — Macro module API

- Add `mtrak.macros.list/get/create/update/delete/replace-form/reload-all`.
- Validate each individual create/update/delete/replacement before committing.
- Reject mutations while the TUI code editor has unsaved changes.
- Re-run setup after module `00` changes; keep built-in modules read-only.
- Implement recursive `replace-form` with exactly-one-match semantics using the
  embedded Fennel parser and source spans.
- Implement `reload-all` as a full build-then-swap operation.

Verification: macro lifecycle, per-operation validation, editor-conflict
rejection, replacement matching/formatting, and reload success/failure tests.

## Phase 5 — Settings and project API

- Add callable `mtrak.settings`, `mtrak.settings.get`, and
  `mtrak.settings.set`.
- Add `mtrak.project` BPM, TPB, seed, selected main, dirty, revision, path,
  pattern list, module list, and playback inspection.
- Add `mtrak.repl.reset` to clear session globals only.

Verification: settings round-trip, project metadata reads/writes, seed
formatting, and reset behaviour tests.

## Phase 6 — Application integration

- Add `--repl` and `--repl-port PORT` CLI parsing, with the default port 6069
  and fatal startup when the port is unavailable.
- Start the HTTP listener off the TUI thread and service requests on the
  application thread through the existing event loop.
- Stop and release playback before `/eval`; keep playback stopped afterwards.
- Implement `/play` validation and start without changing selected-main
  metadata; `/stop` uses the normal stop-and-release path.
- Keep transient TUI state coherent: clear undo history, clamp cursor and
  selection state, refresh the current pattern after deletes, and clear partial
  edits.
- Shut the listener down with the application.

Verification: CLI parsing, occupied-port failure, loopback binding, request
serialization, playback integration, and end-to-end composition workflow tests.

## Phase 7 — Documentation and gates

- Record durable decisions in `DECISIONS.md`.
- Append completed work to `DIARY.md`.
- Run `make verify` and keep the workspace formatting, lint, and test gates
  green.
