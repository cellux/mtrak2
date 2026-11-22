# mtrak Rust v1 implementation plan

## 1. Purpose and goals

Build a new Rust implementation of the MIDI tracker concept represented by the Go prototype, using the prototype as inspiration rather than as a design or compatibility contract.

The v1 implementation should prioritize:

- a clean, reusable engine/library architecture;
- reliable and deterministic scheduling;
- a terminal UI with a better event-editing workflow;
- standard MIDI import;
- two kinds of MIDI output:
  - tracker project save, preserving tracker-specific structure in standards-valid MIDI sequencer-specific meta-events;
  - standard MIDI export, rendering tracker-specific behavior into ordinary MIDI events;
- user-authored, sandboxed Lua macros;
- multiple reusable patterns and per-pattern timing;
- real-time MIDI output, with MIDI input architecture prepared for future recording;
- Linux-first operation while keeping the MIDI backend abstraction portable.

Feature parity with the Go version is not required. The Go source is a feature reference and should be mined for useful behavior, not copied blindly.

## 2. Agreed product model

### 2.1 Composition and patterns

- A composition contains up to 256 stable numeric pattern IDs (`00`–`FF`).
- Pattern IDs are stable slots; deleting a pattern leaves its ID unused rather than renumbering other patterns.
- A composition may have multiple possible main/root patterns. Playback and export begin from the pattern explicitly selected by the user.
- A pattern contains any number of tracks and `N` event rows/slots.
- Every track in a pattern shares the pattern's row grid and timing.
- One slot contains one event. Chords may use multiple tracks, or a single event may invoke a chord-producing Lua macro.
- A pattern has its own LPB (lines per beat).
- BPM and TPB (ticks per beat) are composition-wide.
- TPB is the global scheduler/MIDI resolution. Each pattern's LPB must divide TPB exactly, and a pattern row lasts `TPB / LPB` global ticks.
- Lua coroutines and all concurrent patterns share the same global TPB tick timeline; `wait(n)` counts beats.
- A pattern stops when it reaches its final row in v1; it does not loop automatically.
- A pattern-triggered child runs concurrently with its parent and has its own LPB.

### 2.2 Track-local pattern ownership

Pattern triggering uses track-local ownership:

- A trigger event on a parent track starts a child pattern owned by that track.
- The child pattern's own tracks execute concurrently and do not replace the parent's other tracks.
- A later trigger on that parent track replaces the previously owned child pattern instance and its descendants.
- The parent pattern continues running.
- Patterns and child instances are stopped at their natural end in v1.

Lua coroutine lifetime is intentionally more flexible than pattern ownership:

- triggering a replacement does not automatically cancel already-running macro coroutines;
- a macro can explicitly call `stop_track(include_current)`;
- `stop_track(true)` cancels all coroutines and child instances associated with the invoking track, including the caller;
- `stop_track(false)` cancels the other coroutines and child instances on that track while keeping the caller alive.

The scheduler must define cancellation behavior precisely and test it, especially for descendants and coroutines that continue after their pattern instance is replaced.

### 2.3 MIDI and custom events

The engine uses a typed event model rather than treating every cell as an undifferentiated byte array.

It must represent all relevant MIDI messages and file events, including:

- channel voice messages;
- system-common and system-exclusive messages;
- MIDI-file meta-events;
- tempo changes;
- time-signature changes;
- metadata and other imported events that do not initially have specialized TUI editors;
- tracker-specific macro calls.

Common three-byte messages are rendered and edited inline as hexadecimal. Longer or complex events appear as a special marker and open a full event editor. The full editor must also be usable for ordinary three-byte messages when desired.

Each event preserves its typed meaning and, where relevant, its raw-byte representation. Events may use per-field inheritance, but explicit presence flags must distinguish:

- an omitted/inherited field;
- an explicitly supplied byte with value `00`.

Inherited MIDI defaults are local to each pattern-track instance and reset whenever the instance starts or is replaced. Initial zero defaults mean “unset”; incomplete events must not be emitted as valid MIDI.

### 2.4 Timing and off-grid events

