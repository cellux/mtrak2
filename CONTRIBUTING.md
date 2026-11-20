# Contributing

## Workflow

Implementation follows the documents under `docs/20261120-v1/`:

- `PLAN.md` — the product specification ("what to build").
- `PHASES.md` — ordered, independently implementable and testable phases.
- `DECISIONS.md` — enduring design choices, each with a stable ID.
- `DIARY.md` — the append-only record of completed work and a `HANDOFF` section.

Before starting a phase, make sure every decision it lists under *Prereq
decisions* is `Accepted` in `DECISIONS.md`. If a phase needs a new decision,
add an `Open` entry with a proposed resolution and, once it is accepted, flip
its status in place; decisions are superseded, never deleted.

## Conventions

- Keep `mtrak-core` and `mtrak-engine` free of backend, terminal, and scripting
  dependencies.
- Prefer typed state over byte conventions; preserve raw bytes for anything the
  model does not understand.
- Every phase acceptance criterion must be covered by an automated test that
  runs without MIDI hardware. Mark genuinely manual checks explicitly.
- Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo test --workspace` before considering a phase complete.

## Recording progress

After completing a phase, append a timestamped entry to `DIARY.md`, update the
phase's `Status` in `PHASES.md`, and keep the `HANDOFF` section current so work
can resume after a context flush.
