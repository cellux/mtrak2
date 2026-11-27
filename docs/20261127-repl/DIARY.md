# Implementation diary

## Purpose

This file is the append-only, timestamped log of completed implementation
work. Earlier entries must never be edited. Pending work is maintained in one
`HANDOFF` section at the end of the file and updated before every context
flush so another agent can resume accurately. Ordered work belongs in
`PHASES.md`, and durable design choices belong in `DECISIONS.md`.

## 2026-11-27 — Phases 1–6 implemented

### Phase 1 — Compiler support for REPL forms

- Added `FennelCompiler::compile_repl(source, allowed_globals)` in
  `crates/mtrak-engine/src/fennel.rs`, sharing option building with the module
  compiler through `compile_with`.
- Added `FennelCompiler::replace_form` and `value_to_source`, backed by an
  embedded Lua `REPLACE_FORM_SCRIPT` that uses the Fennel parser and source
  spans. The script matches exactly one AST form and preserves all source
  outside the matched span.
- Tests in `fennel::tests`: REPL source name, allowlist enforcement,
  `replace-form` source preservation, and zero/multiple-match diagnostics.

### Phase 2 — REPL crate, session, and JSON protocol

- Added `crates/mtrak-repl` with `ReplSession`, `ReplRequest`, `ReplResponse`,
  JSON conversion, and the HTTP listener.
- `ReplSession` owns one persistent Lua 5.4 state with a 1 GiB memory limit, a
  30 s wall clock, and a 50 M instruction budget. A persistent `globals` table
  holds session globals; each evaluation installs a fresh API table whose
  `__index` is `globals`.
- `json.rs` converts Lua values to JSON with depth (32) and node (10 000)
  limits and falls back to a printed representation for opaque values.
- `http.rs` implements a minimal localhost-only HTTP/1.1 server with
  `POST /eval`, `POST /play`, `POST /stop`, and `POST /repl/reset`. Requests
  are capped at 64 KiB and responses at 1 MiB. Unknown routes and malformed
  bodies return structured errors.
- Tests: `crates/mtrak-repl/tests/http.rs` covers all four routes, structured
  compile errors, unknown routes, malformed JSON, oversized requests, and
  invalid `/play` points.

### Phase 3 — Pattern and event API

- `mtrak.patterns.list/get/create/delete/set-name/set-lpb/resize` and
  `get-slot/set-slot` are installed in `api.rs`.
- `events.rs` converts empty, channel, macro, sysex, meta, SMF-escape, and wire
  events to structured Lua tables and back, including inherited fields and slot
  offsets. `mtrak.events` provides constructors for empty, note-on, note-off,
  control-change, program-change, sysex, and macro events.
- Pattern operations reuse `Composition` validation for IDs, geometry, timing,
  and selected-main deletion protection.

### Phase 4 — Macro module API

- `mtrak.macros.list/get/create/update/delete/replace-form/reload-all` are
  installed in `api.rs`.
- Each create/update/delete/replacement validates the changed module before
  committing. Module `00` cannot be deleted; built-in modules are read-only.
- `replace-form` accepts string prefix and replacement forms and delegates to
  the embedded Fennel parser. `reload-all` rebuilds and swaps the runtime only
  after complete validation.
- Macro edits reject unsaved code-editor buffers through
  `App::repl_editor_conflict`.

### Phase 5 — Settings and project API

- Callable `mtrak.settings` with `__call`, `get`, and `set`.
- `mtrak.project` exposes BPM, TPB, seed, selected main, dirty, revision,
  path, pattern list, module list, and playback state. No TPB-changing API is
  exposed.
- `mtrak.repl.reset` clears session globals only.

### Phase 6 — Application integration

- `crates/mtrak/src/main.rs` parses `--repl`, `--repl-port PORT`, and `FILE`,
  with a default port of 6069. `--repl-port` without `--repl` is an argument
  error.
- `start_repl` binds to `127.0.0.1` and starts `ReplService`. `run_app` drives
  the TUI loop with `ui::run_with`, draining REPL requests each frame.
- `App` gained `repl_*` helpers for project mutation, dirty/revision updates,
  undo clearing, playback stop/start, pattern-change cleanup, editor conflict
  detection, module source application, module deletion, and `reload-all`.
- `ui::run_with` calls the request drainer once per frame.

### Tests and gates

- `cargo test --workspace` passes: 22 `mtrak-repl` unit tests, 9 HTTP
  integration tests, 9 CLI parsing tests, plus the existing workspace suites.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- `cargo fmt --all -- --check` is clean.

## 2026-11-27 — Acceptance-criteria tests added

Added the following `mtrak-repl` unit tests on top of the existing suite:

- `editor_conflict_rejects_macro_change` — unsaved code-editor buffers reject
  macro updates.
- `local_scope_does_not_persist` — `local` bindings stay scoped to one form.
- `sandbox_blocks_io_os_debug` — `io`, `os`, and `debug` are unavailable.
- `callable_settings_reads` — `(mtrak.settings :root)` works.
- `selected_main_deletion_is_protected` — deleting the main pattern errors.
- `invalid_source_preserves_module` — a failed update leaves the old source in
  place.