- The scheduler uses global BPM and TPB plus each pattern's LPB.
- TPB is also the canonical MIDI tick resolution and the single global scheduler clock.
- Each pattern's LPB divides TPB exactly; a row occupies `TPB / LPB` global ticks.
- Imported MIDI must be preserved as losslessly as possible rather than quantized unnecessarily.
- The project automatically chooses or increases TPB to a common multiple of all pattern LPBs and imported MIDI resolutions so grid-aligned events can be represented exactly.
- Events that do not fall on row boundaries use a row anchor plus an explicit integer intra-row offset in the global TPB tick domain.
- Standard MIDI export converts these absolute/anchored times into ordinary MIDI delta-times.
- Tempo changes are first-class typed timed events and affect playback timing from their event position onward.
- Time-signature changes are preserved and exported, but do not need to affect the primary tracker display in v1.
- The exact handling of MIDI files with unusual timing divisions, tempo maps, same-timestamp ordering, and resolution limits must be specified and covered by tests.

### 2.5 Macro system

User-authored macros are Lua modules executed through `mlua` in a strict sandbox.

Macro ID allocation:

- `00`: reserved for the project setup module and is not a normal composition macro;
- `01–3F`: built-in macro IDs;
- `40–7F`: user-authored macro IDs;
- `80–FF`: reserved because they overlap the standard MIDI status-byte range;
- normal macro ID `00` is not usable as an event ID.

Each macro event has a uniform three-byte compact form:

`macro-id arg1 arg2`

Both arguments are raw bytes (`00–FF`). Every macro uses the same two-argument shape. An argument that is not meaningful to a particular macro is simply ignored; users do not need to enter padding values deliberately.

Built-in macros initially include:

- `01`: trigger the pattern whose ID is `arg1`; `arg2` is ignored/reserved;
- `02`: tempo shorthand using an unsigned Q8.8 BPM value (`arg1` integer part, `arg2` fractional part), while full-fidelity tempo events use the typed complex-event representation.

Undefined built-in IDs are reserved and rejected. User macros can use IDs `40–7F`.

Each macro-ID module defines one or more Lua functions and must expose a fixed entry function:

`run(arg1, arg2, context)`

Macros run as coroutines. The scheduler resumes each active coroutine once per engine tick. A macro may emit zero or more MIDI/custom events before yielding and may use explicit APIs such as:

- emit MIDI/custom events;
- wait for a number of beats;
- trigger child patterns;
- stop track coroutines with a boolean controlling whether the caller is included;
- read or change permitted global/pattern settings;
- register project-local TUI commands from the setup module.

The exact injected API, error behavior, coroutine limits, cancellation semantics, and deterministic resource limits must be specified before implementation. The scheduler resumes coroutines on the single global TPB tick timeline, so macro timing is independent of the LPB of the pattern that invoked the macro.

The project setup module (`00`) is automatically executed in the sandbox as part of loading/initializing a tracker project. It can register project-specific `:` commands and initialize project state. The sandbox has no filesystem, network, process, unrestricted host-object, `os`, `io`, `debug`, `package`, or FFI access. The runtime should enforce practical instruction/time and allocation limits where feasible. v1 treats this sandbox as sufficient containment and does not require a trust prompt.

### 2.6 Settings inheritance

The Go version's root note, scale, modal degree, and chromatic/scale-aware note-entry mode are retained.

- Composition-wide settings are exposed through a global Lua-backed settings module.
- Each pattern has a local settings module whose `__index` links to the global settings module.
- Inheritance is live rather than snapshotted: changing a global value immediately affects running patterns that have not overridden it.
- A pattern-local value shadows the corresponding global value.
- Macros may change either global values or the current pattern's local values.

The built-in scales from the Go version are retained: major, natural minor, harmonic minor, melodic minor, pentatonic, whole-tone, and octatonic. User-defined scales are desirable and should initially be manageable through the colon command layer, using named 12-note pitch-class definitions; a dedicated scale editor can follow later.

### 2.7 MIDI I/O and architecture

The implementation is split into reusable components/libraries, at minimum:

1. core composition and typed event model;
2. deterministic scheduler and macro runtime;
3. Standard MIDI File parsing/writing and tracker project metadata;
4. MIDI backend abstraction;
5. TUI and command layer.

The core scheduler must be testable without hardware by emitting timestamped events into a mock/collector sink.

