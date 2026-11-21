# Design decisions

## Purpose

This file records the design decisions that shape implementation across
multiple phases. Decisions remain in force until explicitly superseded. A
superseded decision is retained and marked as such, with a new decision
recording its replacement. Ordered implementation work belongs in `PHASES.md`,
and completed-work history belongs in `DIARY.md`.

---

### RG-001 — Generic wire events and SMF Escape replace `Event::Raw`

- Status: Accepted
- Decision: `Event::Raw` is removed. Valid live wire-MIDI byte sequences that
  have no specialized typed representation are stored as `Event::Wire(Vec<u8>)`
  and are validated at creation/import. Standard MIDI File Escape events
  (`F7` framing) are stored as `Event::SmfEscape(Vec<u8>)`. Meta-events remain
  typed.
- Consequence: the SMF writer only encodes wire events that have a faithful
  file representation; other wire forms cause save/export to fail with a
  location-specific error. `:event` accepts wire bytes for live MIDI and SMF
  encodings for meta/escape events, and treats exactly three bytes below `0x80`
  as a macro call.
- Source: `PLAN.md` §2.1, §3.4.

### RG-002 — Settings are composition-global only

- Status: Accepted
- Supersedes: v1 D-010 (live settings inheritance).
- Decision: the pattern-local settings layer is removed. A single concrete
  `Settings` value holds root, scale, modal degree, and scale-aware mode for the
  whole composition. `:root`, `:scale`, and `:mode` modify the global value.
- Consequence: `Composition::effective_settings` no longer resolves a layer;
  macro setting changes are global-only.

### RG-003 — One ID-keyed module source map

- Status: Accepted
- Decision: `TrackerProject` stores authored modules in one `BTreeMap<u8,
  String>` containing module `00` (the editable setup/library module) and user
  modules `40`–`7F`. The separate `setup_source` field is removed. Built-in
  modules `01`–`3F` are embedded from `crates/mtrak-core/src/builtins/NN.lua`
  via `include_str!` and are never serialized.
- Consequence: module `00` is always present in new and imported projects; user
  modules are created lazily from the minimal template.

### RG-004 — Numeric-hex `require` in the Lua sandbox

- Status: Accepted
- Decision: every module environment exposes `require(0xNN)`, which loads and
  caches the module's returned value (any Lua value, not just tables), reports
  unavailable IDs and circular dependencies as deterministic errors, and shares
  the existing sandbox and resource limits. The runtime holds no `RefCell`
  borrow across Lua evaluation so `require` can re-enter module loading.
- Source: `PLAN.md` §7.2.

### RG-005 — Built-in macros are Lua modules only

- Status: Accepted (amended 2026-11-21T11:30:00Z)
- Decision: built-in macros `01` (pattern trigger) and `02` (Q8.8 tempo
  shorthand) are inspectable, read-only Lua modules and have **no** Rust
  implementation. A caller-installed factory runs them when it provides them;
  otherwise the engine lazily creates its own built-in `LuaRuntime`. Lua is a
  mandatory dependency (`mlua` vendored 5.4); there is no lua-less build.
- Amendment: the original decision kept a native Rust fallback for lua-less
  builds. That fallback was removed so the Lua modules are the single source of
  truth; the fallback runtime is Lua, not Rust.
- Source: `PLAN.md` §7.2.

### RG-006 — Per-item undo and separate structural histories

- Status: Accepted
- Decision: pattern content edits are recorded in a per-pattern-ID history and
  module text edits in a per-module-ID history. The Pattern and Code choosers
  each have a separate structural history; deleting or replacing an item
  discards its content history. Pattern and Code histories never cross.
- Source: `PLAN.md` §8.

### RG-007 — The Code editor and Lua highlighter are built in

- Status: Accepted
- Decision: the multiline Lua editor and its lexical highlighter live in
  `mtrak-tui` (`code.rs`) rather than in an external crate. The Lua-capable
  ratatui editor crates (`edtui`, `tui-syntax-highlight`, `tui-code-editor`)
  target `ratatui-core 0.1`/`tree-sitter` and would force a ratatui 0.29+
  upgrade that conflicts with the pinned Rust 1.85 MSRV; the only compatible
  crate (`tui-textarea` 0.6) has no Lua highlighting and would pull ratatui into
  the terminal-independent app state.
- Source: `PLAN.md` §7.1.