- `reload_all_failure_preserves_runtime` — a failed full reload leaves the
  previous runtime usable and recovers after the source is fixed.
- `dirty_state_is_set_by_repl_mutation` — a REPL mutation marks the project
  dirty.
- `end_to_end_workflow` — create pattern, populate a slot, create a macro,
  structurally edit it, and finish with `reload-all`.

The suite now has 31 `mtrak-repl` unit tests plus 9 HTTP integration tests and
9 CLI parsing tests. `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` all pass.

## 2026-11-27 — More acceptance-criteria tests and a bug fix

Added tests for:

- built-in module read-only enforcement (also fixed `update`, `delete`, and
  `replace-form` to reject module IDs below `0x40`);
- invalid pattern operations (occupied ID and zero LPB);
- absence of a TPB-changing API;
- `mtrak.repl.reset` clearing session globals;
- `/eval` stopping playback through `handle_request`;
- occupied-port startup failure and loopback binding.

The suite now has 36 `mtrak-repl` unit tests, 9 HTTP integration tests, and 11
CLI parsing tests. `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` all pass.

## 2026-11-27 — Live API sweep and bug-fix round

Exercised the running REPL against a release build with `--repl` and swept the
whole documented surface: patterns, macros, settings, project, events, the
sandbox, HTTP routes, and the resource limits. Eight defects surfaced; all are
fixed and covered by regression tests.

1. **Pattern mutations did not bump `project_revision` or clear undo.**
   `repl_after_pattern_change` only set `dirty`, contradicting D7. It now also
   calls `repl_mark_mutated`.
2. **`replace-form` could not match table or map literals.** Fennel gives every
   table literal a unique metatable, so the AST comparison bailed out. It now
   compares sequence-ness instead of metatable identity and matches tables
   structurally.
3. **`replace-form` crashed on sequence literals.** Sequences carry no
   `bytestart`/`byteend`, so the replacement arithmetic hit `nil`. Sequence
   matches now return a clear "no source position metadata" diagnostic.
4. **A non-string `replace-form` argument killed the application.** Passing a
   Lua table from the REPL state into the compiler state panicked with "Lua
   instance passed Value created from a different main Lua state". The API now
   accepts source strings only and rejects anything else.
5. **`stop-track` produced a raw Lua error** instead of the friendly sandbox
   message, because the rejected-global handler was installed under the
   unmangled name only. Rejected globals are now installed under both names.
6. **Hyphenated session globals could not be called** after definition. The
   allowlist now unmangles `__fnl_global__...` keys before compiling.
7. **Timeout and instruction-limit errors reported `kind: "runtime"`.** The
   hook now records the limit beside the raise so the session reports
   `timeout`/`instruction_limit`.
8. **Deleting the selected main pattern reported "does not exist".** The API
   now checks the selected main first and reports that it cannot be deleted.

The dead `FennelCompiler::value_to_source` helper was removed; it was the
cross-state call that caused defect 4.


### Second live sweep (continued)

A second pass over the rebuilt server found five more defects:

9. **Literal prefixes gave a misleading error.** `replace-form` with a number or
   string prefix reported "no matching form" because the walker only descends
   into tables. Literal prefixes now return "prefix literal cannot be located"
   instead of pretending nothing matched.
10. **Module `00` was treated as read-only.** The plan makes `00` the writable
   setup module and only `01`–`3F` read-only. The range check now allows `00`
   and rejects only built-ins; `create` still requires `40`–`7F`.
11. **Event constructors accepted out-of-range MIDI values.** `note-on` with
   channel 200 produced an event that silently fails to encode. Constructors and
   `set-slot` now reject channel > 15 and data bytes > 127.
12. **`patterns.create` was not atomic.** A rejected geometry left a
   half-created pattern behind. Creation now validates the geometry on a clone
   and deletes the pattern when the size is rejected.
13. **`patterns.resize` was not atomic.** `{:rows 32 :tracks 0}` applied the row
   change before failing on tracks. Both axes are now applied to a clone and
   committed only when both succeed.

## HANDOFF

### Current state

All seven phases are implemented. The workspace builds and the full test suite
passes. The REPL crate lives in `crates/mtrak-repl`; engine changes are in
`crates/mtrak-engine/src/fennel.rs`; TUI helpers are in
`crates/mtrak-tui/src/app.rs`; CLI and application wiring are in
`crates/mtrak/src/main.rs`.


### Third live sweep (bugs 14-26)

Continuing against a restarted release build, the following were found and
fixed:

- `/play` silently ignored non-numeric, out-of-range, and malformed bodies;
  it now rejects them with a protocol error (#14).
- `patterns.create` ignored invalid geometry options; it now rejects
  non-integer and out-of-range values (#15).
- `:scale` silently truncated masks above `0xFFFF`; it now rejects values
  outside `0..=65535` (#16).
- A malformed request line reported "only POST requests are supported"; it now
  reports "malformed request line" (#17).
- Request headers were unbounded; `MAX_HEADER_BYTES` (16 KiB) now caps them
  (#18).
- No socket read/write timeout existed, so a half-open connection pinned a
  worker thread forever; both are now 10 s (#19).
- A malformed `Content-Length` was silently treated as zero; it is now rejected
  (#20).
- `mlua::RuntimeError` messages were prefixed twice ("runtime error: runtime
  error: ..."); the callback wrapper is now unwrapped and the bare message kept
  (#21).
- Compiler diagnostics carried the same redundant prefix ("compile error:
  runtime error: ..."); `compile_error` now strips it (#22).
- Rejected requests left unread bytes in the socket, so clients saw a
  connection reset instead of the error body; rejected connections now drain
  before closing (#23).
- `{"form": 1}` reported "missing form field"; non-string and empty values now
  get precise diagnostics (#24).

Two observations were left as designed behavior:

- `mtrak.project.playing` always reads `false`, because `/eval` stops playback
  first (plan requirement, D6).
- `mtrak.macros.get` returns `nil` for an absent module while
  `mtrak.patterns.get` errors; the asymmetry is intentional (`nil` means
  "absent" for modules).

Verified in the same sweep: raw event tables, concurrent `/eval` (12 parallel
requests, serialized correctly), 3 000-term forms, 20 000-deep nesting without a
crash, cyclic-table JSON fallback, the 1 MiB response cap, and `/stop` while
idle.


### Fourth live sweep (bugs 25-28)

Against a release build carrying the third batch, the following were found and
fixed:

- A truncated request body surfaced as a raw OS error (`io error: Resource
  temporarily unavailable`); it now reports "request body incomplete" (#25).
- `mtrak.events.sysex` silently truncated out-of-range bytes (`300` became
  `240`, `-1` became `255`); bytes are now validated against `0..=255` (#26).
- `Transfer-Encoding: chunked` was silently mis-parsed as an empty body; it is
  now rejected with "unsupported transfer-encoding" (#27).
- `set-bpm` and `set-seed` silently truncated fractional numbers (`1.9` became
  `1`); both now reject non-whole numbers, and seeds above 2^53 point at hex
  strings (#28).

The read timeout was lowered from 10 s to 2 s so an incomplete body is reported
promptly.

Verified in the same sweep: event-table round trips and defaults, atomic
`create`/`resize` failures, module `00` update, `reload-all` after module
changes, invalid-source preservation on `update`, "no matching form" for
`replace-form`, built-in/user module ID rules, session persistence of tables and
closures, sandbox rejection of `os`/`coroutine`/`loadstring`/`setmetatable`/
`rawget`, multi-form input returning the last value, JSON fallbacks for
functions and non-UTF-8 strings, and the 10 000-node JSON limit.


### Fifth live sweep (bugs 29-30)

Against a release build carrying the fourth batch:

- Fennel parse errors carried a full Lua stack traceback, exposing internal
  `fennel.lua` paths. `error_message` now unwraps the callback chain and strips
  the traceback, so diagnostics end at the parse position (#29).
- Pattern and module arguments silently truncated fractional numbers
  (`get-slot 0 1.9 0` read row 1, `create {:lpb 1.9}` made an LPB-1 pattern).
  Every ID, row, track, LPB, offset, and geometry argument now validates
  integrality and range (#30).

Verified in the same sweep: `sysex`/BPM/seed range rejection, `Transfer-Encoding:
identity` acceptance, event defaults, atomic create/resize, module `00` update,
`reload-all` after module changes, invalid-source preservation, "no matching
form", sandbox rejections, multi-form evaluation returning the last value, and
JSON node/response limits.


### Sixth live sweep (bug 31)

Verified fixes 29 and 30 against a restarted release build: parse errors now end
at the diagnostic position with no traceback, and fractional IDs/rows/LPBs are
rejected with "must be a whole number".

One more bug was found by reading the dispatch path:

- A request whose HTTP worker hit the 35 s response timeout was still executed
  by the application loop afterwards, so a client that had already seen a
  timeout could still mutate the project. `ReplRequest` now carries a
  cancellation flag that the worker sets on timeout; `drain_requests` skips
  cancelled requests (#31).

Sweep complete. The REPL surface has been exercised end to end: every pattern,
macro, settings, project, event, and repl operation; the sandbox allowlist; the
HTTP protocol layer; JSON limits; and session persistence.

### Remaining work

The following plan acceptance-criteria tests are still not covered:

- request timeouts, memory-limit diagnostics, and the 1 MiB response limit (the timeout and instruction-limit kinds are now covered);
- timing errors beyond zero LPB and invalid-ID diagnostics beyond the
  occupied-pattern case;
- undo-clearing assertions and TUI selection/transient-edit cleanup;
- `/play` validation and start behavior without MIDI hardware.

### Notes

- Fennel 5.4 `quote` cannot be used at runtime; `replace-form` takes strings
  (decision D4).
- `repl_set_editor_module` is a test-only helper on `App` for editor-conflict
  coverage.
- `make verify` runs `fmt-check`, `clippy`, and `test` and covers the REPL
  crate automatically.