The initial concrete MIDI stack is:

- `midir` for real-time MIDI device enumeration and output (and future input);
- `midly` for Standard MIDI File parsing and writing;
- project-defined abstractions around both crates.

The v1 application has one globally selected MIDI output port. Automatic JACK graph wiring is out of scope; users may use external tools for routing. The engine should nevertheless not bake JACK assumptions into its core.

Real-time MIDI output is a v1 feature. MIDI input/recording is deferred, but the engine must expose an event-ingestion boundary that can later record incoming messages into patterns/tracks with quantization and off-grid offsets.

### 2.8 MIDI file operations

Project save and standard export are distinct operations, both using `.mid` files.

Project save:

- writes a standards-valid Standard MIDI File;
- stores authoritative tracker structure—patterns, macros, Lua source, settings, and other custom state—in MIDI sequencer-specific meta-events (`FF 7F`);
- also includes a rendered ordinary-MIDI fallback for playback in software that ignores tracker metadata;
- explicitly identifies the fallback so the tracker loader can discard it when tracker metadata is present.

Standard MIDI export:

- renders macros and all other custom tracker behavior into ordinary MIDI events;
- removes tracker-specific metadata;
- uses the selected main/root pattern as the render start point;
- ends at the end of that main pattern;
- produces a file that does not depend on mtrak-specific features.

MIDI import:

- accepts standard `.mid` files;
- preserves standard MIDI events and file metadata losslessly where the model permits;
- chooses a canonical resolution automatically;
- creates one designated main pattern initially;
- splits source tracks into as many tracker tracks as necessary when simultaneous events would collide in one-slot-per-track-row representation;
- does not need to preserve the original MIDI track layout;
- produces an equivalent playback when re-exported, even if the exported track layout differs;
- preserves tempo and time-signature events, with time signatures not necessarily shown in the main display.

The tracker loader treats its authoritative metadata as the source of truth and does not import the rendered fallback as duplicate composition data.

### 2.9 TUI and editing behavior

Retain and redesign the Go version's productive editing workflow:

- terminal-based tracker view;
- keyboard navigation;
- row/track scrolling;
- cursor and resizable brush;
- rectangular selection;
- block insert/delete/backspace-delete/zero;
- cut/copy/paste;
- undo/redo;
- command mode on the bottom line;
- scale-aware and chromatic keyboard note entry;
- inline hexadecimal editing for common messages;
- event markers and a full event editor for complex events;
- pattern selection/creation/deletion and stable numeric IDs;
- globally selected MIDI output port.

The command layer retains the useful Go commands (`read/write`, `bpm`, `tpb`, `lpb`, `root`, `scale`, `mode`, `rows`, and `tracks`) and adds commands for pattern management, MIDI ports, custom scales, import/export, event editing, and macro/project setup as needed. Project-defined commands are TUI-only in v1.

The TUI should make pattern-local LPB, event offsets, inherited fields, macro arguments, and complex event markers understandable without requiring users to manipulate padding bytes or internal serialization details.

### 2.10 Self-contained interaction baseline

The following behavior from the Go prototype is part of the v1 interaction baseline. It is documented here so implementation does not require consulting the Go source; the Go repository is not a runtime dependency or a normative implementation specification.

#### New-project defaults

A new project starts with:

- BPM `120`;
- TPB `24`;
- LPB `4`;
- root note `C4` / MIDI note `60`;
- major scale;
- modal degree `0`;
- scale-aware note entry enabled;
- one pattern with ID `00`;
- 64 rows and 16 tracks in that initial pattern.

#### Default key bindings

