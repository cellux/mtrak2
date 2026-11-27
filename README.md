# mtrak

A MIDI tracker implemented in Rust. The v1 design is specified in
[`docs/20261120-v1/PLAN.md`](docs/20261120-v1/PLAN.md); the ordered
implementation work lives in
[`docs/20261120-v1/PHASES.md`](docs/20261120-v1/PHASES.md) and the enduring
choices in [`docs/20261120-v1/DECISIONS.md`](docs/20261120-v1/DECISIONS.md).
The follow-up milestone that closes the remaining v1 gaps (nibble editing,
offset editing, workspaces, the chooser, the Code screen, and the module
workflow) lives in
[`docs/20261120-remaining-gaps/PLAN.md`](docs/20261120-remaining-gaps/PLAN.md).
The milestone that migrates macro authoring from Lua source to Fennel source
(while keeping Lua 5.4 as the execution VM) lives in
[`docs/20261127-fennel/PLAN.md`](docs/20261127-fennel/PLAN.md).

## Workspace layout

| Crate | Role |
|-------|------|
| `mtrak-core` | Composition model, typed events, timing, and global settings. No backend or terminal dependencies. |
| `mtrak-engine` | Deterministic scheduler and sandboxed Fennel macro runtime. |
| `mtrak-midi-io` | Standard MIDI File import/export and tracker project metadata. |
| `mtrak-midi` | Portable MIDI backend abstraction. |
| `mtrak-tui` | Terminal user interface and command layer. |
| `mtrak` | The application binary. |

Dependencies point inward toward `mtrak-core`. The core and engine crates never
depend on `midir`, `midly`, or a terminal library.

## Building and testing

The workspace targets Rust 1.85 (2021 edition) and is pinned by
`rust-toolchain.toml`.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

A `Makefile` wraps the common commands; `make` (or `make help`) lists them:
`make build`, `make release`, `make test`, `make verify`, and more.

Real-time MIDI output is gated behind the `midir-backend` feature of
`mtrak-midi`, so the rest of the workspace builds without a system MIDI stack.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the phase/decision workflow.

## Running

```sh
cargo run -p mtrak -- [FILE]
```

`FILE` is either a tracker project or a standard MIDI file to import. Macros
are authored in Fennel and compiled to Lua; the engine embeds the standalone
Fennel compiler and always compiles the vendored Lua 5.4 execution runtime, so
normal builds need neither a network connection nor an installed Fennel
command. Real-time MIDI output is gated behind the `midir` feature, which is
enabled by default; build with `--no-default-features` for a portable build
with no system MIDI stack.

The checked-in compiler asset is refreshed with
`make update-fennel FENNEL_VERSION=1.6.1`.

## Tests and fixtures

`cargo test --workspace` runs unit tests plus the end-to-end suite in
`crates/mtrak/tests/e2e.rs`, which exercises the `PLAN.md` §5 targets without
MIDI hardware. MIDI fixtures are committed under
`crates/mtrak/tests/fixtures/` and embedded into the tests.
