# Design decisions

## Purpose

This file records the design decisions that shape the implementation of the
milestones in `PLAN.md`. Decisions remain in force until explicitly superseded;
superseded decisions are retained and linked to their replacements. Ordered
work belongs in `PHASES.md`, and completed-work history belongs in `DIARY.md`.

---

### FB-001 — The browser is a real workspace and owns process cwd

- Status: Accepted
- Decision: F2 is a `Screen::Browser` workspace, not a modal overlay. Its
  directory is the process current directory while it is active and after
  leaving it until another screen changes cwd.
- Consequence: `:r`, `:w`, and completion resolve relative paths from the
  displayed browser directory. Reopening F2 never resets to the loaded file's
  parent after first initialization.
- Source: `PLAN.md` §3.1, §3.2, §3.7.

### FB-002 — Selection is identified by entry name, not a numeric index

- Status: Accepted
- Decision: the browser stores the selected entry name as its identity. A
  parent-return selection is looked up by the recorded child entry name.
- Consequence: refreshing a directory or changing filter cannot leave a stale
  numeric index pointing at a different row; a vanished remembered name falls
  back to the first visible entry.
- Source: `PLAN.md` §3.4, §4.

### FB-003 — Filter matching is character-based with a simple case rule

- Status: Accepted
- Decision: an entry matches when its name contains the filter. Matching is
  case-insensitive unless the filter contains at least one uppercase character,
  in which case it is exact. Match spans are character offsets and are
  non-overlapping.
- Consequence: digits and punctuation do not force case sensitivity, repeated
  matches are reported in display order, and Unicode names are highlighted
  without byte-offset slicing.
- Source: `PLAN.md` §3.5, §5.

### FB-004 — Symlinks follow only for classification; broken links stay visible

- Status: Accepted
- Decision: a symlink resolving to a directory acts as a directory, one
  resolving to a regular file acts as a file, and a broken/unreadable symlink is
  an `Unreadable` entry. Only files whose first four bytes are exactly `MThd`
  may be opened.
- Consequence: broken links remain visible but Enter and Right are no-ops; no
  full MIDI parse happens just to decide whether to attempt loading.
- Source: `PLAN.md` §3.3, §3.6.

### FB-005 — Paths are canonicalized at file-operation boundaries

- Status: Accepted
- Decision: successful loads store the canonical absolute file path, set cwd to
  its parent, and switch to Pattern. Saves store a canonical absolute path,
  canonicalizing the existing parent of a new relative file and never creating
  directories implicitly.
- Consequence: plain `:w` remains anchored even after the browser moves, and
  missing/inaccessible parents or cwd/canonicalization failures surface as
  status errors rather than silent state changes.
- Source: `PLAN.md` §3.2, §3.6, §3.7.

### FB-006 — Command mode keeps its existing modal precedence

- Status: Accepted
- Decision: `:` opens command mode only when the browser filter is empty; with a
  non-empty filter it is an ordinary printable character. Esc clears a
  non-empty filter before anything else can interpret it, and command-mode keys
  are never captured by the browser filter.
- Consequence: the browser reuses the existing line editor, completion menu,
  error handling, and return-to-previous-workspace behavior without a second
  command implementation.
- Source: `PLAN.md` §3.5, §3.7.