- `Ctrl-Q`: quit.
- Arrow keys: move the cursor/brush.
- `PageUp`/`PageDown`: move by `max(LPB, brush height)` rows.
- `Home`/`End`: first/last row.
- `Ctrl-Home`/`Ctrl-End`: top-left/bottom-right.
- `Tab`/`Shift-Tab`: next/previous track.
- `Ctrl-Shift-Right`/`Ctrl-Shift-Left`: insert/delete a track.
- `Ctrl-Right`/`Ctrl-Left`: grow/shrink brush width.
- `Ctrl-Down`/`Ctrl-Up`: grow/shrink brush height.
- `Shift-Right`/`Shift-Left` and `Shift-Down`/`Shift-Up`: resize the selection.
- `Insert` or `Ctrl-Shift-Down`: insert a block.
- `Delete` or `Ctrl-Shift-Up`: delete a block.
- `Backspace`: delete a block backward.
- `.`: clear the selection.
- `Ctrl-X`/`Ctrl-C`/`Ctrl-V`: cut/copy/paste.
- `Space`: start or stop playback.
- `s`: set the playback start row to the cursor row.
- `:`: enter command mode.
- `Ctrl-N`: enter note mode, or leave note mode.
- `N`: toggle chromatic note entry while in note mode.
- `Ctrl-Z`/`Ctrl-R`: undo/redo.
- `Ctrl-S`: save the tracker project.

These are v1 defaults, not an immutable keybinding limitation; the keymap should be isolated from the core engine.

#### Note entry

Scale-aware mode maps `z x c v b n m ,` to scale degrees `0–7` and `q w e r t y u i` to degrees `12–19`. Chromatic mode maps `z s x d c v g b h n j m ,` to semitones `0–12` and `q 2 w 3 e r 5 t 6 y 7 u i` to semitones `12–24`. The selected root, scale, and modal degree convert scale degrees to MIDI notes; resulting notes are clamped to `127`.

The built-in scale names and aliases are:

- major: `M`, `major`;
- natural minor: `m`, `minor`, `min`, `natural minor`;
- harmonic minor: `hm`, `hmin`, `harmonic minor`;
- melodic minor: `mm`, `mmin`, `melodic minor`;
- pentatonic: `p`, `penta`, `pentatonic`;
- whole tone: `w`, `whole`, `whole tone`;
- octatonic: `o`, `octa`, `octatonic`.

Note names use forms such as `C-4` and `C#4`; v1 accepts one-digit octave notation and clamps the resulting MIDI note to `127`.

#### Command mode

The colon command prompt is displayed on the bottom line. Commands accept decimal or conventional hexadecimal integer syntax (`64`, `0x40`, and `0X40`). Invalid values produce an error and do not mutate project state.

The retained commands are:

- `:read [filename]`, `:r [filename]`: load a tracker project or standard MIDI file, detecting tracker metadata automatically;
- `:write [filename]`, `:w [filename]`: save a tracker project with metadata and rendered fallback;
- `:export [filename]`: render and write standard MIDI without tracker metadata;
- `:bpm INTEGER`: set composition BPM;
- `:tpb INTEGER`: set global ticks per beat;
- `:lpb INTEGER`: set the current pattern's lines per beat;
- `:root NOTE`: set the global root note;
- `:scale [ROOT] [SCALE] [+MODE]`: select root, scale, and/or modal degree in any order;
- `:sca ...` and `:s ...`: aliases for `:scale`;
- `:mode INTEGER`, `:m INTEGER`: set the modal degree;
- `:rows INTEGER`: resize the current pattern;
- `:tracks INTEGER`: resize the current pattern's track count.

The scale command additionally supports user-defined scales:

- `:scale add NAME MASK`: add a named 12-bit pitch-class mask, such as `101001010100`;
- `:scale use NAME`: select a user-defined scale;
- `:scale list`: list built-in and user-defined scales;
- `:scale delete NAME`: delete a user-defined scale.

Pattern management, MIDI-port selection, event editing, macro setup, and other new features add commands without changing the retained command semantics. Project-defined commands registered by Lua are TUI-only in v1.

#### Tracker grid and visual conventions

The main view has a status header and a bordered tracker grid. Row numbers use four hexadecimal digits. Track headers use two-digit hexadecimal track IDs. A common three-byte event occupies six hexadecimal character positions; empty nibbles render as `·`. The grid scrolls vertically by row and horizontally by track. Cursor, brush, selection, playback-row, beat-row, and note-entry locations have distinct visual styling. Command mode adds a bottom-line prompt, and errors add a bottom-line diagnostic.

The header displays BPM, TPB, LPB, and available timing/status information. Backend-specific frame/sample-rate diagnostics may be omitted when the selected MIDI backend cannot provide them.

#### Block editing

The default brush is one cell wide and one row high. Brush width cycles through `1`, `2`, `6`, and the full pattern width; brush height cycles through `1`, the current pattern LPB, and the full pattern height. Selection resizing follows the key bindings above.

