# mtrak F2 file browser and workspace-key shift

## Purpose

This document specifies the implementation of a filesystem browser on F2 and
moves the existing Pattern and Code workspaces to F3 and F4. It is the
milestone specification for the feature. Ordered implementation work,
enduring design decisions, and completed-work history will be recorded
separately in `PHASES.md`, `DECISIONS.md`, and `DIARY.md` after the plan has
been reviewed and accepted.

## 1. Goals

Implement a keyboard-driven F2 file and directory browser that lets the user:

- navigate the filesystem hierarchy;
- filter the current directory by a name substring while typing;
- select and open Standard MIDI Files;
- use the existing colon command line, including `:w`, from the browser;
- keep relative file commands anchored to the browser's current directory;
- preserve the existing Pattern and Code workflows under F3 and F4.

Opening a MIDI file must use the existing project/MIDI loader, switch to the
Pattern screen, and make the loaded file's parent directory the process
current directory. The browser must not add a separate text-file or Lua-file
loading workflow: Lua source remains embedded in the MIDI/project file.

The F1 help screen remains otherwise unchanged; its contents will be populated
later.

## 2. Current implementation context

The application is a Rust workspace. The relevant TUI code is in
`crates/mtrak-tui`:

- `app.rs` owns `Screen`, application state, screen switching, command
  execution, project loading/saving, and key dispatch;
- `key.rs` maps function keys and common actions independently of the terminal;
- `ui.rs` renders the header, screen body, status line, command line, and
  completion popup;
- `complete.rs` implements filesystem completion for `:r`/`:w`;
- `path.rs` expands leading `~` paths;
- `command.rs` parses `:read`/`:r`, `:write`/`:w`, and other commands;
- `crates/mtrak/src/main.rs` loads an optional initial path before starting the
  TUI.

The current screen map is Help/F1, Pattern/F2, and Code/F3. `App::load` and
`App::save` already perform the substantive MIDI/project operations and keep a
single project path. Path completion lists entries from the process current
working directory (or a typed directory), appends `/` to directories, and
supports an interactive completion menu. The browser should reuse the same
filesystem/path conventions where appropriate, but it has distinct filtering
and navigation behavior.

## 3. Functional requirements

### 3.1 Workspace key mapping

Change the top-level screen map to:

- F1 — Help;
- F2 — File browser;
- F3 — Pattern;
- F4 — Code.

Update the `Screen` enum, key lookup/dispatch, rendering matches, header/status
labels, and affected tests. Pattern and Code behavior must otherwise remain
unchanged. F1 help text is not expanded as part of this milestone.

F2 must be a real workspace, not a modal overlay. Command mode remains modal
over whichever workspace is active.

### 3.2 Browser location and process current directory

The browser displays an absolute path for its current directory at the top of
the browser view.

When the browser is first initialized:

- if a project/file is already loaded, use the parent directory of its
  canonical absolute path;
- otherwise use the process current directory.

The initial browser directory is also installed as the process current
directory, so relative commands immediately resolve from the displayed
location.

Once initialized, reopening F2 in the same process preserves the browser's
current directory, selected entry, and filter rather than resetting them to the
loaded file's parent or clearing the filter. On the first initialization, if a
project is loaded, select the loaded project's filename; otherwise select the
first entry in normal listing order.

Whenever the browser changes directory, call `std::env::set_current_dir` so the
process current directory follows the browser. This is intentional: relative
paths entered into `:r`, `:w`, and related file commands must resolve from the
browser's current directory. Leaving F2 for another workspace does not restore
the previous cwd; the browser's last directory remains the process cwd.

Loading a MIDI file from the browser, `:r`, or the command-line startup path
must change the process current directory to that file's parent after
successful loading. The loaded project path must be stored as a canonical
absolute path.