Insert shifts the selected columns downward and clears the inserted area. Delete shifts them upward and clears the trailing area. Backspace shifts them upward before the current selection. Clear fills the selection with empty events. Cut copies the selection and clears it; copy leaves it unchanged; paste is aligned to the current row and track while preserving the copied horizontal byte/nibble offset. Mutating block operations are undoable, and the undo history is capped at 64 actions. Track insertion/deletion affects every row; row/track resizing preserves existing content and adds or removes empty cells at the end.

#### Playback controls

`Space` toggles playback. `s` sets the playback start row. Starting playback resets the relevant scheduler position; stopping stops output and resets the current tick position. The UI highlights the active playback row and beat rows. The old prototype loops its only pattern, but the Rust design stops a pattern instance at its final row and uses explicit child-pattern triggers instead. In note mode, entering a note both writes it to the current event slot and auditions it immediately through the selected MIDI output.

## 3. Engineering principles

- Keep the core independent of Bubble Tea, terminal rendering, JACK, and a particular MIDI backend.
- Prefer explicit typed state over ambiguous byte conventions, while retaining compact tracker editing where it is useful.
- Make scheduling deterministic and testable with a fake clock and event sink.
- Preserve imported data rather than silently quantizing or dropping it.
- Keep tracker-specific data in standards-valid MIDI containers.
- Version and validate the tracker-specific metadata format from the beginning.
- Treat Lua as a controlled extension mechanism, never as a substitute for core invariants.
- Add round-trip, playback-equivalence, and malformed-input tests before optimizing the TUI.

## 4. Major implementation milestones

1. Define the Rust workspace and crate boundaries; establish CI, formatting, linting, and test conventions.
2. Implement typed MIDI/custom event values, inheritance flags, pattern/track/slot structures, stable pattern IDs, timing values, and settings inheritance.
3. Implement deterministic tick scheduling, pattern instances, track-local child ownership, tempo changes, and event collection without a real MIDI backend.
4. Embed sandboxed `mlua`, implement module loading, fixed macro entry points, built-ins `01` and `02`, coroutine scheduling, APIs, cancellation, and limits.
5. Implement Standard MIDI File import/export plus versioned sequencer-specific project metadata and rendered fallback handling.
6. Implement the reusable MIDI backend layer with `midir` and global output-port selection.
7. Implement the Rust TUI, including the tracker grid, typed inline rendering, complex-event editor, commands, note entry, pattern management, block editing, undo/redo, and macro/setting workflows.
8. Add end-to-end tests and usability hardening for project save/load, standard export, MIDI import, macro rendering, tempo maps, pattern replacement, and malformed/untrusted project data.

## 5. Validation targets

The implementation is ready for v1 when it can demonstrate at least:

- deterministic playback of nested patterns with per-pattern LPB, global BPM/TPB, and one shared scheduler clock;
- explicit track-local child replacement and macro-controlled coroutine cancellation;
- lossless round-trip of supported standard MIDI timing/events, including tempo changes and complex events;
- project save/load without duplicating rendered fallback events;
- standard MIDI export that renders custom macros and remains usable without mtrak metadata;
- sandboxed Lua macros with no host access and bounded failure behavior;
- a usable TUI for common three-byte events, complex events, patterns, tracks, scales, and commands;
- unit and integration tests that run without MIDI hardware.

## 6. Items requiring detailed design during implementation

The following are intentionally left for the reviewed design and implementation phases to pin down precisely:

- exact Rust workspace/crate names and public APIs;
- the automatic TPB/common-resolution calculation, TPB rescaling when new LPBs are introduced, and handling of unusual MIDI divisions;
- ordering rules for simultaneous events and imported metadata;
- the full typed event enum and raw-byte preservation rules;
- the exact tracker metadata schema/versioning and fallback marker;
- Lua module packaging inside sequencer-specific meta-events;
- macro coroutine ownership, cancellation delivery, and resource limits;
- the exact layout and keyboard interaction of the complex-event editor;
- MIDI output scheduling behavior and port reconnection/error handling;
- the initial limits for patterns, rows, tracks, event sizes, and Lua memory/time.