Saving a MIDI/project file must also remember a canonical absolute path. For a
new relative output path that does not yet exist, resolve it against the
current process directory and canonicalize its existing parent before storing
the final absolute path. A missing or inaccessible parent is an error; saving
must not create directories implicitly. Errors changing directory or
canonicalizing paths must be reported in the existing status/error channel
rather than silently changing state.

Changing the process current directory is global process state. Tests that
exercise it must restore the original directory and avoid leaking temporary
working-directory changes into other tests.

### 3.3 Directory listing

The browser lists every directory entry in the current directory, including
hidden entries. It does not hide or extension-filter non-MIDI files.

Entries are sorted with directories before non-directories, and alphabetically
within each group. Directories are rendered with a trailing `/` (or another
clear directory indicator consistent with the TUI). Symlink behavior must be explicit and safe: a symlink to a directory is
shown as a directory and can be entered; a symlink to a file is shown as a file
and can be opened only when its target begins with `MThd`; unreadable or broken
symlinks remain visible but cannot be entered or opened.

The browser maintains a selected entry. Up/Down move that selection through the
currently visible list and clamp at its ends. An empty directory or a filter
with no matches has no selectable entry.

### 3.4 Directory navigation and history

- Right Arrow on a directory enters it.
- Enter on a directory also enters it.
- Left Arrow moves to the parent directory, unless already at the filesystem
  root.
- Enter on a regular file attempts to open it only if its first four bytes are
  exactly `MThd`.
- Enter on a non-MIDI file is a no-op: the browser remains open, the selection
  does not change, and no project state is mutated.
- Right Arrow on a non-directory/non-MIDI file is a no-op.

Entering a directory clears the search filter and selects the first visible
entry in the new directory. The browser remembers the directory entry used to
enter each child directory. When Left returns to a parent, that child entry is
selected; pressing Right again therefore re-enters the directory just left.
The remembered relationship is updated as navigation proceeds and must not
cause stale selection indices after directory contents change.

Directory changes refresh the listing and update the process current directory.
Filesystem read, metadata, or directory-change failures remain in the browser
and are reported through the bottom status line.

### 3.5 Search/filter behavior

The browser has an implicit filter editor while F2 is active; there is no
separate search command required to start typing.

- Printable characters append to the filter.
- Backspace removes the last filter character.
- Alt+Backspace removes the last word, using the same word-oriented behavior
  expected by the existing line editor.
- The filter applies to both files and directories.
- An entry matches when its name contains the filter pattern.
- Matching is case-insensitive unless the filter contains at least one
  uppercase character; a filter containing an uppercase character is
  case-sensitive.
- An empty pattern shows all entries.
- Matching substrings are visibly highlighted in every displayed filename.
- Search results continue to include hidden entries.
- Changing directory clears the filter.

Arrow keys retain their browser navigation meanings and are not inserted into
the filter. When the filter is non-empty, Esc clears it and remains in F2.
The filter itself need not be printed in the status line because its matches
are highlighted in the listing. The bottom line remains available for status
messages and the colon command line.

### 3.6 MIDI detection and loading

The browser classifies a regular file as MIDI-openable by reading only its
first four bytes and checking for the exact `MThd` signature. The browser does
not need to parse the full file merely to decide whether Enter should attempt
to open it. If the file cannot be read or has fewer than four bytes, Enter is a
silent no-op just like Enter on any other non-MIDI file.

For an `MThd` file, invoke the existing `App::load` path. Full SMF/project
validation remains the loader's responsibility. If loading fails after the
header check, preserve the browser/project state as far as the existing load
contract permits and show the error in the bottom status line; do not treat the
file as a text or Lua source file.

After successful browser or `:r` loading:

- store the canonical absolute file path;
- set the process current directory to the file's parent;
- switch to the Pattern screen (F3);
- preserve the existing project/Lua initialization and reset behavior of
  `App::load`.

### 3.7 Command line in the browser

The existing `:` command entry and line editor must be available while F2 is
active. The command line is rendered at the bottom in the same way as on the
other workspaces, including existing errors and path completion behavior.

In particular:

- `:w filename.mid` resolves a relative filename against the browser's current
  directory and saves the current project there;
- plain `:w` continues to save to the project's existing canonical absolute
  path;
- `:r path` continues to use the existing loader, and on success also changes
  cwd and switches to Pattern;
- command mode returns to the browser after a successful command that does not
  change the loaded workspace;
- browser-specific printable-key filtering must not intercept keys while
  command mode is active.

The existing completion rules for `:r`/`:w` remain in force: relative paths
use cwd, `~` expansion remains supported, directories receive a trailing `/`,
and ambiguous candidates can be selected from the completion popup. Plain `:`
opens command mode only when the browser filter is empty; with a non-empty
filter, `:` is treated as another printable filter character. Esc must first
clear the filter before command mode can be entered.

### 3.8 Rendering

Render F2 as a full-screen browser workspace with:

- the absolute current-directory path at the top of the browser panel;
- one row per visible directory entry;
- a clear selected-row style;
- directory indicators;
- visible highlighting of every substring that matched the current filter;
- the existing bottom status line or command line.

The implementation must handle narrow terminals, long paths, long filenames,
empty directories, no-match filters, and directories with more entries than
fit on screen through scrolling/clamping rather than panicking.

## 4. State and module design

Keep filesystem behavior testable independently of ratatui rendering. Add a
small browser state module under `crates/mtrak-tui/src` (for example
`browser.rs`) containing:

- the current absolute directory;
- the current filter text;
- the selected entry identity (prefer a name/path identity over a fragile
  numeric index);
- the visible/sorted entry list or a refreshable representation;
- the parent/child navigation memory needed to restore the child selection;
- pure helpers for sorting, matching, case-mode selection, and match-span
  calculation.

`App` owns this state and coordinates filesystem effects, project loading,
workspace switching, command mode, and status messages. The browser module
must not depend on ratatui. Use the existing `Code`, `Key`, and `LineEditor`
abstractions for input behavior rather than introducing terminal-specific key
handling.

The browser should share or factor common path-resolution logic with
`complete.rs`/`path.rs` where doing so does not change their established
behavior. Do not make path completion depend on the browser's filter state.

## 5. Validation and tests

Add or update unit tests for:

- F1/F2/F3/F4 key mappings and screen switching;
- first-time browser directory/selection initialization, startup loading, and
  subsequent F2 reopening with directory, selection, and filter preserved;
- directory-first alphabetical sorting, hidden-entry inclusion, and directory
  indicators;
- substring filtering for files and directories;
- case-insensitive matching when the filter has no uppercase characters,
  versus case-sensitive matching when it contains at least one uppercase
  character, including filters containing digits or punctuation;
- match-span/highlight calculation, including repeated matches and Unicode
  filenames where supported by the chosen implementation;
- filter editing, Backspace, Alt+Backspace, and clearing on directory entry;
- selection clamping, empty directories, and no-match results;
- right/Enter directory entry and left-parent navigation;
- remembered child-directory navigation after stepping upward;
- `MThd` detection, unreadable/short/non-MIDI silent Enter no-op behavior, and
  directory Enter/right behavior;
- successful MIDI open switching to Pattern and updating cwd/path;
- `:r` from the browser, including cwd/path/screen changes;
- `:w filename` from the browser and plain `:w` after cwd changes;
- canonical absolute path handling for existing and newly written files;
- filesystem failure messages and malformed MIDI load errors;
- browser rendering under narrow, empty, long-name, and long-list fixtures.

Use temporary directories and restore the process current directory in every
cwd-sensitive test. Preserve hardware independence. Run the full existing
verification suite after implementation:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 6. Completion criteria

The milestone is complete when F2 provides the specified browser workflow,
F3 and F4 provide the existing Pattern and Code workspaces, all file commands
work from the browser's current directory, MIDI opening follows the `MThd`
rule and switches to Pattern, and the full workspace checks pass without
regressions.
