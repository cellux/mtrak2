//! Application state: workspaces, the pattern grid, the Code editor, the
//! chooser, the command layer, and playback. This is terminal-independent so it
//! can be fully unit-tested; [`crate::ui`] only draws it and feeds it keys.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use mtrak_core::composition::{Composition, Pattern, PatternId, Slot};
use mtrak_core::event::{ChannelKind, ChannelMessage, Event};
use mtrak_core::modules::{builtin_module, is_builtin_module, is_reserved_module, MODULE_TEMPLATE};
use mtrak_core::settings::EffectiveSettings;
use mtrak_core::timing::{row_ticks, Bpm};
use mtrak_engine::macros::MacroFactory;
use mtrak_engine::scheduler::TimedEvent;
use mtrak_midi::{MidiBackend, MidiError, OutputManager, PortInfo};

use crate::chooser::{Chooser, ChooserKind};
use crate::clipboard::{destination_edit, NibbleClipboard};
use crate::code::CodeEditor;
use crate::command::{self, Command, ScaleArgs, SeedArg};
use crate::edit::{CellEdit, EVENT_NIBBLES, OFFSET_NIBBLES};
use crate::key::{self, Action, Code, Key};
use crate::line::LineEditor;
use crate::undo::UndoStack;

/// A top-level workspace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// `F1` — the help placeholder.
    Help,
    /// `F2` — the pattern grid.
    Pattern,
    /// `F3` — the Lua code editor.
    Code,
}

/// The mode the editor is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Play notes without changing pattern data.
    Play,
    /// Play notes and record them into the selected track.
    Rec,
    /// Edit pattern cells nibble by nibble.
    Edit,
    /// The colon command prompt.
    Command,
    /// The pattern/module chooser.
    Chooser,
}

impl Mode {
    /// Whether this musical mode follows the playhead.
    pub fn follows_playback(self) -> bool {
        matches!(self, Mode::Play | Mode::Rec)
    }

    /// Whether this musical mode allows pattern editing.
    pub fn is_editing(self) -> bool {
        self == Mode::Edit
    }

    /// Whether this is a musical (not transient UI) mode.
    pub fn is_musical(self) -> bool {
        matches!(self, Mode::Play | Mode::Rec | Mode::Edit)
    }
}

/// Playback position captured while command mode pauses the transport.
#[derive(Clone, Debug)]
struct PlaybackPause {
    tick: u64,
    micros: f64,
    start_row: usize,
    current_row: usize,
}

/// A single-line status message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// The message text.
    pub text: String,
    /// Whether it reports an error.
    pub is_error: bool,
}

/// How many candidates PgUp/PgDn move the completion highlight.
const COMPLETION_PAGE: isize = 10;
/// Range of the session-local keyboard transpose in semitones.
pub const MIN_TRANSPOSE: i32 = -127;
/// Range of the session-local keyboard transpose in semitones.
pub const MAX_TRANSPOSE: i32 = 127;
/// Octave step used by the transpose keys.
const TRANSPOSE_OCTAVE: i32 = 12;

/// The open Tab-completion list of a path argument on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PathCompletion {
    /// Character index where the path argument starts.
    start: usize,
    /// Character index one past the path argument.
    end: usize,
    /// Directory part of the path when the menu opened, including its `/`.
    /// The menu closes once the typed path no longer starts with it.
    dir_prefix: String,
    /// Every candidate for that directory, sorted.
    all: Vec<String>,
    /// Indices into `all` that match the typed path.
    matches: Vec<usize>,
    /// Highlighted index into `matches`.
    selected: usize,
}

/// A project command registered by the Lua setup module. It is TUI-only and is
/// never serialized into an exported file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LuaCommand {
    /// The command name (without the leading colon).
    pub name: String,
    /// An optional description.
    pub description: Option<String>,
}

/// How the brush width cycles (measured in nibbles).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrushWidth {
    /// One nibble.
    One,
    /// Two nibbles.
    Two,
    /// Six nibbles (one three-byte event).
    Six,
    /// Ten nibbles (one event plus its offset; offsets visible only).
    Ten,
    /// The full pattern width.
    Full,
}

impl BrushWidth {
    /// The next wider setting, given whether offsets are visible.
    pub fn grow(self, offsets_visible: bool) -> Self {
        match self {
            BrushWidth::One => BrushWidth::Two,
            BrushWidth::Two => BrushWidth::Six,
            BrushWidth::Six if offsets_visible => BrushWidth::Ten,
            BrushWidth::Six => BrushWidth::Full,
            BrushWidth::Ten => BrushWidth::Full,
            BrushWidth::Full => BrushWidth::Full,
        }
    }

    /// The next narrower setting, given whether offsets are visible.
    pub fn shrink(self, offsets_visible: bool) -> Self {
        match self {
            BrushWidth::One => BrushWidth::One,
            BrushWidth::Two => BrushWidth::One,
            BrushWidth::Six => BrushWidth::Two,
            BrushWidth::Ten => BrushWidth::Six,
            BrushWidth::Full if offsets_visible => BrushWidth::Ten,
            BrushWidth::Full => BrushWidth::Six,
        }
    }

    /// Converts a preset that is no longer valid when offsets are hidden.
    pub fn hide_offsets(self) -> Self {
        match self {
            BrushWidth::Ten => BrushWidth::Six,
            other => other,
        }
    }

    /// The number of nibbles across, given the cell width and track count.
    pub fn nibbles(self, nibbles_per_cell: usize, tracks: usize) -> usize {
        match self {
            BrushWidth::One => 1,
            BrushWidth::Two => 2,
            BrushWidth::Six => 6,
            BrushWidth::Ten => 10,
            BrushWidth::Full => nibbles_per_cell.saturating_mul(tracks),
        }
        .max(1)
    }
}

/// How the brush height cycles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrushHeight {
    /// One row.
    One,
    /// The current pattern's LPB.
    Lpb,
    /// The full pattern height.
    Full,
}

impl BrushHeight {
    /// The next taller setting.
    pub fn grow(self) -> Self {
        match self {
            BrushHeight::One => BrushHeight::Lpb,
            BrushHeight::Lpb => BrushHeight::Full,
            BrushHeight::Full => BrushHeight::Full,
        }
    }

    /// The next shorter setting.
    pub fn shrink(self) -> Self {
        match self {
            BrushHeight::One => BrushHeight::One,
            BrushHeight::Lpb => BrushHeight::One,
            BrushHeight::Full => BrushHeight::Lpb,
        }
    }

    /// The number of rows, given LPB and the pattern height.
    pub fn cells(self, lpb: usize, height: usize) -> usize {
        match self {
            BrushHeight::One => 1,
            BrushHeight::Lpb => lpb.max(1),
            BrushHeight::Full => height,
        }
        .min(height.max(1))
    }
}

/// An inclusive rectangular region. `left`/`right` are absolute nibble columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    /// First row.
    pub top: usize,
    /// Last row (inclusive).
    pub bottom: usize,
    /// First nibble column.
    pub left: usize,
    /// Last nibble column (inclusive).
    pub right: usize,
}

impl Rect {
    /// The number of rows.
    pub fn height(&self) -> usize {
        self.bottom.saturating_sub(self.top) + 1
    }

    /// The number of nibble columns.
    pub fn width(&self) -> usize {
        self.right.saturating_sub(self.left) + 1
    }

    /// The inclusive track range covered by the column range.
    pub fn tracks(&self, nibbles_per_cell: usize) -> (usize, usize) {
        (self.left / nibbles_per_cell, self.right / nibbles_per_cell)
    }

    /// Whether the rectangle contains the given row and nibble column.
    pub fn contains(&self, row: usize, column: usize) -> bool {
        row >= self.top && row <= self.bottom && column >= self.left && column <= self.right
    }
}

/// A rectangular nibble selection anchored at a point.
///
/// The anchor is fixed; the logical head follows the far edge of the current
/// extension. The operation bounds are derived from the anchor and head but
/// are rounded outward to complete bytes horizontally and snapped to whole
/// event cells once the head leaves the anchor cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The fixed anchor `(row, column)`.
    pub anchor: (usize, usize),
    /// The logical head `(row, column)` the cursor follows.
    pub head: (usize, usize),
    /// The byte-aligned operation rectangle.
    pub bounds: Rect,
    /// Brush width in nibbles captured when the selection started.
    pub brush_width: usize,
    /// Brush height in rows captured when the selection started.
    pub brush_height: usize,
    /// Nibbles per event cell captured when the selection started.
    pub cell_nibbles: usize,
    /// The current horizontal growth direction (`-1`, `0`, or `1`).
    pub hdir: isize,
    /// The current vertical growth direction (`-1`, `0`, or `1`).
    pub vdir: isize,
}

impl Selection {
    /// The byte-aligned operation rectangle.
    pub fn rect(&self) -> Rect {
        self.bounds
    }
}

/// What the chooser clipboard holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChooserClipboard {
    /// A copied pattern.
    Pattern(Pattern),
    /// A copied module source.
    Module {
        /// The source module ID (for diagnostics).
        id: u8,
        /// The module source.
        source: String,
    },
}

/// Provides a fresh macro factory for rendering (e.g. a Lua runtime).
pub trait MacroSource {
    /// Builds a macro factory, or `None` when there are no macros.
    fn factory(&self) -> Option<Box<dyn MacroFactory>>;
    /// Rebuilds the runtime from `modules`, returning registered commands.
    fn reload(&mut self, modules: &BTreeMap<u8, String>) -> Result<Vec<LuaCommand>, String>;
}

/// The whole application state.
pub struct App {
    project: mtrak_midi_io::TrackerProject,
    screen: Screen,
    mode: Mode,
    previous_mode: Mode,
    message: Option<Message>,
    input: LineEditor,
    command_failed: bool,
    path_completion: Option<PathCompletion>,

    // Pattern workspace.
    current: PatternId,
    row: usize,
    track: usize,
    nibble: usize,
    brush_width: BrushWidth,
    brush_height: BrushHeight,
    selection: Option<Selection>,
    edit: Option<CellEdit>,
    edit_recorded: bool,
    clipboard: NibbleClipboard,
    offsets_visible: bool,
    record_step: u8,
    /// Session-local keyboard/input transpose in semitones.
    transpose: i32,
    pattern_undo: HashMap<PatternId, UndoStack<Pattern>>,
    pattern_struct_undo: UndoStack<Composition>,

    // Code workspace.
    module_id: u8,
    code: CodeEditor,
    code_scroll: usize,
    module_undo: HashMap<u8, UndoStack<String>>,
    code_struct_undo: UndoStack<BTreeMap<u8, String>>,

    // Chooser.
    chooser: Option<Chooser>,
    chooser_clipboard: Option<ChooserClipboard>,

    // External services.
    midi: Option<OutputManager<Box<dyn MidiBackend>>>,
    macro_source: Option<Box<dyn MacroSource>>,
    lua_commands: Vec<LuaCommand>,
    lua_dirty: bool,

    // Session.
    path: Option<PathBuf>,
    quit: bool,
    dirty: bool,
    project_revision: u64,

    // Playback.
    playing: bool,
    play_events: Vec<TimedEvent>,
    play_next: usize,
    play_tick: u64,
    play_micros: f64,
    play_start_row: usize,
    play_current_row: usize,
    /// Absolute tick where the pattern ends; playback stops there even if the
    /// last rows contain no events.
    play_end_tick: u64,
    /// Playback captured while command mode pauses the transport.
    command_pause: Option<PlaybackPause>,
}

impl App {
    /// Creates an app around a project.
    pub fn new(project: mtrak_midi_io::TrackerProject) -> Self {
        let current = project.composition.selected_main();
        let module_id = 0u8;
        let code = CodeEditor::from_text(&module_source(&project, module_id).unwrap_or_default());
        App {
            project,
            screen: Screen::Pattern,
            mode: Mode::Play,
            previous_mode: Mode::Play,
            message: None,
            input: LineEditor::new(),
            command_failed: false,
            path_completion: None,
            current,
            row: 0,
            track: 0,
            nibble: 0,
            brush_width: BrushWidth::One,
            brush_height: BrushHeight::One,
            selection: None,
            edit: None,
            edit_recorded: false,
            clipboard: NibbleClipboard::default(),
            offsets_visible: false,
            record_step: 1,
            transpose: 0,
            pattern_undo: HashMap::new(),
            pattern_struct_undo: UndoStack::new(64),
            module_id,
            code,
            code_scroll: 0,
            module_undo: HashMap::new(),
            code_struct_undo: UndoStack::new(64),
            chooser: None,
            chooser_clipboard: None,
            midi: None,
            macro_source: None,
            lua_commands: Vec::new(),
            lua_dirty: true,
            path: None,
            quit: false,
            dirty: false,
            project_revision: 0,
            playing: false,
            play_events: Vec::new(),
            play_next: 0,
            play_tick: 0,
            play_micros: 0.0,
            play_start_row: 0,
            play_current_row: 0,
            play_end_tick: 0,
            command_pause: None,
        }
    }

    /// The current project.
    pub fn project(&self) -> &mtrak_midi_io::TrackerProject {
        &self.project
    }

    /// A revision counter bumped whenever the loaded project is replaced.
    pub fn project_revision(&self) -> u64 {
        self.project_revision
    }

    /// The active workspace.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The current mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The current command line / editor buffer.
    pub fn input_text(&self) -> String {
        self.input.text()
    }

    /// The command cursor position.
    pub fn input_cursor(&self) -> usize {
        self.input.cursor()
    }

    /// The open path-completion menu as `(all candidates, matching indices,
    /// highlighted match)`.
    pub fn path_completions(&self) -> Option<(&[String], &[usize], usize)> {
        self.path_completion.as_ref().map(|completion| {
            (
                completion.all.as_slice(),
                completion.matches.as_slice(),
                completion.selected,
            )
        })
    }

    /// Whether the current command is in the failed (red) state.
    pub fn command_failed(&self) -> bool {
        self.command_failed
    }

    /// The current status message.
    pub fn message(&self) -> Option<&Message> {
        self.message.as_ref()
    }

    /// Whether the app should exit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether the project has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether playback is running.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// The highlighted playback row.
    pub fn play_row(&self) -> usize {
        self.play_current_row
    }

    /// The playback start row.
    pub fn start_row(&self) -> usize {
        self.play_start_row
    }

    /// The current pattern ID.
    pub fn current_pattern(&self) -> PatternId {
        self.current
    }

    /// The current pattern.
    pub fn pattern(&self) -> &Pattern {
        self.project
            .composition
            .pattern(self.current)
            .expect("current pattern always exists")
    }

    /// The commands registered by the project setup module.
    pub fn lua_commands(&self) -> &[LuaCommand] {
        &self.lua_commands
    }

    /// Sets the project-defined Lua commands (TUI-only).
    pub fn set_lua_commands(&mut self, commands: Vec<LuaCommand>) {
        self.lua_commands = commands;
    }

    /// Installs the MIDI output manager.
    pub fn set_midi(&mut self, midi: OutputManager<Box<dyn MidiBackend>>) {
        self.midi = Some(midi);
    }

    /// Installs the macro source used for rendering.
    pub fn set_macro_source(&mut self, source: Box<dyn MacroSource>) {
        self.macro_source = Some(source);
        self.lua_dirty = true;
    }

    /// Sets the current file path.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// The current file path, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// The composition-global settings.
    pub fn effective_settings(&self) -> EffectiveSettings {
        *self.project.composition.global_settings()
    }

    // --- Geometry -------------------------------------------------------------

    /// The number of nibbles in one cell (event, plus offset when visible).
    pub fn nibbles_per_cell(&self) -> usize {
        if self.offsets_visible {
            EVENT_NIBBLES + OFFSET_NIBBLES
        } else {
            EVENT_NIBBLES
        }
    }

    /// Whether offsets are visible.
    pub fn offsets_visible(&self) -> bool {
        self.offsets_visible
    }

    /// Whether the current musical mode follows the playhead.
    pub fn follows_playhead(&self) -> bool {
        self.mode.follows_playback()
    }

    /// The session-local recording step in rows.
    pub fn recording_step(&self) -> u8 {
        self.record_step
    }

    /// Sets the session-local recording step.
    pub fn set_record_step(&mut self, step: u8) {
        self.record_step = step;
    }

    /// The session-local keyboard/input transpose in semitones.
    pub fn transpose(&self) -> i32 {
        self.transpose
    }

    /// Sets the keyboard transpose, clamped to the supported range.
    pub fn set_transpose(&mut self, semitones: i32) {
        self.transpose = semitones.clamp(MIN_TRANSPOSE, MAX_TRANSPOSE);
    }

    /// Changes the keyboard transpose by one octave.
    pub fn transpose_octave(&mut self, direction: i32) {
        let value = self
            .transpose
            .saturating_add(direction.signum() * TRANSPOSE_OCTAVE);
        self.set_transpose(value);
        self.set_message(format!("transpose {:+}", self.transpose), false);
    }

    /// The grid cursor as `(row, track, nibble)`.
    pub fn cursor(&self) -> (usize, usize, usize) {
        (self.row, self.track, self.nibble)
    }

    /// The brush width setting.
    pub fn brush_width(&self) -> BrushWidth {
        self.brush_width
    }

    /// The brush height setting.
    pub fn brush_height(&self) -> BrushHeight {
        self.brush_height
    }

    /// The selection, if any.
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// Whether the selected scale is chromatic.
    pub fn note_chromatic(&self) -> bool {
        self.effective_settings().is_chromatic()
    }

    fn pattern_geometry(&self) -> (usize, usize) {
        (self.pattern().row_count(), self.pattern().track_count())
    }

    fn column_of(&self, track: usize, nibble: usize) -> usize {
        track * self.nibbles_per_cell() + nibble
    }

    fn cursor_column(&self) -> usize {
        self.column_of(self.track, self.nibble)
    }

    fn set_cursor_column(&mut self, column: usize) {
        let npc = self.nibbles_per_cell();
        let (_, tracks) = self.pattern_geometry();
        let max = tracks.saturating_mul(npc).saturating_sub(1);
        let column = column.min(max);
        self.track = column / npc;
        self.nibble = column % npc;
    }

    /// The inclusive brush rectangle derived from the logical cursor.
    ///
    /// The brush origin shifts backward at the right and bottom pattern edges
    /// so the rectangle stays fully inside the pattern; the logical cursor is
    /// never moved by this adjustment.
    pub fn brush_rect(&self) -> Rect {
        let npc = self.nibbles_per_cell().max(1);
        let (rows, tracks) = self.pattern_geometry();
        let rows = rows.max(1);
        let tracks = tracks.max(1);
        let total_cols = tracks * npc;
        let width = self.brush_width.nibbles(npc, tracks).clamp(1, total_cols);
        let height = self
            .brush_height
            .cells(self.pattern().lpb() as usize, rows)
            .clamp(1, rows);
        let cursor_col = self.cursor_column().min(total_cols - 1);
        let start_col = if cursor_col + width > total_cols {
            total_cols - width
        } else {
            cursor_col
        };
        let cursor_row = self.row.min(rows - 1);
        let start_row = if cursor_row + height > rows {
            rows - height
        } else {
            cursor_row
        };
        Rect {
            top: start_row,
            bottom: start_row + height - 1,
            left: start_col,
            right: start_col + width - 1,
        }
    }

    /// The exact byte-aligned selection bounds, if a selection is active.
    pub fn selection_bounds(&self) -> Option<Rect> {
        self.selection.map(|selection| selection.rect())
    }

    /// The byte-aligned rectangle Copy, Cut, and Clear act on.
    ///
    /// With an active selection this is the selection bounds; otherwise it is
    /// the brush rectangle rounded outward to complete bytes.
    pub fn operation_rect(&self) -> Rect {
        match self.selection_bounds() {
            Some(rect) => rect,
            None => {
                let rect = self.brush_rect();
                let (rows, tracks) = self.pattern_geometry();
                let max_col = tracks.max(1) * self.nibbles_per_cell().max(1);
                Rect {
                    top: rect.top.min(rows.saturating_sub(1)),
                    bottom: rect.bottom.min(rows.saturating_sub(1)),
                    left: rect.left & !1,
                    right: (rect.right | 1).min(max_col.saturating_sub(1)),
                }
            }
        }
    }

    /// The whole-cell rectangle Insert, Delete, and Backspace act on.
    pub fn cell_rect(&self) -> Rect {
        let npc = self.nibbles_per_cell().max(1);
        let base = match self.selection_bounds() {
            Some(rect) => rect,
            None => self.brush_rect(),
        };
        let (left, right) = base.tracks(npc);
        Rect {
            top: base.top,
            bottom: base.bottom,
            left: left * npc,
            right: right * npc + npc - 1,
        }
    }

    /// Whether any cell in `rect` holds a non-inline (opaque) event.
    fn rect_has_complex(&self, rect: Rect) -> bool {
        let npc = self.nibbles_per_cell().max(1);
        let (left, right) = rect.tracks(npc);
        for row in rect.top..=rect.bottom {
            for track in left..=right {
                if let Some(slot) = self.pattern().slot(row, track) {
                    if !slot.is_empty() && slot.event.inline_hex().is_none() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Whether the active edit buffer's offset is out of range.
    pub fn offset_invalid(&self) -> bool {
        self.edit
            .as_ref()
            .map(|edit| edit.offset_invalid)
            .unwrap_or(false)
    }

    /// Renders a cell, including the active edit buffer and offset column.
    pub fn cell_display(&self, row: usize, track: usize) -> String {
        if let Some(edit) = &self.edit {
            if edit.row == row && edit.track == track {
                return render_cell(
                    &edit.event_text(),
                    Some(&edit.offset_text()),
                    self.offsets_visible,
                );
            }
        }
        let Some(slot) = self.pattern().slot(row, track) else {
            return render_cell("······", Some("0000"), self.offsets_visible);
        };
        let event = if slot.is_empty() {
            "······".to_string()
        } else {
            match slot.event.inline_hex() {
                Some(hex) => format!("{hex:<6}").chars().take(6).collect(),
                None => "◈·····".to_string(),
            }
        };
        let offset = format!("{:04X}", slot.offset);
        render_cell(&event, Some(&offset), self.offsets_visible)
    }

    // --- Navigation -----------------------------------------------------------

    fn clamp_cursor(&mut self) {
        let (rows, tracks) = self.pattern_geometry();
        self.row = self.row.min(rows.saturating_sub(1));
        self.track = self.track.min(tracks.saturating_sub(1));
        self.nibble = self.nibble.min(self.nibbles_per_cell().saturating_sub(1));
    }

    fn move_left(&mut self) {
        if self.nibble > 0 {
            self.nibble -= 1;
        } else if self.track > 0 {
            self.track -= 1;
            self.nibble = self.nibbles_per_cell() - 1;
        }
    }

    fn move_right(&mut self) {
        let (_, tracks) = self.pattern_geometry();
        if self.nibble + 1 < self.nibbles_per_cell() {
            self.nibble += 1;
        } else if self.track + 1 < tracks {
            self.track += 1;
            self.nibble = 0;
        }
    }

    /// Applies a navigation or editing action.
    pub fn apply(&mut self, action: Action) {
        // Workspace-independent actions.
        match action {
            Action::HelpScreen => return self.switch_screen(Screen::Help),
            Action::PatternScreen => return self.switch_screen(Screen::Pattern),
            Action::CodeScreen => return self.switch_screen(Screen::Code),
            Action::OpenChooser => return self.open_chooser(),
            Action::ReloadLua => return self.reload_lua(),
            Action::Quit => {
                // Quitting is a user-initiated stop: do not leave notes
                // ringing behind us.
                self.stop_playback_and_release();
                self.quit = true;
                return;
            }
            Action::Save => {
                let _ = self.save(None);
                return;
            }
            Action::AllNotesOff => {
                self.all_notes_off();
                return;
            }
            _ => {}
        }
        if self.screen != Screen::Pattern {
            return;
        }
        let page = (self.pattern().lpb() as usize)
            .max(self.brush_rect().height())
            .max(
                self.selection_bounds()
                    .map(|rect| rect.height())
                    .unwrap_or(1),
            );
        let (rows, tracks) = self.pattern_geometry();
        let editing = self.mode.is_editing();
        match action {
            Action::MoveLeft => {
                self.cancel_selection();
                if self.mode.follows_playback() {
                    // PLAY/REC navigate whole tracks and keep the nibble.
                    self.track = self.track.saturating_sub(1);
                } else {
                    self.move_left();
                }
                self.sync_edit();
            }
            Action::MoveRight => {
                self.cancel_selection();
                if self.mode.follows_playback() {
                    self.track = (self.track + 1).min(tracks.saturating_sub(1));
                } else {
                    self.move_right();
                }
                self.sync_edit();
            }
            Action::MoveUp => {
                self.cancel_selection();
                self.row = self.row.saturating_sub(1);
                self.sync_edit();
            }
            Action::MoveDown => {
                self.cancel_selection();
                self.row = (self.row + 1).min(rows.saturating_sub(1));
                self.sync_edit();
            }
            Action::PageUp => {
                self.cancel_selection();
                self.row = self.row.saturating_sub(page);
                self.sync_edit();
            }
            Action::PageDown => {
                self.cancel_selection();
                self.row = (self.row + page).min(rows.saturating_sub(1));
                self.sync_edit();
            }
            Action::Home => {
                self.cancel_selection();
                self.row = 0;
                self.sync_edit();
            }
            Action::End => {
                self.cancel_selection();
                self.row = rows.saturating_sub(1);
                self.sync_edit();
            }
            Action::TopLeft => {
                self.cancel_selection();
                self.row = 0;
                self.track = 0;
                self.nibble = 0;
                self.sync_edit();
            }
            Action::BottomRight => {
                self.cancel_selection();
                self.row = rows.saturating_sub(1);
                self.track = tracks.saturating_sub(1);
                self.nibble = self.nibbles_per_cell() - 1;
                self.sync_edit();
            }
            Action::NextTrack => {
                self.cancel_selection();
                self.track = (self.track + 1).min(tracks.saturating_sub(1));
                self.sync_edit();
            }
            Action::PrevTrack => {
                self.cancel_selection();
                self.track = self.track.saturating_sub(1);
                self.sync_edit();
            }
            Action::TrackInsert => {
                if editing {
                    self.insert_track();
                }
            }
            Action::TrackDelete => {
                if editing {
                    self.delete_track();
                }
            }
            Action::BrushWider => {
                if editing {
                    self.cancel_selection();
                    self.brush_width = self.brush_width.grow(self.offsets_visible);
                }
            }
            Action::BrushNarrower => {
                if editing {
                    self.cancel_selection();
                    self.brush_width = self.brush_width.shrink(self.offsets_visible);
                }
            }
            Action::BrushTaller => {
                if editing {
                    self.cancel_selection();
                    self.brush_height = self.brush_height.grow();
                }
            }
            Action::BrushShorter => {
                if editing {
                    self.cancel_selection();
                    self.brush_height = self.brush_height.shrink();
                }
            }
            Action::SelectLeft | Action::SelectRight | Action::SelectUp | Action::SelectDown => {
                if editing {
                    match action {
                        Action::SelectLeft => self.extend_selection(-1, 0),
                        Action::SelectRight => self.extend_selection(1, 0),
                        Action::SelectUp => self.extend_selection(0, -1),
                        _ => self.extend_selection(0, 1),
                    }
                }
            }
            Action::InsertBlock => {
                if editing {
                    self.insert_block();
                }
            }
            Action::DeleteBlock => {
                if editing {
                    self.delete_block();
                }
            }
            Action::BackspaceBlock => {
                if editing {
                    self.backspace_block();
                }
            }
            Action::ClearBlock => {
                if editing {
                    self.clear_block();
                }
            }
            Action::CancelSelection => self.cancel_selection(),
            Action::Cut => {
                if editing {
                    self.cut();
                }
            }
            Action::Copy => {
                if editing {
                    self.copy();
                }
            }
            Action::Paste => {
                if editing {
                    self.paste();
                }
            }
            Action::ToggleOffsets => {
                if editing {
                    self.toggle_offsets();
                }
            }
            Action::TogglePlay => self.toggle_playback(),
            Action::SetStartRow => self.play_start_row = self.row,
            Action::SetRecordStep(step) => self.record_step = step,
            Action::TransposeDown => self.transpose_octave(-1),
            Action::TransposeUp => self.transpose_octave(1),
            Action::CommandMode => self.enter_command_mode(),
            Action::PlayMode => self.set_mode(Mode::Play),
            Action::RecMode => self.set_mode(Mode::Rec),
            Action::EditMode => self.set_mode(Mode::Edit),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::OpenEventEditor => {
                if editing {
                    self.open_event_command();
                }
            }
            Action::HelpScreen
            | Action::PatternScreen
            | Action::CodeScreen
            | Action::OpenChooser
            | Action::ReloadLua
            | Action::Quit
            | Action::Save
            | Action::AllNotesOff => {}
        }
        if self.mode.follows_playback() && self.playing {
            self.row = self.play_current_row.min(rows.saturating_sub(1));
            self.selection = None;
        }
    }

    /// Creates a fresh selection at the cursor with the current brush size.
    ///
    /// The first Shift+Arrow only starts the selection; later presses extend
    /// it. The initial horizontal bounds are rounded outward to complete
    /// bytes. A one-nibble brush whose anchor sits on the second nibble of a
    /// byte includes the following byte, matching the documented examples.
    fn create_selection(&mut self, dcol: isize, drow: isize) {
        let npc = self.nibbles_per_cell().max(1);
        let (rows, tracks) = self.pattern_geometry();
        let rows = rows.max(1);
        let total_cols = tracks.max(1) * npc;
        let anchor_row = self.row.min(rows - 1);
        let anchor_col = self.cursor_column().min(total_cols - 1);
        let brush_width = self.brush_width.nibbles(npc, tracks).max(1);
        let brush_height = self
            .brush_height
            .cells(self.pattern().lpb() as usize, rows)
            .max(1);
        let left = anchor_col & !1;
        // Growing right from the second nibble of a one-nibble brush includes
        // the following byte, matching the documented examples. Growing left
        // keeps the anchor's own byte.
        let right = if dcol > 0 && brush_width == 1 && anchor_col % 2 == 1 {
            (anchor_col + 1) | 1
        } else {
            (anchor_col + brush_width - 1) | 1
        };
        let right = right.min(total_cols - 1);
        let bottom = (anchor_row + brush_height - 1).min(rows - 1);
        let hdir = dcol.signum();
        let vdir = drow.signum();
        let head = (
            if vdir > 0 { bottom } else { anchor_row },
            match hdir {
                1 => right,
                -1 => left,
                _ => anchor_col,
            },
        );
        self.selection = Some(Selection {
            anchor: (anchor_row, anchor_col),
            head,
            bounds: Rect {
                top: anchor_row,
                bottom,
                left,
                right,
            },
            brush_width,
            brush_height,
            cell_nibbles: npc,
            hdir,
            vdir,
        });
        self.row = head.0;
        self.set_cursor_column(head.1);
        self.sync_edit();
    }

    fn extend_selection(&mut self, dcol: isize, drow: isize) {
        if self.selection.is_none() {
            self.create_selection(dcol, drow);
            return;
        }
        let mut selection = self.selection.expect("selection exists");
        let npc = selection.cell_nibbles.max(1);
        let (rows, tracks) = self.pattern_geometry();
        let rows = rows.max(1);
        let total_cols = tracks.max(1) * npc;

        if dcol != 0 {
            let dir = dcol.signum();
            if selection.hdir == 0 {
                selection.hdir = dir;
                grow_horizontal(&mut selection, dir, npc, total_cols);
            } else if selection.hdir == dir {
                grow_horizontal(&mut selection, dir, npc, total_cols);
            } else if !shrink_horizontal(&mut selection) {
                selection.hdir = 0;
            }
        }

        if drow != 0 {
            let dir = drow.signum();
            if selection.vdir == 0 {
                selection.vdir = dir;
                grow_vertical(&mut selection, dir, rows);
            } else if selection.vdir == dir {
                grow_vertical(&mut selection, dir, rows);
            } else if !shrink_vertical(&mut selection) {
                selection.vdir = 0;
            }
        }

        selection.head = (
            match selection.vdir {
                1 => selection.bounds.bottom,
                -1 => selection.bounds.top,
                _ => selection.anchor.0,
            },
            match selection.hdir {
                1 => selection.bounds.right,
                -1 => selection.bounds.left,
                _ => selection.anchor.1,
            },
        );
        self.selection = Some(selection);
        let head = selection.head;
        self.row = head.0.min(rows - 1);
        self.set_cursor_column(head.1);
        self.sync_edit();
    }

    // --- Edit buffers ---------------------------------------------------------

    /// Finalizes the active cell buffer and opens one for the new cursor cell.
    fn sync_edit(&mut self) {
        if let Some(edit) = self.edit.take() {
            self.commit_edit(edit);
        }
        self.edit_recorded = false;
        if !self.mode.is_editing() {
            return;
        }
        let (row, track) = (self.row, self.track);
        let Some(slot) = self.pattern().slot(row, track) else {
            return;
        };
        let editable = slot.is_empty() || slot.event.inline_hex().is_some();
        if editable {
            self.edit = Some(CellEdit::from_slot(row, track, slot, self.offsets_visible));
        }
    }

    fn commit_edit(&mut self, edit: CellEdit) {
        let event = edit.finalize_event();
        let row_ticks =
            row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
        let existing_offset = self
            .pattern()
            .slot(edit.row, edit.track)
            .map(|slot| slot.offset)
            .unwrap_or(0);
        let offset = if self.offsets_visible {
            edit.finalize_offset(row_ticks)
        } else {
            existing_offset
        };
        if let Some(slot) = self
            .project
            .composition
            .pattern_mut(self.current)
            .and_then(|pattern| pattern.slot_mut(edit.row, edit.track))
        {
            slot.event = event;
            slot.offset = if slot.event.is_empty() { 0 } else { offset };
        }
    }

    fn record_edit_session(&mut self) {
        if !self.edit_recorded {
            self.record_pattern();
            self.edit_recorded = true;
        }
    }

    /// Types one hexadecimal nibble at the cursor.
    pub fn type_nibble(&mut self, value: u8) {
        if self.edit.is_none() {
            self.sync_edit();
        }
        if self.edit.is_none() {
            return;
        }
        self.record_edit_session();
        let (row, track, offset_visible, nibble) = {
            let edit = self.edit.as_mut().expect("edit buffer exists");
            edit.set_nibble(self.nibble, value);
            (edit.row, edit.track, edit.offset_visible, self.nibble)
        };
        // Attempt immediate conversion into the committed slot.
        if let Some(event) = self.edit.as_ref().and_then(|edit| edit.try_event()) {
            if let Some(slot) = self
                .project
                .composition
                .pattern_mut(self.current)
                .and_then(|pattern| pattern.slot_mut(row, track))
            {
                slot.event = event;
            }
        }
        if offset_visible && nibble >= EVENT_NIBBLES {
            let row_ticks =
                row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
            let valid = self
                .edit
                .as_ref()
                .and_then(|edit| edit.try_offset(row_ticks));
            if let Some(edit) = self.edit.as_mut() {
                edit.offset_invalid = valid.is_none();
            }
            if let Some(value) = valid {
                if let Some(slot) = self
                    .project
                    .composition
                    .pattern_mut(self.current)
                    .and_then(|pattern| pattern.slot_mut(row, track))
                {
                    slot.offset = value;
                }
            }
        }
        self.dirty = true;
        // Advance the nibble cursor, wrapping to the next track.
        self.move_right();
        if (self.row, self.track) != (row, track) {
            self.sync_edit();
        }
    }

    fn cancel_selection(&mut self) {
        self.selection = None;
    }

    fn toggle_offsets(&mut self) {
        self.sync_edit();
        self.offsets_visible = !self.offsets_visible;
        if !self.offsets_visible {
            self.brush_width = self.brush_width.hide_offsets();
        }
        self.selection = None;
        self.nibble = 0;
        self.sync_edit();
    }

    // --- Blocks ---------------------------------------------------------------

    fn record_pattern(&mut self) {
        if let Some(pattern) = self.project.composition.pattern(self.current).cloned() {
            self.pattern_undo
                .entry(self.current)
                .or_insert_with(|| UndoStack::new(64))
                .record(pattern);
        }
        self.dirty = true;
    }

    /// Grows an active selection's visible bounds to the operation rectangle.
    fn expand_visible_selection(&mut self, rect: Rect) {
        if let Some(mut selection) = self.selection {
            selection.bounds = rect;
            self.selection = Some(selection);
        }
    }

    fn insert_block(&mut self) {
        let rect = self.cell_rect();
        self.expand_visible_selection(rect);
        let (left, right) = rect.tracks(self.nibbles_per_cell());
        let height = rect.height();
        self.sync_edit();
        self.record_pattern();
        let rows = self.pattern().row_count();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for track in left..=right {
            for row in (rect.top + height..rows).rev() {
                let moved = pattern
                    .slot(row - height, track)
                    .cloned()
                    .unwrap_or_default();
                if let Some(slot) = pattern.slot_mut(row, track) {
                    *slot = moved;
                }
            }
            for row in rect.top..(rect.top + height).min(rows) {
                if let Some(slot) = pattern.slot_mut(row, track) {
                    *slot = Slot::empty();
                }
            }
        }
        self.after_pattern_change_keep_selection();
    }

    fn delete_block(&mut self) {
        let rect = self.cell_rect();
        self.expand_visible_selection(rect);
        let (left, right) = rect.tracks(self.nibbles_per_cell());
        self.delete_rows(rect.top, rect.height(), left, right, true);
    }

    /// Deletes the nibble left of the cursor and steps the cursor back.
    ///
    /// Returns `false` at the first nibble (or when the cell cannot be edited
    /// nibble by nibble), so block Backspace can take over.
    fn backspace_nibble(&mut self) -> bool {
        if self.selection_bounds().is_some() {
            return false;
        }
        if self.nibble == 0 {
            // Nothing is left of the cursor. A cell modified in this session
            // swallows Backspace; untouched cells keep the structural delete.
            return self.edit_recorded;
        }
        if self.edit.is_none() {
            self.sync_edit();
        }
        if self.edit.is_none() {
            return false;
        }
        let (row, track, offset_visible) = {
            let edit = self.edit.as_ref().expect("edit buffer exists");
            (edit.row, edit.track, edit.offset_visible)
        };
        self.record_edit_session();
        let index = self.nibble - 1;
        if let Some(edit) = self.edit.as_mut() {
            edit.clear_nibble(index);
        }
        if let Some(event) = self.edit.as_ref().and_then(|edit| edit.try_event()) {
            if let Some(slot) = self
                .project
                .composition
                .pattern_mut(self.current)
                .and_then(|pattern| pattern.slot_mut(row, track))
            {
                slot.event = event;
            }
        }
        if offset_visible && index >= EVENT_NIBBLES {
            let row_ticks =
                row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
            let valid = self
                .edit
                .as_ref()
                .and_then(|edit| edit.try_offset(row_ticks));
            if let Some(edit) = self.edit.as_mut() {
                edit.offset_invalid = valid.is_none();
            }
            if let Some(value) = valid {
                if let Some(slot) = self
                    .project
                    .composition
                    .pattern_mut(self.current)
                    .and_then(|pattern| pattern.slot_mut(row, track))
                {
                    slot.offset = value;
                }
            }
        }
        self.nibble = index;
        self.dirty = true;
        true
    }

    fn backspace_block(&mut self) {
        let rect = self.cell_rect();
        self.expand_visible_selection(rect);
        let (left, right) = rect.tracks(self.nibbles_per_cell());
        let height = rect.height();
        let start = rect.top.saturating_sub(height);
        // The cells at and below the cursor shift up, so the cursor steps up
        // with them. A selection keeps its anchor and leaves the cursor alone.
        if self.selection.is_none() {
            self.row = self.row.saturating_sub(height);
        }
        self.delete_rows(start, height, left, right, true);
    }

    fn delete_rows(
        &mut self,
        start: usize,
        height: usize,
        left: usize,
        right: usize,
        keep_selection: bool,
    ) {
        self.sync_edit();
        self.record_pattern();
        let rows = self.pattern().row_count();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for track in left..=right {
            for row in start..rows.saturating_sub(height) {
                let moved = pattern
                    .slot(row + height, track)
                    .cloned()
                    .unwrap_or_default();
                if let Some(slot) = pattern.slot_mut(row, track) {
                    *slot = moved;
                }
            }
            for row in rows.saturating_sub(height)..rows {
                if let Some(slot) = pattern.slot_mut(row, track) {
                    *slot = Slot::empty();
                }
            }
        }
        if keep_selection {
            self.after_pattern_change_keep_selection();
        } else {
            self.after_pattern_change();
        }
    }

    /// Clears the byte-aligned rectangle, preserving unselected bytes.
    fn clear_rect(&mut self, rect: Rect) {
        let npc = self.nibbles_per_cell().max(1);
        let offsets_visible = self.offsets_visible;
        let row_ticks =
            row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
        let (left_track, right_track) = rect.tracks(npc);
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for row in rect.top..=rect.bottom {
            for track in left_track..=right_track {
                let cell_start = track * npc;
                let cell_end = cell_start + npc - 1;
                let sel_left = rect.left.max(cell_start);
                let sel_right = rect.right.min(cell_end);
                if sel_left > sel_right {
                    continue;
                }
                let Some(slot) = pattern.slot_mut(row, track) else {
                    continue;
                };
                let full_cell = sel_left == cell_start && sel_right == cell_end;
                let status_selected = sel_left <= cell_start + 1;
                if full_cell || status_selected {
                    *slot = Slot::empty();
                    continue;
                }
                let mut edit = CellEdit::from_slot(row, track, slot, offsets_visible);
                for col in sel_left..=sel_right {
                    let nibble = col - cell_start;
                    if nibble < EVENT_NIBBLES {
                        edit.event[nibble] = Some(0);
                    } else if offsets_visible {
                        edit.offset[nibble - EVENT_NIBBLES] = Some(0);
                    }
                }
                let event = edit.finalize_event();
                let offset = if offsets_visible {
                    edit.finalize_offset(row_ticks)
                } else {
                    slot.offset
                };
                slot.event = event;
                slot.offset = if slot.event.is_empty() { 0 } else { offset };
            }
        }
    }

    fn clear_block(&mut self) {
        let rect = self.operation_rect();
        if self.rect_has_complex(rect) {
            self.set_message("cannot clear a non-inline event".to_string(), true);
            return;
        }
        self.sync_edit();
        self.record_pattern();
        self.clear_rect(rect);
        self.after_pattern_change();
    }

    fn copy(&mut self) {
        let rect = self.operation_rect();
        if self.rect_has_complex(rect) {
            self.set_message("cannot copy a non-inline event".to_string(), true);
            return;
        }
        self.clipboard = NibbleClipboard::from_pattern(
            self.pattern(),
            rect.top,
            rect.bottom,
            rect.left,
            rect.right,
            self.offsets_visible,
        );
        self.set_message(
            format!(
                "copied {}x{} nibbles",
                self.clipboard.height(),
                self.clipboard.width()
            ),
            false,
        );
    }

    fn cut(&mut self) {
        let rect = self.operation_rect();
        if self.rect_has_complex(rect) {
            self.set_message("cannot cut a non-inline event".to_string(), true);
            return;
        }
        self.clipboard = NibbleClipboard::from_pattern(
            self.pattern(),
            rect.top,
            rect.bottom,
            rect.left,
            rect.right,
            self.offsets_visible,
        );
        self.sync_edit();
        self.record_pattern();
        self.clear_rect(rect);
        self.after_pattern_change();
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        self.sync_edit();
        let npc = self.nibbles_per_cell().max(1);
        let offsets_visible = self.offsets_visible;
        let (rows, tracks) = self.pattern_geometry();
        let clip = self.selection_bounds();
        let (origin_row, origin_col) = match clip {
            Some(rect) => (rect.top, rect.left),
            None => (self.row, self.cursor_column()),
        };
        let mut writes: BTreeMap<(usize, usize), CellEdit> = BTreeMap::new();
        for (dr, dest_col, value) in self
            .clipboard
            .mapped_columns(origin_col, npc, offsets_visible)
        {
            let row = origin_row + dr;
            let track = dest_col / npc;
            let dest_nib = dest_col % npc;
            if row >= rows || track >= tracks {
                continue;
            }
            if let Some(rect) = clip {
                if row < rect.top
                    || row > rect.bottom
                    || dest_col < rect.left
                    || dest_col > rect.right
                {
                    continue;
                }
            }
            let entry = writes
                .entry((row, track))
                .or_insert_with(|| destination_edit(self.pattern(), row, track, offsets_visible));
            entry.set_nibble(dest_nib, value);
        }
        if writes.is_empty() {
            return;
        }
        for &(row, track) in writes.keys() {
            if let Some(slot) = self.pattern().slot(row, track) {
                if !slot.is_empty() && slot.event.inline_hex().is_none() {
                    self.set_message("paste would overwrite a non-inline event".to_string(), true);
                    return;
                }
            }
        }
        self.record_pattern();
        let row_ticks =
            row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
        let offsets_included = self.clipboard.offsets_included;
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for ((row, track), edit) in writes {
            let event = edit.finalize_event();
            let existing = pattern
                .slot(row, track)
                .map(|slot| slot.offset)
                .unwrap_or(0);
            let offset = if offsets_included && offsets_visible {
                edit.finalize_offset(row_ticks)
            } else {
                existing
            };
            if let Some(slot) = pattern.slot_mut(row, track) {
                slot.event = event;
                slot.offset = if slot.event.is_empty() { 0 } else { offset };
            }
        }
        self.after_pattern_change();
    }

    fn insert_track(&mut self) {
        let tracks = self.pattern().track_count();
        self.sync_edit();
        self.record_pattern();
        self.cancel_selection();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let _ = pattern.resize_tracks(tracks + 1);
        self.clamp_cursor();
    }

    fn delete_track(&mut self) {
        let tracks = self.pattern().track_count();
        if tracks <= 1 {
            return;
        }
        self.sync_edit();
        self.record_pattern();
        self.cancel_selection();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let _ = pattern.resize_tracks(tracks - 1);
        self.clamp_cursor();
        self.sync_edit();
    }

    fn after_pattern_change(&mut self) {
        self.finish_pattern_change(false);
    }

    fn after_pattern_change_keep_selection(&mut self) {
        self.finish_pattern_change(true);
    }

    fn finish_pattern_change(&mut self, keep_selection: bool) {
        self.clamp_cursor();
        if !keep_selection {
            self.selection = None;
        }
        self.edit = None;
        self.edit_recorded = false;
        self.sync_edit();
        self.dirty = true;
    }

    // --- Note entry -----------------------------------------------------------

    /// Handles a note key in PLAY or REC, returning whether it was consumed.
    pub fn note_key(&mut self, c: char) -> bool {
        match self.mode {
            Mode::Play => {
                let Some(note) = self.note_for_key(c) else {
                    return false;
                };
                let (channel, velocity) = self.note_defaults();
                self.audition(note, channel, velocity);
                true
            }
            Mode::Rec => {
                let Some(note) = self.note_for_key(c) else {
                    return false;
                };
                let (channel, velocity) = self.note_defaults();
                // Send first so live recording cannot be delayed by the
                // future-stream rebuild.
                self.audition(note, channel, velocity);
                if self.playing {
                    self.record_live(note, channel, velocity);
                } else {
                    self.record_step(note, channel, velocity);
                }
                true
            }
            _ => false,
        }
    }

    /// Resolves a keyboard character to a MIDI note in the selected scale.
    fn note_for_key(&self, c: char) -> Option<u8> {
        let settings = self.effective_settings();
        let mapping = if settings.is_chromatic() {
            chromatic_map()
        } else {
            scale_map()
        };
        mapping
            .iter()
            .find(|(key, _)| *key == c)
            .map(|(_, degree)| settings.degree_to_note(*degree))
            .map(|note| (i32::from(note) + self.transpose).clamp(0, 127) as u8)
    }

    /// Writes one stopped-REC note and advances by the recording step.
    fn record_step(&mut self, note: u8, channel: u8, velocity: u8) {
        let (rows, _) = self.pattern_geometry();
        self.record_pattern();
        self.edit = None;
        self.edit_recorded = false;
        let event = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            channel,
            note,
            Some(velocity),
        ));
        if let Some(slot) = self
            .project
            .composition
            .pattern_mut(self.current)
            .and_then(|pattern| pattern.slot_mut(self.row, self.track))
        {
            slot.event = event;
            slot.offset = 0;
        }
        self.dirty = true;
        self.row = self
            .row
            .saturating_add(usize::from(self.record_step))
            .min(rows.saturating_sub(1));
    }

    /// Writes one live-REC note at the current playhead position.
    fn record_live(&mut self, note: u8, channel: u8, velocity: u8) {
        let (rows, _) = self.pattern_geometry();
        let row = self.play_current_row.min(rows.saturating_sub(1));
        let row_len = row_ticks(self.project.composition.tpb(), self.pattern().lpb()).unwrap_or(1);
        let row_start = row as u64 * row_len as u64;
        let offset = self.play_tick.saturating_sub(row_start).min(row_len as u64) as u32;
        self.row = row;
        self.record_pattern();
        self.edit = None;
        self.edit_recorded = false;
        let event = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            channel,
            note,
            Some(velocity),
        ));
        if let Some(slot) = self
            .project
            .composition
            .pattern_mut(self.current)
            .and_then(|pattern| pattern.slot_mut(row, self.track))
        {
            slot.event = event;
            slot.offset = offset;
        }
        self.dirty = true;
        self.rebuild_future_stream();
    }

    /// Discards the not-yet-emitted tail and re-renders from the current tick.
    ///
    /// Every event at or before `play_tick` has already been sent, so the
    /// rebuilt stream starts strictly after it. This removes replaced pending
    /// events and macro-generated descendants, and never resends the note that
    /// live recording just auditioned.
    fn rebuild_future_stream(&mut self) {
        if !self.playing {
            return;
        }
        let events = self.render();
        self.play_next = events
            .iter()
            .position(|event| event.tick > self.play_tick)
            .unwrap_or(events.len());
        self.play_events = events;
    }

    /// The channel and velocity a new note inherits.
    ///
    /// Each value is found independently by scanning the selected track from
    /// the current row toward earlier rows. A channel-voice event supplies its
    /// channel; a note-on/note-off also supplies its velocity. Missing parts
    /// fall back to channel 0 and velocity 100.
    fn note_defaults(&self) -> (u8, u8) {
        let mut channel = 0;
        let mut velocity = 100;
        let mut channel_found = false;
        let mut velocity_found = false;
        for row in (0..=self.row).rev() {
            let Some(slot) = self.pattern().slot(row, self.track) else {
                continue;
            };
            let Event::Channel(message) = &slot.event else {
                continue;
            };
            if !channel_found {
                if let Some(value) = message.channel {
                    channel = value;
                    channel_found = true;
                }
            }
            if !velocity_found && matches!(message.kind, ChannelKind::NoteOn | ChannelKind::NoteOff)
            {
                if let Some(value) = message.data2 {
                    velocity = value;
                    velocity_found = true;
                }
            }
            if channel_found && velocity_found {
                break;
            }
        }
        (channel, velocity)
    }

    /// Plays a note while entering it.
    ///
    /// Only the note-on is sent: a note-off right behind it would cut the note
    /// off before it is heard. A note that keeps ringing can be silenced with
    /// Alt+. (All Notes Off).
    fn audition(&mut self, note: u8, channel: u8, velocity: u8) {
        let on = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            channel,
            note,
            Some(velocity),
        ));
        self.send(&on);
    }

    fn send(&mut self, event: &Event) {
        if let Some(midi) = self.midi.as_mut() {
            let _ = midi.send_event(event);
        }
    }

    /// Sends an All Notes Off control change on every MIDI channel.
    pub fn all_notes_off(&mut self) {
        let Some(midi) = self.midi.as_mut() else {
            return;
        };
        for channel in 0..16u8 {
            let event = Event::Channel(ChannelMessage::with_data(
                ChannelKind::ControlChange,
                channel,
                123,
                Some(0),
            ));
            let _ = midi.send_event(&event);
        }
    }

    // --- Undo -----------------------------------------------------------------

    fn undo(&mut self) {
        match self.screen {
            Screen::Pattern => self.undo_pattern(),
            Screen::Code => self.undo_module(),
            Screen::Help => {}
        }
    }

    fn redo(&mut self) {
        match self.screen {
            Screen::Pattern => self.redo_pattern(),
            Screen::Code => self.redo_module(),
            Screen::Help => {}
        }
    }

    fn undo_pattern(&mut self) {
        let id = self.current;
        let current = self.project.composition.pattern(id).cloned();
        let Some(current) = current else { return };
        let previous = self
            .pattern_undo
            .get_mut(&id)
            .and_then(|stack| stack.undo(current));
        if let Some(previous) = previous {
            if let Some(slot) = self.project.composition.pattern_mut(id) {
                *slot = previous;
            }
            self.after_pattern_change();
            if self.playing {
                self.rebuild_future_stream();
            }
        }
    }

    fn redo_pattern(&mut self) {
        let id = self.current;
        let current = self.project.composition.pattern(id).cloned();
        let Some(current) = current else { return };
        let next = self
            .pattern_undo
            .get_mut(&id)
            .and_then(|stack| stack.redo(current));
        if let Some(next) = next {
            if let Some(slot) = self.project.composition.pattern_mut(id) {
                *slot = next;
            }
            self.after_pattern_change();
            if self.playing {
                self.rebuild_future_stream();
            }
        }
    }

    fn record_module(&mut self, id: u8) {
        if self.module_readonly(id) {
            return;
        }
        if let Some(source) = self.project.modules.get(&id).cloned() {
            self.module_undo
                .entry(id)
                .or_insert_with(|| UndoStack::new(64))
                .record(source);
        }
        self.dirty = true;
    }

    fn undo_module(&mut self) {
        let id = self.module_id;
        let current = self.code.text();
        let previous = self
            .module_undo
            .get_mut(&id)
            .and_then(|stack| stack.undo(current));
        if let Some(previous) = previous {
            self.code = CodeEditor::from_text(&previous);
            self.project.modules.insert(id, previous);
            self.lua_dirty = true;
            self.dirty = true;
        }
    }

    fn redo_module(&mut self) {
        let id = self.module_id;
        let current = self.code.text();
        let next = self
            .module_undo
            .get_mut(&id)
            .and_then(|stack| stack.redo(current));
        if let Some(next) = next {
            self.code = CodeEditor::from_text(&next);
            self.project.modules.insert(id, next);
            self.lua_dirty = true;
            self.dirty = true;
        }
    }

    // --- Command layer --------------------------------------------------------

    // --- Modes and playback pause --------------------------------------------

    /// Switches to a musical or transient mode.
    ///
    /// An active cell edit is finalized exactly once. PLAY/REC drop selection
    /// state they cannot use and snap the cursor row to the playhead while
    /// playback runs; EDIT recreates the edit buffer at the preserved cursor.
    pub fn set_mode(&mut self, mode: Mode) {
        if let Some(edit) = self.edit.take() {
            self.commit_edit(edit);
        }
        self.edit_recorded = false;
        if mode.follows_playback() {
            self.selection = None;
        }
        self.mode = mode;
        if mode.follows_playback() && self.playing {
            let (rows, _) = self.pattern_geometry();
            self.row = self.play_current_row.min(rows.saturating_sub(1));
        }
    }

    /// Pauses playback for command mode, preserving its exact position.
    fn pause_for_command(&mut self) {
        if !self.playing {
            return;
        }
        self.all_notes_off();
        self.command_pause = Some(PlaybackPause {
            tick: self.play_tick,
            micros: self.play_micros,
            start_row: self.play_start_row,
            current_row: self.play_current_row,
        });
        self.playing = false;
    }

    /// Restores the paused playback position and rebuilds the future stream.
    fn resume_after_command(&mut self) {
        let Some(paused) = self.command_pause.take() else {
            return;
        };
        self.play_tick = paused.tick;
        self.play_micros = paused.micros;
        self.play_start_row = paused.start_row;
        self.play_current_row = paused.current_row;
        let tpb = self.project.composition.tpb();
        let row_len = row_ticks(tpb, self.pattern().lpb()).unwrap_or(1) as u64;
        self.play_end_tick = self.pattern().row_count() as u64 * row_len;
        let events = self.render();
        self.play_next = events
            .iter()
            .position(|event| event.tick > self.play_tick)
            .unwrap_or(events.len());
        self.play_events = events;
        self.playing = true;
        if self.mode.follows_playback() {
            let (rows, _) = self.pattern_geometry();
            self.row = self.play_current_row.min(rows.saturating_sub(1));
        }
    }

    fn enter_command_mode(&mut self) {
        if self.mode.is_musical() {
            self.previous_mode = self.mode;
        }
        self.command_failed = false;
        self.path_completion = None;
        self.input.clear();
        self.set_mode(Mode::Command);
        self.pause_for_command();
    }

    fn open_event_command(&mut self) {
        let (row, track) = (self.row, self.track);
        let Some(slot) = self.pattern().slot(row, track) else {
            return;
        };
        if slot.is_empty() {
            return;
        }
        let Ok(bytes) = slot.event.to_event_bytes() else {
            self.set_message("this event has no editable byte form".to_string(), true);
            return;
        };
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in &bytes {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02X}");
        }
        // The status line draws the leading colon, so the buffer holds the
        // command body only.
        let text = format!("event {hex}");
        if self.mode.is_musical() {
            self.previous_mode = self.mode;
        }
        self.command_failed = false;
        self.path_completion = None;
        self.input = LineEditor::from_text(&text);
        // Place the cursor on the first event nibble (after "event ").
        self.input.set_cursor("event ".len());
        self.set_mode(Mode::Command);
        self.pause_for_command();
    }

    /// Executes a parsed command.
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Read(path) => return self.load(path),
            Command::Write(path) => return self.save(path),
            Command::Export(path) => return self.export(path),
            Command::Event(bytes) => return self.commit_event_bytes(&bytes),
            Command::Step(value) => {
                self.record_step = u8::try_from(value)
                    .map_err(|_| format!("recording step is too large: {value}"))?;
                return Ok(());
            }
            Command::Transpose(value) => {
                match value {
                    Some(value) => {
                        if !(MIN_TRANSPOSE..=MAX_TRANSPOSE).contains(&value) {
                            return Err(format!(
                                "transpose out of range [{MIN_TRANSPOSE}, {MAX_TRANSPOSE}]: {value}"
                            ));
                        }
                        self.set_transpose(value);
                        self.set_message(format!("transpose {:+}", self.transpose), false);
                    }
                    None => self.set_message(format!("transpose {:+}", self.transpose), false),
                }
                return Ok(());
            }
            Command::Bpm(value) => {
                let bpm = Bpm::from_int(value).ok_or_else(|| format!("invalid bpm: {value}"))?;
                self.project.composition.set_bpm(bpm);
            }
            Command::Tpb(value) => {
                self.project
                    .composition
                    .raise_tpb(value)
                    .map_err(|e| e.to_string())?;
            }
            Command::Lpb(value) => {
                self.sync_edit();
                self.record_pattern();
                self.project
                    .composition
                    .set_pattern_lpb(self.current, value)
                    .map_err(|e| e.to_string())?;
            }
            Command::Root(note) => {
                let midi = mtrak_core::settings::parse_note(&note)
                    .ok_or_else(|| format!("invalid note: {note}"))?;
                self.project.composition.global_settings_mut().root = midi;
            }
            Command::Scale(args) => return self.execute_scale(args),
            Command::Mode(value) => {
                self.project.composition.global_settings_mut().modal_degree = value as u8;
            }
            Command::Rows(value) => {
                self.sync_edit();
                self.record_pattern();
                self.project
                    .composition
                    .pattern_mut(self.current)
                    .ok_or_else(|| "no pattern".to_string())?
                    .resize_rows(value as usize)
                    .map_err(|e| e.to_string())?;
                self.after_pattern_change();
            }
            Command::Tracks(value) => {
                self.sync_edit();
                self.record_pattern();
                self.project
                    .composition
                    .pattern_mut(self.current)
                    .ok_or_else(|| "no pattern".to_string())?
                    .resize_tracks(value as usize)
                    .map_err(|e| e.to_string())?;
                self.after_pattern_change();
            }
            Command::PatternNew(lpb) => {
                let id = self
                    .project
                    .composition
                    .create_pattern(lpb.unwrap_or(4))
                    .map_err(|e| e.to_string())?;
                self.select_pattern(id);
            }
            Command::PatternSelect(id) => {
                if self.project.composition.pattern(id).is_none() {
                    return Err(format!("pattern {id} does not exist"));
                }
                self.select_pattern(id);
            }
            Command::PatternDelete(id) => {
                self.project
                    .composition
                    .delete_pattern(id)
                    .map_err(|e| e.to_string())?;
                self.pattern_undo.remove(&id);
                if self.current == id {
                    self.current = self.project.composition.selected_main();
                }
                self.sync_edit();
            }
            Command::PatternMain(id) => {
                self.project
                    .composition
                    .select_main(id)
                    .map_err(|e| e.to_string())?;
            }
            Command::PortList => {
                let ports: Vec<PortInfo> = match self.midi.as_ref() {
                    Some(midi) => midi.list_outputs().map_err(|e: MidiError| e.to_string())?,
                    None => Vec::new(),
                };
                let text = if ports.is_empty() {
                    "no MIDI ports".to_string()
                } else {
                    ports
                        .iter()
                        .map(|p| format!("{}: {}", p.id, p.name))
                        .collect::<Vec<_>>()
                        .join("  ")
                };
                self.set_message(text, false);
            }
            Command::PortSelect(id) => {
                let midi = self
                    .midi
                    .as_mut()
                    .ok_or_else(|| "no MIDI backend".to_string())?;
                midi.select(id).map_err(|e| e.to_string())?;
            }
            Command::Seed(arg) => match arg {
                SeedArg::Show => {
                    self.set_message(
                        format!("seed 0x{:016X}", self.project.composition.seed()),
                        false,
                    );
                    return Ok(());
                }
                SeedArg::Set(value) => {
                    self.project.composition.set_seed(value);
                }
                SeedArg::Random => {
                    let seed = fresh_seed();
                    self.project.composition.set_seed(seed);
                    self.set_message(format!("seed 0x{seed:016X}"), false);
                }
            },
            Command::Help => {
                self.set_message(
                    "read write export bpm tpb lpb root scale mode rows tracks pattern port event step transpose seed"
                        .to_string(),
                    false,
                );
            }
        }
        self.dirty = true;
        Ok(())
    }

    fn commit_event_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let event = Event::from_event_bytes(bytes).map_err(|e| e.to_string())?;
        let (row, track) = (self.row, self.track);
        self.sync_edit();
        self.record_pattern();
        if let Some(slot) = self
            .project
            .composition
            .pattern_mut(self.current)
            .and_then(|pattern| pattern.slot_mut(row, track))
        {
            slot.event = event;
            if slot.event.is_empty() {
                slot.offset = 0;
            }
        }
        self.after_pattern_change();
        Ok(())
    }

    fn execute_scale(&mut self, args: ScaleArgs) -> Result<(), String> {
        if let Some((name, mask)) = args.add {
            self.project
                .composition
                .scales_mut()
                .add(&name, mtrak_core::settings::Scale::from_mask(mask));
        }
        if let Some(name) = args.delete {
            self.project.composition.scales_mut().remove(&name);
        }
        if let Some(name) = args.scale {
            let scale = self
                .project
                .composition
                .scales()
                .get(&name)
                .ok_or_else(|| format!("unknown scale: {name}"))?;
            self.project.composition.global_settings_mut().scale = scale;
        }
        if let Some(name) = args.use_name {
            let scale = self
                .project
                .composition
                .scales()
                .get(&name)
                .ok_or_else(|| format!("unknown scale: {name}"))?;
            self.project.composition.global_settings_mut().scale = scale;
        }
        if let Some(note) = args.root {
            let midi = mtrak_core::settings::parse_note(&note)
                .ok_or_else(|| format!("invalid note: {note}"))?;
            self.project.composition.global_settings_mut().root = midi;
        }
        if let Some(mode) = args.mode {
            self.project.composition.global_settings_mut().modal_degree = mode as u8;
        }
        if args.list {
            let mut names: Vec<String> = mtrak_core::settings::BUILTIN_SCALES
                .iter()
                .map(|s| s.name.to_string())
                .collect();
            names.extend(
                self.project
                    .composition
                    .scales()
                    .iter()
                    .map(|(n, _)| n.to_string()),
            );
            self.set_message(names.join(" "), false);
        }
        self.dirty = true;
        Ok(())
    }

    /// Completes the path under the cursor, opening its menu when ambiguous.
    fn complete_input(&mut self) {
        let text = self.input.text();
        let cursor = self.input.cursor();
        let Some(found) = crate::complete::path_candidates(&text, cursor) else {
            return;
        };
        match found.values.len() {
            0 => {}
            1 => {
                self.replace_input_range(found.start, found.end, &found.values[0]);
            }
            _ => {
                let shared = crate::complete::shared_prefix(&found.values);
                let end = if shared.chars().count() > found.fragment.chars().count() {
                    self.replace_input_range(found.start, found.end, &shared)
                } else {
                    found.end
                };
                let dir_prefix = match found.fragment.rfind('/') {
                    Some(index) => found.fragment[..=index].to_string(),
                    None => String::new(),
                };
                self.path_completion = Some(PathCompletion {
                    start: found.start,
                    end,
                    dir_prefix,
                    matches: (0..found.values.len()).collect(),
                    all: found.values,
                    selected: 0,
                });
            }
        }
    }

    /// Replaces characters `start..end` of the command line, returning the new
    /// cursor position.
    fn replace_input_range(&mut self, start: usize, end: usize, value: &str) -> usize {
        let chars: Vec<char> = self.input.text().chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len()).max(start);
        let mut out: Vec<char> = chars[..start].to_vec();
        out.extend(value.chars());
        out.extend(chars[end..].iter());
        let cursor = start + value.chars().count();
        self.input = LineEditor::from_text(&out.into_iter().collect::<String>());
        self.input.set_cursor(cursor);
        cursor
    }

    fn execute_command_line(&mut self) {
        let input = self.input.text();
        match command::parse(&input) {
            Ok(command) => match self.execute(command) {
                Ok(()) => {
                    self.command_failed = false;
                    self.input.clear();
                    self.restore_previous_mode();
                }
                Err(error) => {
                    self.command_failed = true;
                    self.set_message(error, true);
                }
            },
            Err(error) => {
                self.command_failed = true;
                self.set_message(error, true);
            }
        }
    }

    fn set_message(&mut self, text: String, is_error: bool) {
        self.message = Some(Message { text, is_error });
    }

    // --- Workspaces -----------------------------------------------------------

    fn switch_screen(&mut self, screen: Screen) {
        if self.screen == Screen::Pattern {
            self.sync_edit();
        }
        if self.screen == Screen::Code {
            self.sync_module_text();
        }
        self.screen = screen;
    }

    // --- Chooser --------------------------------------------------------------

    fn open_chooser(&mut self) {
        let kind = match self.screen {
            Screen::Pattern => ChooserKind::Pattern,
            Screen::Code => ChooserKind::Code,
            Screen::Help => return,
        };
        if self.screen == Screen::Help {
            return;
        }
        if self.mode.is_musical() {
            self.previous_mode = self.mode;
        }
        let cursor = match kind {
            ChooserKind::Pattern => self.current.value(),
            ChooserKind::Code => self.module_id,
        };
        self.set_mode(Mode::Chooser);
        self.chooser = Some(Chooser::new(kind, cursor));
    }

    /// The open chooser, if any.
    pub fn chooser(&self) -> Option<&Chooser> {
        self.chooser.as_ref()
    }

    /// Whether a pattern with `id` exists.
    pub fn pattern_exists(&self, id: u8) -> bool {
        self.project.composition.pattern(PatternId(id)).is_some()
    }

    /// The source of a module (built-in or stored), if available.
    pub fn module_source(&self, id: u8) -> Option<String> {
        module_source(&self.project, id)
    }

    /// Whether a module is a read-only built-in.
    pub fn module_readonly(&self, id: u8) -> bool {
        is_builtin_module(id) && builtin_module(id).is_some()
    }

    /// Whether a module cell is selectable (defined built-in, setup, or user).
    pub fn module_available(&self, id: u8) -> bool {
        if is_reserved_module(id) {
            return false;
        }
        if id == 0 {
            return true;
        }
        if is_builtin_module(id) {
            return builtin_module(id).is_some();
        }
        true
    }

    /// The current module ID.
    pub fn module_id(&self) -> u8 {
        self.module_id
    }

    /// The Code editor.
    pub fn code_editor(&self) -> &CodeEditor {
        &self.code
    }

    /// The Code viewport scroll offset.
    pub fn code_scroll(&self) -> usize {
        self.code_scroll
    }

    /// Sets the Code viewport scroll offset.
    pub fn set_code_scroll(&mut self, scroll: usize) {
        self.code_scroll = scroll;
    }

    fn accept_chooser(&mut self) {
        let Some(chooser) = self.chooser.take() else {
            return;
        };
        self.set_mode(self.previous_mode);
        let id = chooser.cursor;
        match chooser.kind {
            ChooserKind::Pattern => {
                if self.project.composition.pattern(PatternId(id)).is_none() {
                    self.pattern_struct_undo
                        .record(self.project.composition.clone());
                    if let Err(error) = self.project.composition.create_pattern_at(PatternId(id), 4)
                    {
                        self.set_message(error.to_string(), true);
                        return;
                    }
                }
                self.select_pattern(PatternId(id));
            }
            ChooserKind::Code => {
                if !self.module_available(id) {
                    self.set_message(format!("module {id:02X} is unavailable"), true);
                    return;
                }
                self.select_module(id);
            }
        }
    }

    fn select_pattern(&mut self, id: PatternId) {
        self.sync_edit();
        self.current = id;
        self.offsets_visible = false;
        self.row = 0;
        self.track = 0;
        self.nibble = 0;
        self.selection = None;
        self.sync_edit();
    }

    /// Selects a module for the Code screen, creating user modules lazily.
    pub fn select_module(&mut self, id: u8) {
        if !self.module_available(id) {
            self.set_message(format!("module {id:02X} is unavailable"), true);
            return;
        }
        if !self.module_readonly(id) && !self.project.modules.contains_key(&id) {
            self.project.modules.insert(id, MODULE_TEMPLATE.to_string());
        }
        let source = self.module_source(id).unwrap_or_default();
        self.module_id = id;
        self.code = CodeEditor::from_text(&source);
        self.code_scroll = 0;
    }

    fn chooser_copy(&mut self) {
        let Some(chooser) = self.chooser.as_ref() else {
            return;
        };
        let id = chooser.cursor;
        match chooser.kind {
            ChooserKind::Pattern => {
                if let Some(pattern) = self.project.composition.pattern(PatternId(id)).cloned() {
                    self.chooser_clipboard = Some(ChooserClipboard::Pattern(pattern));
                    self.set_message(format!("copied pattern {id:02X}"), false);
                }
            }
            ChooserKind::Code => {
                if let Some(source) = self.module_source(id) {
                    self.chooser_clipboard = Some(ChooserClipboard::Module { id, source });
                    self.set_message(format!("copied module {id:02X}"), false);
                }
            }
        }
    }

    fn chooser_cut(&mut self) {
        let Some(chooser) = self.chooser.as_ref() else {
            return;
        };
        let id = chooser.cursor;
        match chooser.kind {
            ChooserKind::Pattern => {
                if PatternId(id) == self.project.composition.selected_main() {
                    self.set_message("the main pattern is protected".to_string(), true);
                    return;
                }
                self.chooser_copy();
                self.pattern_struct_undo
                    .record(self.project.composition.clone());
                if self
                    .project
                    .composition
                    .delete_pattern(PatternId(id))
                    .is_ok()
                {
                    self.pattern_undo.remove(&PatternId(id));
                }
            }
            ChooserKind::Code => {
                if self.module_readonly(id) {
                    self.set_message("built-in modules are read-only".to_string(), true);
                    return;
                }
                self.chooser_copy();
                self.code_struct_undo.record(self.project.modules.clone());
                self.project.modules.remove(&id);
                self.module_undo.remove(&id);
            }
        }
    }

    fn chooser_paste(&mut self) {
        let Some(chooser) = self.chooser.as_ref() else {
            return;
        };
        let dest = chooser.cursor;
        let kind = chooser.kind;
        let Some(clipboard) = self.chooser_clipboard.clone() else {
            return;
        };
        match (kind, clipboard) {
            (ChooserKind::Pattern, ChooserClipboard::Pattern(pattern)) => {
                if PatternId(dest) == self.project.composition.selected_main() {
                    self.set_message("the main pattern is protected".to_string(), true);
                    return;
                }
                self.pattern_struct_undo
                    .record(self.project.composition.clone());
                let replaced = pattern.with_id(PatternId(dest));
                if self.project.composition.replace_pattern(replaced).is_ok() {
                    self.pattern_undo.remove(&PatternId(dest));
                    self.set_message(format!("pasted pattern into {dest:02X}"), false);
                }
            }
            (ChooserKind::Code, ChooserClipboard::Module { source, .. }) => {
                if self.module_readonly(dest) || is_reserved_module(dest) {
                    self.set_message(format!("module {dest:02X} cannot be overwritten"), true);
                    return;
                }
                self.code_struct_undo.record(self.project.modules.clone());
                self.project.modules.insert(dest, source);
                self.module_undo.remove(&dest);
                self.set_message(format!("pasted module into {dest:02X}"), false);
            }
            _ => {
                self.set_message("clipboard type does not match".to_string(), true);
            }
        }
    }

    fn chooser_undo(&mut self) {
        let kind = self.chooser.as_ref().map(|c| c.kind);
        match kind {
            Some(ChooserKind::Pattern) => {
                let current = self.project.composition.clone();
                if let Some(previous) = self.pattern_struct_undo.undo(current) {
                    self.project.composition = previous;
                    if self.project.composition.pattern(self.current).is_none() {
                        self.current = self.project.composition.selected_main();
                    }
                }
            }
            Some(ChooserKind::Code) => {
                let current = self.project.modules.clone();
                if let Some(previous) = self.code_struct_undo.undo(current) {
                    self.project.modules = previous;
                }
            }
            None => {}
        }
    }

    fn chooser_redo(&mut self) {
        let kind = self.chooser.as_ref().map(|c| c.kind);
        match kind {
            Some(ChooserKind::Pattern) => {
                let current = self.project.composition.clone();
                if let Some(next) = self.pattern_struct_undo.redo(current) {
                    self.project.composition = next;
                    if self.project.composition.pattern(self.current).is_none() {
                        self.current = self.project.composition.selected_main();
                    }
                }
            }
            Some(ChooserKind::Code) => {
                let current = self.project.modules.clone();
                if let Some(next) = self.code_struct_undo.redo(current) {
                    self.project.modules = next;
                }
            }
            None => {}
        }
    }

    // --- Code editing ---------------------------------------------------------

    fn sync_module_text(&mut self) {
        if self.module_readonly(self.module_id) {
            return;
        }
        self.project
            .modules
            .insert(self.module_id, self.code.text());
        self.lua_dirty = true;
    }

    fn code_mutate(&mut self, mutate: impl FnOnce(&mut CodeEditor)) {
        if self.module_readonly(self.module_id) {
            self.set_message("built-in modules are read-only".to_string(), true);
            return;
        }
        self.record_module(self.module_id);
        mutate(&mut self.code);
        self.sync_module_text();
        self.dirty = true;
    }

    /// Validates and reloads the project Lua runtime.
    pub fn reload_lua(&mut self) {
        self.sync_module_text();
        match self.macro_source.as_mut() {
            Some(source) => match source.reload(&self.project.modules) {
                Ok(commands) => {
                    self.lua_commands = commands;
                    self.lua_dirty = false;
                    self.set_message("Lua runtime reloaded".to_string(), false);
                }
                Err(error) => {
                    self.set_message(format!("Lua error: {error}"), true);
                }
            },
            None => {
                self.set_message("no Lua runtime is available".to_string(), true);
            }
        }
    }

    fn ensure_lua(&mut self) -> Result<(), String> {
        if !self.lua_dirty {
            return Ok(());
        }
        self.sync_module_text();
        match self.macro_source.as_mut() {
            Some(source) => {
                let commands = source.reload(&self.project.modules)?;
                self.lua_commands = commands;
                self.lua_dirty = false;
                Ok(())
            }
            None => {
                self.lua_dirty = false;
                Ok(())
            }
        }
    }

    // --- File operations ------------------------------------------------------

    fn resolve_path(&self, given: Option<String>) -> Result<PathBuf, String> {
        match given {
            Some(path) => Ok(crate::path::expand_tilde(&path)),
            None => self
                .path
                .clone()
                .ok_or_else(|| "no filename given".to_string()),
        }
    }

    /// Saves the project as a tracker file.
    pub fn save(&mut self, path: Option<String>) -> Result<(), String> {
        let path = self.resolve_path(path)?;
        self.ensure_lua()?;
        let factory = self.macro_source.as_ref().and_then(|s| s.factory());
        let bytes =
            mtrak_midi_io::smf::save_project(&self.project, factory).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        self.path = Some(path.clone());
        self.dirty = false;
        self.set_message(format!("wrote {}", path.display()), false);
        Ok(())
    }

    /// Exports standard MIDI.
    pub fn export(&mut self, path: Option<String>) -> Result<(), String> {
        let path = self.resolve_path(path)?;
        self.ensure_lua()?;
        let factory = self.macro_source.as_ref().and_then(|s| s.factory());
        let bytes = mtrak_midi_io::smf::export_standard(&self.project.composition, factory)
            .map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        self.set_message(format!("exported {}", path.display()), false);
        Ok(())
    }

    /// Loads a tracker project or standard MIDI file.
    pub fn load(&mut self, path: Option<String>) -> Result<(), String> {
        let path = self.resolve_path(path)?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let loaded = mtrak_midi_io::smf::load(&bytes).map_err(|e| e.to_string())?;
        self.project = loaded.project;
        self.project.ensure_setup_module();
        self.current = self.project.composition.selected_main();
        self.row = 0;
        self.track = 0;
        self.nibble = 0;
        self.selection = None;
        self.offsets_visible = false;
        self.pattern_undo.clear();
        self.module_undo.clear();
        self.module_id = 0;
        self.code =
            CodeEditor::from_text(&self.project.modules.get(&0).cloned().unwrap_or_default());
        self.path = Some(path.clone());
        self.dirty = false;
        self.project_revision += 1;
        self.lua_dirty = true;
        self.lua_commands.clear();
        self.edit = None;
        let _ = self.ensure_lua();
        self.set_message(
            format!(
                "loaded {} ({})",
                path.display(),
                if loaded.was_project {
                    "project"
                } else {
                    "MIDI import"
                }
            ),
            false,
        );
        Ok(())
    }

    // --- Playback -------------------------------------------------------------

    /// Renders the current main pattern to timestamped events.
    pub fn render(&self) -> Vec<TimedEvent> {
        let factory = self.macro_source.as_ref().and_then(|s| s.factory());
        mtrak_midi_io::smf::render_events(&self.project.composition, factory)
    }

    /// Starts or stops playback.
    pub fn toggle_playback(&mut self) {
        if self.playing {
            self.stop_playback_and_release();
        } else {
            self.start_playback();
        }
    }

    /// Starts playback from the configured start row.
    pub fn start_playback(&mut self) {
        if self.ensure_lua().is_err() {
            return;
        }
        let events = self.render();
        let tpb = self.project.composition.tpb();
        let row_len = row_ticks(tpb, self.pattern().lpb()).unwrap_or(1) as u64;
        let base = self.play_start_row as u64 * row_len;
        self.play_end_tick = self.pattern().row_count() as u64 * row_len;
        self.play_tick = base;
        self.play_current_row = self.play_start_row;
        self.play_next = events
            .iter()
            .position(|e| e.tick >= base)
            .unwrap_or(events.len());
        self.play_events = events;
        self.play_micros = 0.0;
        self.playing = true;
        if self.mode.follows_playback() {
            self.row = self
                .play_start_row
                .min(self.pattern().row_count().saturating_sub(1));
        }
    }

    /// Clears playback state without touching the synth.
    ///
    /// A stop that only ends a pattern does not panic: a macro can start a
    /// child pattern that keeps sounding. Reaching the end of *everything*
    /// releases notes — see [`Self::tick_playback`] and
    /// [`Self::stop_playback_and_release`].
    pub fn stop_playback(&mut self) {
        let follow = self.mode.follows_playback();
        let (rows, _) = self.pattern_geometry();
        let stopping_row = self.play_current_row.min(rows.saturating_sub(1));
        self.playing = false;
        self.play_events.clear();
        self.play_next = 0;
        self.play_end_tick = 0;
        if follow {
            self.row = stopping_row;
            self.selection = None;
        }
    }

    /// Whether another pattern is still sounding when this one ends.
    ///
    /// A trigger starts a child owned by the triggering track, and the engine
    /// removes a child together with its parent (D-008), so a child cannot
    /// outlive its parent: the timeline never holds events past the pattern's
    /// last row. This is therefore always false today. It is kept as a guard so
    /// that a longer child would not be silenced mid-note if the engine ever
    /// allows one to outlive its parent.
    pub fn other_patterns_playing(&self) -> bool {
        self.playing
            && self
                .play_events
                .get(self.play_next)
                .is_some_and(|event| event.tick >= self.play_end_tick)
    }

    /// The tick playback must reach: the end of the pattern's rows, or the
    /// last event when a child pattern runs past it.
    fn playback_horizon(&self) -> u64 {
        self.play_events.last().map_or(self.play_end_tick, |event| {
            event.tick.max(self.play_end_tick)
        })
    }

    /// Stops playback and releases every sounding note.
    ///
    /// This is the `Alt+.` panic routine (All Notes Off on every channel),
    /// used when the *user* asks to stop: a stop can land mid-note, and a
    /// pattern may end on a note-on with no room left for its note-off.
    pub fn stop_playback_and_release(&mut self) {
        if !self.playing {
            return;
        }
        self.all_notes_off();
        self.stop_playback();
    }

    /// How long until the next event is due, or `None` when not playing.
    ///
    /// The event loop sleeps for this long instead of a fixed frame interval,
    /// so an event leaves when it is due rather than at the end of the next
    /// frame. When no event is left, the answer is the time until the end of
    /// the pattern, so the loop still wakes up to stop.
    pub fn next_event_in(&self) -> Option<Duration> {
        if !self.playing {
            return None;
        }
        let tpb = self.project.composition.tpb();
        let micros_per_tick = self.project.composition.bpm().micros_per_tick(tpb);
        if micros_per_tick <= 0.0 {
            return None;
        }
        let horizon = self.playback_horizon();
        let next_tick = self
            .play_events
            .get(self.play_next)
            .map_or(horizon, |event| event.tick)
            .min(horizon);
        let ticks = next_tick.saturating_sub(self.play_tick);
        // `play_micros` already holds the fraction of a tick consumed so far.
        let micros = ticks as f64 * micros_per_tick - self.play_micros;
        Some(Duration::from_micros(micros.max(0.0) as u64))
    }

    /// Advances playback by a wall-clock delta, sending due events.
    pub fn tick_playback(&mut self, delta_micros: u64) {
        if !self.playing {
            return;
        }
        let tpb = self.project.composition.tpb();
        let micros_per_tick = self.project.composition.bpm().micros_per_tick(tpb);
        if micros_per_tick <= 0.0 {
            return;
        }
        self.play_micros += delta_micros as f64;
        let whole_ticks = (self.play_micros / micros_per_tick) as u64;
        if whole_ticks > 0 {
            self.play_micros -= whole_ticks as f64 * micros_per_tick;
            self.play_tick += whole_ticks;
        }

        // Send whatever is due at the current tick, even when no whole tick
        // elapsed: an event sitting exactly on the tick we are already at (the
        // first event of the start row, say) must leave now rather than after
        // another tick. `next_event_in` reports zero for that case, so returning
        // early here would make the event loop spin.
        let mut due: Vec<Event> = Vec::new();
        while self.play_next < self.play_events.len()
            && self.play_events[self.play_next].tick <= self.play_tick
        {
            due.push(self.play_events[self.play_next].event.clone());
            self.play_next += 1;
        }
        for event in due {
            self.send(&event);
        }
        let row_len = row_ticks(tpb, self.pattern().lpb()).unwrap_or(1) as u64;
        self.play_current_row = (self.play_tick / row_len) as usize;
        if self.mode.follows_playback() {
            let (rows, _) = self.pattern_geometry();
            self.row = self.play_current_row.min(rows.saturating_sub(1));
        }
        // Stop at the end of the pattern, not when the last event has been
        // sent: trailing empty rows still belong to the pattern. A longer child
        // pattern would keep playing past this point, so only release when
        // nothing else is still sounding.
        if self.play_tick >= self.play_end_tick && !self.other_patterns_playing() {
            self.all_notes_off();
            self.stop_playback();
        }
    }

    // --- Key dispatch ---------------------------------------------------------

    /// Handles a key press according to the current mode and workspace.
    pub fn handle_key(&mut self, key: Key) {
        // Silence is a panic button: available in every workspace and mode.
        if key.alt && key.code == Code::Char('.') {
            return self.all_notes_off();
        }
        // Esc stops active playback before any context-sensitive cancel.
        if key.code == Code::Esc && !key.ctrl && !key.alt && !key.shift && self.playing {
            return self.stop_playback_and_release();
        }
        match self.mode {
            Mode::Command => return self.handle_command_key(key),
            Mode::Chooser => return self.handle_chooser_key(key),
            _ => {}
        }
        if let Code::Function(1) = key.code {
            return self.switch_screen(Screen::Help);
        }
        if let Code::Function(2) = key.code {
            return self.switch_screen(Screen::Pattern);
        }
        if let Code::Function(3) = key.code {
            return self.switch_screen(Screen::Code);
        }
        // Quitting is global to every workspace.
        if key.ctrl && key.code == Code::Char('q') {
            return self.apply(Action::Quit);
        }
        match self.screen {
            Screen::Code => self.handle_code_key(key),
            Screen::Pattern => self.handle_pattern_key(key),
            Screen::Help => {}
        }
    }

    fn handle_pattern_key(&mut self, key: Key) {
        if matches!(self.mode, Mode::Play | Mode::Rec) {
            if let Code::Char(c) = key.code {
                if !key.ctrl && !key.alt && self.note_key(c) {
                    return;
                }
            }
        }
        // Backspace edits nibbles left of the cursor before falling back to
        // the structural block deletion.
        if self.mode.is_editing() && key == Key::new(Code::Backspace) && self.backspace_nibble() {
            return;
        }
        if let Some(action) = key::lookup(key) {
            self.apply(action);
            return;
        }
        if self.mode.is_editing() {
            if let Code::Char(c) = key.code {
                if !key.ctrl && !key.alt {
                    if let Some(value) = c.to_digit(16) {
                        self.type_nibble(value as u8);
                    }
                }
            }
        }
    }

    fn handle_code_key(&mut self, key: Key) {
        match key.code {
            Code::Char('z') if key.ctrl => self.undo_module(),
            Code::Char('y') if key.ctrl => self.redo_module(),
            Code::Char('s') if key.ctrl => self.apply(Action::Save),
            Code::Enter if key.alt => self.open_chooser(),
            Code::Function(5) => self.reload_lua(),
            Code::Left => self.code.move_left(),
            Code::Right => self.code.move_right(),
            Code::Up => self.code.move_up(),
            Code::Down => self.code.move_down(),
            Code::Home => self.code.move_home(),
            Code::End => self.code.move_end(),
            Code::Enter => self.code_mutate(|code| code.newline()),
            Code::Backspace => self.code_mutate(|code| code.backspace()),
            Code::Delete => self.code_mutate(|code| code.delete()),
            Code::Tab => self.code_mutate(|code| code.tab()),
            Code::Char(c) if !key.ctrl && !key.alt => {
                self.code_mutate(|code| code.insert(c));
            }
            _ => {}
        }
    }

    fn handle_command_key(&mut self, key: Key) {
        if self.path_completion.is_some() && self.handle_completion_key(key) {
            return;
        }
        let completes = key.code == Code::Tab && !key.shift && !key.ctrl && !key.alt;
        if !completes {
            // Any other key closes the completion menu.
            self.path_completion = None;
        }
        match key.code {
            Code::Esc => self.cancel_command(),
            Code::Enter => self.execute_command_line(),
            Code::Tab if completes => self.complete_input(),
            // Alt-Backspace (and Alt-Ctrl-H, which some terminals send for it)
            // deletes the word before the cursor.
            Code::Backspace if key.alt => self.input.kill_word_backward(),
            Code::Char('h') if key.ctrl && key.alt => self.input.kill_word_backward(),
            Code::Backspace => self.input.backspace(),
            Code::Delete => self.input.delete(),
            Code::Left if key.alt => self.input.move_word_left(),
            Code::Left => self.input.move_left(),
            Code::Right if key.alt => self.input.move_word_right(),
            Code::Right => self.input.move_right(),
            Code::Home => self.input.move_home(),
            Code::End => self.input.move_end(),
            // Emacs/readline bindings.
            Code::Char('a') if key.ctrl => self.input.move_home(),
            Code::Char('e') if key.ctrl => self.input.move_end(),
            Code::Char('b') if key.ctrl => self.input.move_left(),
            Code::Char('f') if key.ctrl => self.input.move_right(),
            Code::Char('d') if key.ctrl => {
                if self.input.is_empty() {
                    // End of input, like readline's EOF.
                    self.cancel_command();
                } else {
                    self.input.delete();
                }
            }
            Code::Char('h') if key.ctrl => self.input.backspace(),
            Code::Char('k') if key.ctrl => self.input.kill_to_end(),
            Code::Char('u') if key.ctrl => self.input.kill_to_start(),
            Code::Char('w') if key.ctrl => self.input.kill_whitespace_word_backward(),
            Code::Char('y') if key.ctrl => self.input.yank(),
            Code::Char('t') if key.ctrl => self.input.transpose(),
            Code::Char('g') if key.ctrl => self.cancel_command(),
            Code::Char('c') if key.ctrl => self.cancel_command(),
            Code::Char('b') if key.alt => self.input.move_word_left(),
            Code::Char('f') if key.alt => self.input.move_word_right(),
            Code::Char('d') if key.alt => self.input.kill_word_forward(),
            Code::Char(c) if !key.ctrl && !key.alt => self.input.insert(c),
            _ => {}
        }
    }

    /// Handles a key while the completion menu is open, returning whether the
    /// key was consumed by the menu.
    fn handle_completion_key(&mut self, key: Key) -> bool {
        let plain = !key.ctrl && !key.alt && !key.shift;
        match key.code {
            Code::Up => self.move_completion(-1),
            Code::Down => self.move_completion(1),
            Code::PageUp => self.move_completion(-COMPLETION_PAGE),
            Code::PageDown => self.move_completion(COMPLETION_PAGE),
            Code::Home => self.select_completion(0),
            Code::End => self.select_completion(usize::MAX),
            // Emacs-style movement inside the menu.
            Code::Char('p') if key.ctrl => self.move_completion(-1),
            Code::Char('n') if key.ctrl => self.move_completion(1),
            Code::Tab | Code::Enter if plain => self.accept_completion(),
            Code::Esc => self.path_completion = None,
            // Typing narrows the list instead of closing it.
            Code::Char(c) if !key.ctrl && !key.alt => {
                self.input.insert(c);
                self.refilter_completions();
            }
            Code::Backspace if key.alt => {
                let floor = self.completion_floor();
                if self.input.cursor() <= floor {
                    return false;
                }
                self.input.kill_word_backward_from(floor);
                self.refilter_completions();
            }
            Code::Backspace if !key.ctrl && !key.shift => {
                let floor = self.completion_floor();
                if self.input.cursor() <= floor {
                    return false;
                }
                self.input.backspace();
                self.refilter_completions();
            }
            _ => return false,
        }
        true
    }

    /// The path argument's start, or 0 when no menu is open.
    fn completion_floor(&self) -> usize {
        self.path_completion
            .as_ref()
            .map(|completion| completion.start)
            .unwrap_or(0)
    }

    /// Re-filters the menu against the typed path, closing it on no matches.
    fn refilter_completions(&mut self) {
        let Some(completion) = self.path_completion.as_mut() else {
            return;
        };
        let text: Vec<char> = self.input.text().chars().collect();
        let cursor = self.input.cursor().min(text.len());
        let start = completion.start.min(cursor);
        completion.end = text.len();
        let fragment: String = text[start..cursor].iter().collect();
        if !fragment.starts_with(&completion.dir_prefix) {
            self.path_completion = None;
            return;
        }
        completion.matches = completion
            .all
            .iter()
            .enumerate()
            .filter(|(_, value)| value.starts_with(&fragment))
            .map(|(index, _)| index)
            .collect();
        completion.selected = 0;
        if completion.matches.is_empty() {
            self.path_completion = None;
        }
    }

    /// Moves the completion highlight by `delta`, clamped to the menu.
    fn move_completion(&mut self, delta: isize) {
        let Some(completion) = self.path_completion.as_mut() else {
            return;
        };
        let last = completion.matches.len().saturating_sub(1);
        completion.selected = if delta < 0 {
            completion.selected.saturating_sub(delta.unsigned_abs())
        } else {
            completion.selected.saturating_add(delta as usize).min(last)
        };
    }

    /// Highlights one candidate, clamped to the menu.
    fn select_completion(&mut self, index: usize) {
        if let Some(completion) = self.path_completion.as_mut() {
            completion.selected = index.min(completion.matches.len().saturating_sub(1));
        }
    }

    /// Inserts the highlighted candidate and closes the menu.
    fn accept_completion(&mut self) {
        let Some(completion) = self.path_completion.take() else {
            return;
        };
        let Some(index) = completion.matches.get(completion.selected) else {
            return;
        };
        let Some(value) = completion.all.get(*index).cloned() else {
            return;
        };
        self.replace_input_range(completion.start, completion.end, &value);
    }

    /// Restores the musical mode that was active before a transient mode.
    fn restore_previous_mode(&mut self) {
        let previous = self.previous_mode;
        self.set_mode(previous);
        self.resume_after_command();
    }

    /// Abandons the command line and returns to the previous mode.
    fn cancel_command(&mut self) {
        self.input.clear();
        self.path_completion = None;
        self.command_failed = false;
        self.restore_previous_mode();
    }

    fn handle_chooser_key(&mut self, key: Key) {
        if key.ctrl {
            match key.code {
                Code::Char('c') => return self.chooser_copy(),
                Code::Char('x') => return self.chooser_cut(),
                Code::Char('v') => return self.chooser_paste(),
                Code::Char('z') => return self.chooser_undo(),
                Code::Char('y') => return self.chooser_redo(),
                _ => {}
            }
        }
        match key.code {
            Code::Esc => {
                self.chooser = None;
                self.set_mode(self.previous_mode);
            }
            Code::Enter => self.accept_chooser(),
            Code::Left => self.move_chooser(-1, 0),
            Code::Right => self.move_chooser(1, 0),
            Code::Up => self.move_chooser(0, -1),
            Code::Down => self.move_chooser(0, 1),
            Code::Char(c) => {
                if let Some(chooser) = self.chooser.as_mut() {
                    chooser.type_hex(c);
                }
            }
            _ => {}
        }
    }

    fn move_chooser(&mut self, dx: i32, dy: i32) {
        if let Some(chooser) = self.chooser.as_mut() {
            chooser.move_by(dx, dy);
        }
    }
}

/// Grows the selection horizontally by one byte inside the anchor cell, or by
/// a whole cell once the head has left the anchor cell.
fn grow_horizontal(selection: &mut Selection, dir: isize, cell: usize, total_cols: usize) {
    let anchor_col = selection.anchor.1;
    let anchor_cell_start = anchor_col / cell * cell;
    let anchor_cell_end = anchor_cell_start + cell - 1;
    if dir > 0 {
        if selection.bounds.right < anchor_cell_end {
            selection.bounds.right = (selection.bounds.right + 2).min(anchor_cell_end);
        } else {
            let head_cell = selection.bounds.right / cell;
            let next = (head_cell + 1) * cell + cell - 1;
            if next < total_cols {
                selection.bounds.right = next;
            }
            selection.bounds.left = anchor_cell_start;
        }
    } else if selection.bounds.left > anchor_cell_start {
        selection.bounds.left = selection
            .bounds
            .left
            .saturating_sub(2)
            .max(anchor_cell_start);
    } else {
        let head_cell = selection.bounds.left / cell;
        if head_cell > 0 {
            selection.bounds.left = (head_cell - 1) * cell;
        }
        selection.bounds.right = anchor_cell_end;
    }
}

/// Shrinks a horizontally extended selection toward its anchor.
///
/// Returns whether an extension remains on the extended side.
fn shrink_horizontal(selection: &mut Selection) -> bool {
    let anchor_col = selection.anchor.1;
    let anchor_byte_start = anchor_col & !1;
    let anchor_byte_end = anchor_col | 1;
    match selection.hdir.cmp(&0) {
        std::cmp::Ordering::Greater => {
            if selection.bounds.right > anchor_byte_end {
                selection.bounds.right = (selection.bounds.right - 2).max(anchor_byte_end);
                selection.bounds.right > anchor_byte_end
            } else {
                false
            }
        }
        std::cmp::Ordering::Less => {
            if selection.bounds.left < anchor_byte_start {
                selection.bounds.left = (selection.bounds.left + 2).min(anchor_byte_start);
                selection.bounds.left < anchor_byte_start
            } else {
                false
            }
        }
        std::cmp::Ordering::Equal => false,
    }
}

/// Grows the selection vertically by the captured brush height.
fn grow_vertical(selection: &mut Selection, dir: isize, rows: usize) {
    let step = selection.brush_height.max(1);
    if dir > 0 {
        selection.bounds.bottom = (selection.bounds.bottom + step).min(rows - 1);
    } else {
        selection.bounds.top = selection.bounds.top.saturating_sub(step);
    }
}

/// Shrinks a vertically extended selection toward its anchor.
///
/// Returns whether an extension remains on the extended side.
fn shrink_vertical(selection: &mut Selection) -> bool {
    let anchor_row = selection.anchor.0;
    let step = selection.brush_height.max(1);
    match selection.vdir.cmp(&0) {
        std::cmp::Ordering::Greater => {
            if selection.bounds.bottom > anchor_row {
                selection.bounds.bottom =
                    selection.bounds.bottom.saturating_sub(step).max(anchor_row);
                selection.bounds.bottom > anchor_row
            } else {
                false
            }
        }
        std::cmp::Ordering::Less => {
            if selection.bounds.top < anchor_row {
                selection.bounds.top = (selection.bounds.top + step).min(anchor_row);
                selection.bounds.top < anchor_row
            } else {
                false
            }
        }
        std::cmp::Ordering::Equal => false,
    }
}

fn module_source(project: &mtrak_midi_io::TrackerProject, id: u8) -> Option<String> {
    if let Some(source) = builtin_module(id) {
        return Some(source.to_string());
    }
    project.modules.get(&id).cloned()
}

/// Draws a fresh seed from system entropy for `:seed random`.
///
/// This is the only place randomness enters the seed, and it runs on an explicit
/// user command, so renders stay a pure function of the saved seed.
fn fresh_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    // SplitMix64's finalizer, so seeds taken in the same nanosecond still differ.
    let mut x = nanos ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn render_cell(event: &str, offset: Option<&str>, offsets_visible: bool) -> String {
    match (offsets_visible, offset) {
        (true, Some(offset)) => format!("{event}+{offset}"),
        _ => event.to_string(),
    }
}

// --- Note-entry maps ---------------------------------------------------------

fn scale_map() -> Vec<(char, u8)> {
    let mut map = vec![
        ('z', 0),
        ('x', 1),
        ('c', 2),
        ('v', 3),
        ('b', 4),
        ('n', 5),
        ('m', 6),
        (',', 7),
    ];
    for (i, key_char) in ['q', 'w', 'e', 'r', 't', 'y', 'u', 'i'].iter().enumerate() {
        map.push((*key_char, 12 + i as u8));
    }
    map
}

fn chromatic_map() -> Vec<(char, u8)> {
    let mut map = vec![
        ('z', 0),
        ('s', 1),
        ('x', 2),
        ('d', 3),
        ('c', 4),
        ('v', 5),
        ('g', 6),
        ('b', 7),
        ('h', 8),
        ('n', 9),
        ('j', 10),
        ('m', 11),
        (',', 12),
    ];
    for (i, key_char) in [
        'q', '2', 'w', '3', 'e', 'r', '5', 't', '6', 'y', '7', 'u', 'i',
    ]
    .iter()
    .enumerate()
    {
        map.push((*key_char, 12 + i as u8));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::event::{MacroCall, MetaEvent};
    use mtrak_midi::MockBackend;

    fn app_with(rows: usize, tracks: usize) -> App {
        let mut project = mtrak_midi_io::TrackerProject::new(Composition::new());
        {
            let pattern = project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(rows).unwrap();
            pattern.resize_tracks(tracks).unwrap();
        }
        let mut app = App::new(project);
        // Most tests exercise the editor; action/mode tests switch explicitly.
        app.set_mode(Mode::Edit);
        app
    }

    fn note(key: u8, velocity: u8) -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            key,
            Some(velocity),
        ))
    }

    fn slot_event(app: &App, row: usize, track: usize) -> Event {
        app.pattern().slot(row, track).unwrap().event.clone()
    }

    fn type_hex(app: &mut App, hex: &str) {
        for c in hex.chars() {
            app.handle_key(Key::ch(c));
        }
    }

    #[test]
    fn nibble_cursor_wraps_across_tracks() {
        let mut app = app_with(4, 3);
        // Move right through all six event nibbles.
        for _ in 0..5 {
            app.apply(Action::MoveRight);
        }
        assert_eq!(app.cursor(), (0, 0, 5));
        app.apply(Action::MoveRight);
        assert_eq!(app.cursor(), (0, 1, 0), "wraps to the next track");
        app.apply(Action::MoveLeft);
        assert_eq!(app.cursor(), (0, 0, 5), "wraps back");
        app.apply(Action::MoveLeft);
        assert_eq!(app.cursor(), (0, 0, 4));
    }

    #[test]
    fn offsets_skip_the_plus_separator() {
        let mut app = app_with(4, 2);
        app.apply(Action::ToggleOffsets);
        assert_eq!(app.nibbles_per_cell(), 10);
        for _ in 0..5 {
            app.apply(Action::MoveRight);
        }
        assert_eq!(app.cursor(), (0, 0, 5));
        app.apply(Action::MoveRight);
        assert_eq!(app.cursor(), (0, 0, 6), "first offset nibble");
        let display = app.cell_display(0, 0);
        assert_eq!(display.chars().nth(6), Some('+'));
        // Four more moves reach the next track's first event nibble.
        for _ in 0..4 {
            app.apply(Action::MoveRight);
        }
        assert_eq!(app.cursor(), (0, 1, 0));
    }

    #[test]
    fn typing_commits_immediately_and_undoes_per_cell_session() {
        let mut app = app_with(4, 2);
        type_hex(&mut app, "903C64");
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        app.apply(Action::Undo);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
        app.apply(Action::Redo);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
    }

    #[test]
    fn leaving_a_partial_cell_zero_fills_and_clears_reserved_values() {
        let mut app = app_with(4, 2);
        // Only the first four nibbles of a note-on are entered.
        type_hex(&mut app, "903C");
        app.apply(Action::MoveDown);
        assert_eq!(slot_event(&app, 0, 0), note(60, 0));
        // An all-zero buffer normalizes to reserved macro 00 and is cleared.
        app.apply(Action::TopLeft);
        app.apply(Action::MoveDown);
        app.apply(Action::MoveUp);
        type_hex(&mut app, "000000");
        app.apply(Action::MoveDown);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
    }

    #[test]
    fn invalid_offset_is_reported_before_leaving_the_cell() {
        // One track, so the last offset nibble does not wrap away from the cell.
        let mut app = app_with(4, 1);
        app.apply(Action::ToggleOffsets);
        type_hex(&mut app, "903C640018");
        assert!(app.offset_invalid());
        app.nibble = 8;
        type_hex(&mut app, "00");
        assert!(!app.offset_invalid());
    }

    #[test]
    fn offsets_validate_and_clamp_on_leave() {
        let mut app = app_with(4, 2);
        app.apply(Action::ToggleOffsets);
        // row_ticks = tpb 24 / lpb 4 = 6, so valid offsets are 0..5.
        type_hex(&mut app, "903C640018");
        assert_eq!(app.cursor(), (0, 1, 0), "wrapped after the offset nibbles");
        assert_eq!(app.pattern().slot(0, 0).unwrap().offset, 5);
    }

    #[test]
    fn offset_display_is_four_fixed_hex_digits() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .slot_mut(0, 0)
            .unwrap()
            .offset = 5;
        app.apply(Action::ToggleOffsets);
        assert!(app.cell_display(0, 0).ends_with("+0005"));
        // A cell with no event still shows a zero offset column.
        assert!(app.cell_display(1, 0).ends_with("+0000"));
    }

    #[test]
    fn offset_visibility_resets_when_a_new_pattern_is_selected() {
        let mut app = app_with(4, 2);
        app.apply(Action::ToggleOffsets);
        assert!(app.offsets_visible());
        app.execute(Command::PatternNew(Some(4))).unwrap();
        assert!(
            !app.offsets_visible(),
            "visibility resets for a newly selected pattern"
        );
    }

    #[test]
    fn clear_block_clears_whole_slots_and_undoes() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 1, note(62, 100));
        // Full-width brush covers both tracks.
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::ClearBlock);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
        assert_eq!(slot_event(&app, 0, 1), Event::Empty);
        app.apply(Action::Undo);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!(slot_event(&app, 0, 1), note(62, 100));
    }

    #[test]
    fn esc_cancels_a_selection_without_mutating() {
        let mut app = app_with(4, 2);
        app.apply(Action::SelectDown);
        assert!(app.selection().is_some());
        app.apply(Action::CancelSelection);
        assert!(app.selection().is_none());
    }

    #[test]
    fn copy_paste_preserves_nibble_alignment() {
        let mut app = app_with(4, 2);
        type_hex(&mut app, "903C64");
        app.apply(Action::TopLeft);
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider); // six nibbles
        app.apply(Action::Copy);
        app.row = 2;
        app.track = 0;
        app.nibble = 0;
        app.apply(Action::Paste);
        assert_eq!(slot_event(&app, 2, 0), note(60, 100));
        // The destination offset is untouched when offsets are hidden.
        assert_eq!(app.pattern().slot(2, 0).unwrap().offset, 0);
    }

    #[test]
    fn cut_clears_the_whole_touched_slots() {
        let mut app = app_with(4, 2);
        type_hex(&mut app, "903C64");
        app.apply(Action::TopLeft);
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::Cut);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
        assert!(!app.clipboard.is_empty());
    }

    #[test]
    fn structural_shifts_do_not_split_cells() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 1, note(62, 100));
        // A two-nibble brush only touches track 0.
        app.apply(Action::BrushWider);
        app.apply(Action::InsertBlock);
        assert_eq!(slot_event(&app, 1, 0), note(60, 100));
        assert_eq!(slot_event(&app, 0, 1), note(62, 100), "track 1 unmoved");
    }

    #[test]
    fn enter_on_a_nonempty_cell_prepopulates_the_event_command() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Command);
        assert_eq!(app.input_text(), "event 903C64");
        assert_eq!(app.input_cursor(), "event ".len());
    }

    #[test]
    fn event_command_accepts_spaced_and_compact_bytes() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "event 90 64 3F");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Edit);
        assert_eq!(slot_event(&app, 0, 0), note(0x64, 0x3F));
    }

    #[test]
    fn event_command_detects_macros() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "event 01 05 00");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(
            slot_event(&app, 0, 0),
            Event::Macro(MacroCall::new(1, 5, 0))
        );
    }

    #[test]
    fn invalid_event_command_keeps_command_mode_in_error_state() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "event 90 64");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Command, "mode remains open");
        assert!(app.command_failed());
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
    }

    #[test]
    fn complex_events_round_trip_through_the_command_editor() {
        let mut app = app_with(4, 2);
        let tempo = Event::Meta(MetaEvent::Tempo(500_000));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, tempo.clone());
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.input_text(), "event FF510307A120");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(slot_event(&app, 0, 0), tempo);
    }

    #[test]
    fn screens_switch_with_function_keys() {
        let mut app = app_with(4, 2);
        assert_eq!(app.screen(), Screen::Pattern);
        app.handle_key(Key::new(Code::Function(3)));
        assert_eq!(app.screen(), Screen::Code);
        app.handle_key(Key::new(Code::Function(1)));
        assert_eq!(app.screen(), Screen::Help);
        app.handle_key(Key::new(Code::Function(2)));
        assert_eq!(app.screen(), Screen::Pattern);
    }

    #[test]
    fn playback_runs_to_the_end_of_the_pattern() {
        // One note in the first row: playback used to stop as soon as the last
        // event had been sent, i.e. right after the first row.
        let mut app = app_with(8, 1);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.start_playback();
        assert!(app.is_playing());
        // 24 TPB / 4 LPB = 6 ticks per row, 8 rows = 48 ticks at 120 BPM.
        app.tick_playback(500_000);
        assert!(app.is_playing(), "plays past the last event");
        app.tick_playback(500_000);
        assert!(!app.is_playing(), "stops at the pattern end");

        // A pattern without events also plays its full length.
        let mut app = app_with(8, 1);
        app.start_playback();
        assert!(app.is_playing());
        app.tick_playback(500_000);
        assert!(app.is_playing());
    }

    #[test]
    fn tab_completes_paths_in_read_and_write_commands() {
        let dir = std::env::temp_dir().join(format!("mtrak-app-complete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("unique.mid"), b"").unwrap();
        std::fs::write(dir.join("with space.mid"), b"").unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, &format!("r {dir}/uni"));
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.input_text(), format!("r {dir}/unique.mid"));
        assert_eq!(app.input_cursor(), app.input_text().chars().count());
        assert_eq!(app.mode(), Mode::Command, "Tab keeps command mode open");

        // File names with spaces are completed as one argument.
        app.handle_key(Key::new(Code::Esc));
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, &format!("w {dir}/with"));
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.input_text(), format!("w {dir}/with space.mid"));

        // Other commands and a Tab on the command name do nothing.
        app.handle_key(Key::new(Code::Esc));
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "bpm 12");
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.input_text(), "bpm 12");
    }

    #[test]
    fn command_line_emacs_shortcuts() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "r /tmp/song.mid");

        // Alt-Backspace deletes the word before the cursor...
        app.handle_key(Key::alt(Code::Backspace));
        assert_eq!(app.input_text(), "r /tmp/song.");
        // ...and Ctrl-Y puts it back.
        app.handle_key(Key::ctrl(Code::Char('y')));
        assert_eq!(app.input_text(), "r /tmp/song.mid");

        // Ctrl-A/Ctrl-K/Ctrl-Y: home, kill the line, yank it back.
        app.handle_key(Key::ctrl(Code::Char('a')));
        assert_eq!(app.input_cursor(), 0);
        app.handle_key(Key::ctrl(Code::Char('k')));
        assert_eq!(app.input_text(), "");
        app.handle_key(Key::ctrl(Code::Char('y')));
        assert_eq!(app.input_text(), "r /tmp/song.mid");

        // Ctrl-E goes to the end, Ctrl-W kills the whole argument.
        app.handle_key(Key::ctrl(Code::Char('e')));
        assert_eq!(app.input_cursor(), app.input_text().chars().count());
        app.handle_key(Key::ctrl(Code::Char('w')));
        assert_eq!(app.input_text(), "r ");
        assert_eq!(app.input_cursor(), 2);

        // Ctrl-C abandons the command.
        app.handle_key(Key::ctrl(Code::Char('c')));
        assert_eq!(app.mode(), Mode::Edit);
        assert!(app.input_text().is_empty());
    }

    #[test]
    fn ctrl_d_on_an_empty_command_line_closes_it() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        assert_eq!(app.mode(), Mode::Command);
        app.handle_key(Key::ctrl(Code::Char('d')));
        assert_eq!(app.mode(), Mode::Edit);

        // With text, Ctrl-D deletes the character at the cursor instead.
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "abc");
        app.handle_key(Key::ctrl(Code::Char('a')));
        app.handle_key(Key::ctrl(Code::Char('d')));
        assert_eq!(app.input_text(), "bc");
        assert_eq!(app.mode(), Mode::Command);
    }

    #[test]
    fn completion_menu_navigates_and_accepts_with_tab_or_enter() {
        let dir =
            std::env::temp_dir().join(format!("mtrak-app-complete-list-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["aa.mid", "bb.mid", "cc.mid"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let dir = dir.to_string_lossy().into_owned();
        let expected = vec![
            format!("{dir}/aa.mid"),
            format!("{dir}/bb.mid"),
            format!("{dir}/cc.mid"),
            format!("{dir}/sub/"),
        ];

        let mut app = app_with(4, 1);
        let open_menu = |app: &mut App| {
            app.handle_key(Key::new(Code::Esc));
            app.handle_key(Key::ch(':'));
            type_hex(app, &format!("r {dir}/"));
            app.handle_key(Key::new(Code::Tab));
            assert_eq!(app.input_text(), format!("r {dir}/"));
            let (values, matches, selected) = app.path_completions().unwrap();
            assert_eq!(values.to_vec(), expected);
            assert_eq!(matches.len(), 4);
            assert_eq!(selected, 0);
            assert_eq!(app.mode(), Mode::Command);
        };
        let selected = |app: &App| app.path_completions().unwrap().2;

        // Opening the menu leaves the first candidate highlighted.
        open_menu(&mut app);
        assert_eq!(selected(&app), 0);

        // Arrows, Home/End, and PgUp/PgDn move the highlight (clamped).
        app.handle_key(Key::new(Code::Down));
        assert_eq!(selected(&app), 1);
        app.handle_key(Key::new(Code::Down));
        assert_eq!(selected(&app), 2);
        app.handle_key(Key::new(Code::End));
        assert_eq!(selected(&app), 3);
        app.handle_key(Key::new(Code::Down));
        assert_eq!(selected(&app), 3, "clamped at the end");
        app.handle_key(Key::new(Code::PageDown));
        assert_eq!(selected(&app), 3);
        app.handle_key(Key::new(Code::Home));
        assert_eq!(selected(&app), 0);
        app.handle_key(Key::new(Code::PageUp));
        assert_eq!(selected(&app), 0, "clamped at the start");
        app.handle_key(Key::ctrl(Code::Char('n')));
        assert_eq!(selected(&app), 1);
        app.handle_key(Key::ctrl(Code::Char('p')));
        assert_eq!(selected(&app), 0);

        // Tab accepts the highlighted candidate without running the command.
        app.handle_key(Key::new(Code::Down));
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.input_text(), format!("r {dir}/bb.mid"));
        assert!(app.path_completions().is_none());
        assert_eq!(app.mode(), Mode::Command);

        // Enter does the same.
        open_menu(&mut app);
        app.handle_key(Key::new(Code::Down));
        app.handle_key(Key::new(Code::Down));
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.input_text(), format!("r {dir}/cc.mid"));
        assert!(app.path_completions().is_none());
        assert_eq!(app.mode(), Mode::Command);

        // Esc only closes the menu; it does not abandon the command line.
        open_menu(&mut app);
        app.handle_key(Key::new(Code::Esc));
        assert!(app.path_completions().is_none());
        assert_eq!(app.mode(), Mode::Command);
        assert_eq!(app.input_text(), format!("r {dir}/"));
    }

    #[test]
    fn typing_narrows_the_completion_menu() {
        let dir =
            std::env::temp_dir().join(format!("mtrak-app-complete-narrow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["aa.mid", "bb.mid", "cc.mid"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, &format!("r {dir}/"));
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.path_completions().unwrap().1.len(), 4);

        // Typing narrows the match list and keeps the menu open...
        app.handle_key(Key::ch('c'));
        assert_eq!(app.input_text(), format!("r {dir}/c"));
        let (values, matches, selected) = app.path_completions().unwrap();
        assert_eq!(values[matches[selected]], format!("{dir}/cc.mid"));
        assert_eq!(matches.len(), 1);
        // ...and Tab accepts the only match.
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.input_text(), format!("r {dir}/cc.mid"));
        assert!(app.path_completions().is_none());

        // Backspace widens the list again.
        app.handle_key(Key::new(Code::Esc));
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, &format!("r {dir}/"));
        app.handle_key(Key::new(Code::Tab));
        app.handle_key(Key::ch('c'));
        assert_eq!(app.path_completions().unwrap().1.len(), 1);
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.input_text(), format!("r {dir}/"));
        assert_eq!(app.path_completions().unwrap().1.len(), 4);

        // Alt-Backspace removes the whole typed word.
        app.handle_key(Key::ch('c'));
        app.handle_key(Key::ch('c'));
        assert_eq!(app.path_completions().unwrap().1.len(), 1);
        app.handle_key(Key::alt(Code::Backspace));
        assert_eq!(app.input_text(), format!("r {dir}/"));
        assert_eq!(app.path_completions().unwrap().1.len(), 4);

        // A prefix with no matches closes the menu but keeps the text.
        for ch in "zzz".chars() {
            app.handle_key(Key::ch(ch));
        }
        assert!(app.path_completions().is_none());
        assert_eq!(app.input_text(), format!("r {dir}/zzz"));
    }

    #[test]
    fn record_step_is_session_local_and_set_by_alt_digits_and_the_command() {
        let mut app = app_with(8, 2);
        assert_eq!(app.recording_step(), 1);
        app.handle_key(Key::alt(Code::Char('3')));
        assert_eq!(app.recording_step(), 3);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "step 17");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.recording_step(), 17);
        assert_eq!(app.mode(), Mode::Edit);
    }

    #[test]
    fn transpose_shifts_keyboard_input_and_is_session_local() {
        let mut app = app_with(4, 1);
        assert_eq!(app.transpose(), 0);
        assert!(!app.is_dirty());

        app.handle_key(Key::new(Code::Function(8)));
        assert_eq!(app.transpose(), 12);
        assert!(!app.is_dirty(), "transpose is a session setting");
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(72, 100));

        let mut app = app_with(4, 1);
        app.handle_key(Key::new(Code::Function(7)));
        app.handle_key(Key::new(Code::Function(7)));
        assert_eq!(app.transpose(), -24);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(36, 100));
    }

    #[test]
    fn transpose_command_sets_reports_and_clamps() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "transpose -12");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.transpose(), -12);
        assert!(!app.is_dirty());
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(48, 100));

        // No argument reports the current value.
        let mut app = app_with(4, 1);
        app.set_transpose(5);
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "transpose");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.message().map(|m| m.text.as_str()), Some("transpose +5"));

        // Out-of-range values are rejected and leave the setting unchanged.
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "transpose 999");
        app.handle_key(Key::new(Code::Enter));
        assert!(app.command_failed());
        assert_eq!(app.transpose(), 5);
        app.handle_key(Key::new(Code::Esc));

        // F7/F8 saturate instead of overflowing.
        app.set_transpose(MAX_TRANSPOSE);
        app.handle_key(Key::new(Code::Function(8)));
        assert_eq!(app.transpose(), MAX_TRANSPOSE);
        app.set_transpose(MIN_TRANSPOSE);
        app.handle_key(Key::new(Code::Function(7)));
        assert_eq!(app.transpose(), MIN_TRANSPOSE);

        // Notes clamp to the MIDI range.
        let mut app = app_with(4, 1);
        app.set_transpose(MAX_TRANSPOSE);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(127, 100));

        let mut app = app_with(4, 1);
        app.set_transpose(MIN_TRANSPOSE);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(0, 100));
    }

    #[test]
    fn seed_command_sets_reports_and_rerolls() {
        let mut app = app_with(4, 2);
        assert_eq!(app.project.composition.seed(), 0, "fresh projects seed 0");

        app.execute(Command::Seed(SeedArg::Set(0x1234))).unwrap();
        assert_eq!(app.project.composition.seed(), 0x1234);
        assert!(app.is_dirty(), "changing the seed is a project edit");

        // Showing the seed reports it without mutating the project.
        app.dirty = false;
        app.execute(Command::Seed(SeedArg::Show)).unwrap();
        let text = app.message().map(|m| m.text.clone()).unwrap_or_default();
        assert!(text.contains("0000000000001234"), "{text}");
        assert!(!app.is_dirty(), "showing the seed is not an edit");

        // A fresh seed changes the value and reports it.
        app.execute(Command::Seed(SeedArg::Random)).unwrap();
        assert_ne!(app.project.composition.seed(), 0x1234);
        assert!(app.is_dirty());
    }

    #[test]
    fn ctrl_q_quits_from_every_screen() {
        for function in [1u8, 2, 3] {
            let mut app = app_with(4, 2);
            app.handle_key(Key::new(Code::Function(function)));
            app.handle_key(Key::ctrl(Code::Char('q')));
            assert!(app.should_quit(), "ctrl-q on screen {function}");
        }
    }

    #[test]
    fn quitting_during_playback_releases_every_channel() {
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        app.start_playback();
        app.handle_key(Key::ctrl(Code::Char('q')));
        assert!(app.should_quit());
        assert!(!app.is_playing());

        let messages = sent.borrow().clone();
        assert_eq!(messages.len(), 16);
        for (channel, message) in messages.iter().enumerate() {
            assert_eq!(message, &[0xB0 | channel as u8, 123, 0]);
        }
    }

    #[test]
    fn ctrl_s_saves_without_typing_on_the_code_screen() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        let line = app.code_editor().line(0).to_vec();
        app.handle_key(Key::ctrl(Code::Char('s')));
        assert_eq!(app.code_editor().line(0), &line[..], "no 's' is typed");
        assert!(!app.should_quit());
    }

    #[test]
    fn alt_enter_opens_the_chooser() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::alt(Code::Enter));
        assert_eq!(app.mode(), Mode::Chooser);
        app.handle_key(Key::new(Code::Esc));

        // On the Code screen Alt-Enter must not fall through to a newline.
        app.handle_key(Key::new(Code::Function(3)));
        let lines = app.code_editor().line_count();
        app.handle_key(Key::alt(Code::Enter));
        assert_eq!(app.mode(), Mode::Chooser);
        assert_eq!(app.code_editor().line_count(), lines);
    }

    #[test]
    fn chooser_creates_a_pattern_at_the_jumped_id() {
        let mut app = app_with(4, 2);
        app.apply(Action::OpenChooser);
        assert_eq!(app.mode(), Mode::Chooser);
        for c in "4f".chars() {
            app.handle_key(Key::ch(c));
        }
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Edit);
        assert_eq!(app.current_pattern(), PatternId(0x4F));
        assert!(app.pattern_exists(0x4F));
    }

    #[test]
    fn chooser_esc_cancels_without_changing_the_current_item() {
        let mut app = app_with(4, 2);
        app.apply(Action::OpenChooser);
        for c in "10".chars() {
            app.handle_key(Key::ch(c));
        }
        app.handle_key(Key::new(Code::Esc));
        assert_eq!(app.mode(), Mode::Edit);
        assert_eq!(app.current_pattern(), PatternId(0));
        assert!(!app.pattern_exists(0x10));
    }

    #[test]
    fn chooser_protects_the_main_pattern() {
        let mut app = app_with(4, 2);
        app.apply(Action::OpenChooser);
        app.handle_key(Key::ctrl(Code::Char('x')));
        assert!(app.pattern_exists(0), "main pattern cannot be cut");
    }

    #[test]
    fn chooser_pattern_copy_paste_preserves_destination_id() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.execute(Command::PatternNew(Some(4))).unwrap();
        app.apply(Action::OpenChooser);
        app.chooser.as_mut().unwrap().cursor = 0;
        app.handle_key(Key::ctrl(Code::Char('c')));
        app.chooser.as_mut().unwrap().cursor = 0x20;
        app.handle_key(Key::ctrl(Code::Char('v')));
        app.handle_key(Key::new(Code::Esc));
        assert!(app.pattern_exists(0x20));
        assert_eq!(
            app.project()
                .composition
                .pattern(PatternId(0x20))
                .unwrap()
                .slot(0, 0)
                .unwrap()
                .event,
            note(60, 100)
        );
    }

    #[test]
    fn code_screen_creates_lazy_user_modules_and_edits_them() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        app.apply(Action::OpenChooser);
        for c in "40".chars() {
            app.handle_key(Key::ch(c));
        }
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.module_id(), 0x40);
        assert!(app.module_source(0x40).unwrap().contains("function M.run"));
        app.handle_key(Key::ch('X'));
        assert!(app.project().modules.get(&0x40).unwrap().contains('X'));
        app.handle_key(Key::ctrl(Code::Char('z')));
        assert!(!app.project().modules.get(&0x40).unwrap().contains('X'));
    }

    #[test]
    fn builtin_modules_are_read_only() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        app.select_module(1);
        assert!(app.module_readonly(1));
        let before = app.project().modules.get(&1).cloned();
        app.handle_key(Key::ch('X'));
        assert_eq!(app.project().modules.get(&1).cloned(), before);
        assert!(app.message().unwrap().is_error);
    }

    #[test]
    fn per_pattern_undo_histories_are_isolated() {
        let mut app = app_with(4, 2);
        type_hex(&mut app, "903C64"); // pattern 0
        app.execute(Command::PatternNew(Some(4))).unwrap();
        let second = app.current_pattern();
        type_hex(&mut app, "904064"); // pattern 1
        app.execute(Command::PatternSelect(PatternId(0))).unwrap();
        app.apply(Action::Undo);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
        assert_eq!(
            app.project()
                .composition
                .pattern(second)
                .unwrap()
                .slot(0, 0)
                .unwrap()
                .event,
            note(64, 100),
            "the other pattern's history is untouched"
        );
    }

    #[test]
    fn backspace_deletes_the_previous_nibble_and_steps_back() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ch('9'));
        assert_eq!(app.cursor(), (0, 0, 1));
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.cursor(), (0, 0, 0), "the cursor steps back");
        assert_eq!(app.cell_display(0, 0), "······", "the digit is gone");
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);

        // The erased nibble can be retyped immediately.
        app.handle_key(Key::ch('9'));
        assert_eq!(app.cursor(), (0, 0, 1));
        assert_eq!(app.cell_display(0, 0), "9·····");
    }

    #[test]
    fn backspace_only_removes_the_nibble_before_the_cursor() {
        let mut app = app_with(4, 1);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        for _ in 0..3 {
            app.handle_key(Key::new(Code::Right));
        }
        assert_eq!(app.cursor(), (0, 0, 3));
        assert_eq!(app.cell_display(0, 0), "903C64");

        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.cursor(), (0, 0, 2));
        assert_eq!(app.cell_display(0, 0), "90·C64");
        assert_eq!(slot_event(&app, 0, 0), note(60, 100), "slot untouched");

        // Retyping restores the complete event.
        app.handle_key(Key::ch('3'));
        assert_eq!(app.cursor(), (0, 0, 3));
        assert_eq!(app.cell_display(0, 0), "903C64");
    }

    #[test]
    fn backspace_clears_offset_nibbles_too() {
        let mut app = app_with(4, 1);
        app.apply(Action::ToggleOffsets);
        assert_eq!(app.cell_display(0, 0), "······+0000");
        for _ in 0..7 {
            app.apply(Action::MoveRight);
        }
        app.handle_key(Key::ch('8'));
        assert_eq!(app.cursor(), (0, 0, 8));
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.cursor(), (0, 0, 7));
        assert_eq!(
            app.cell_display(0, 0),
            "······+0·00",
            "the typed 8 is blanked again"
        );
    }

    #[test]
    fn backspace_block_moves_the_cursor_with_the_shifted_cells() {
        let mut app = app_with(8, 1);
        for row in 2..6 {
            app.project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .set_event(row, 0, note(60 + row as u8, 100));
        }
        for _ in 0..3 {
            app.apply(Action::MoveDown);
        }
        assert_eq!(app.cursor().0, 3);

        app.apply(Action::BackspaceBlock);
        assert_eq!(app.cursor().0, 2, "the cursor rides the shifted cells");
        assert_eq!(slot_event(&app, 2, 0), note(63, 100));
        assert_eq!(slot_event(&app, 3, 0), note(64, 100));

        // A taller brush shifts the same number of rows (LPB = 4 here).
        let mut app = app_with(8, 1);
        for row in 2..8 {
            app.project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .set_event(row, 0, note(60 + row as u8, 100));
        }
        for _ in 0..6 {
            app.apply(Action::MoveDown);
        }
        app.apply(Action::BrushTaller);
        app.apply(Action::BackspaceBlock);
        assert_eq!(app.cursor().0, 2);
        assert_eq!(slot_event(&app, 2, 0), note(66, 100));
        assert_eq!(slot_event(&app, 3, 0), note(67, 100));
    }

    #[test]
    fn backspace_at_the_first_nibble_keeps_block_behavior() {
        // An untouched cell keeps the structural row-shift Backspace...
        let mut app = app_with(4, 1);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(1, 0, note(60, 100));
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));

        // ...but a cell edited in this session swallows it instead of
        // shifting rows under the user.
        let mut app = app_with(4, 1);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(1, 0, note(60, 100));
        app.handle_key(Key::ch('9'));
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.cursor(), (0, 0, 0));
        app.handle_key(Key::new(Code::Backspace));
        assert!(slot_event(&app, 0, 0).is_empty(), "no row shift");
        assert_eq!(slot_event(&app, 1, 0), note(60, 100));
    }

    #[test]
    fn note_entry_uses_global_settings() {
        let mut app = app_with(8, 1);
        app.handle_key(Key::ctrl(Code::Char('r')));
        assert_eq!(app.mode(), Mode::Rec);
        app.handle_key(Key::ch('z'));
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!(app.cursor().0, 1, "REC advances by the recording step");
    }

    #[test]
    fn rec_step_advances_by_the_configured_step_and_clamps() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::alt(Code::Char('2')));
        app.handle_key(Key::ch('z'));
        assert_eq!(app.cursor().0, 2);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        app.handle_key(Key::ch('x'));
        assert_eq!(app.cursor().0, 3, "clamped at the final row");
        assert_eq!(slot_event(&app, 2, 0), note(62, 100));

        // Step 0 leaves the row unchanged and replaces the same cell.
        app.handle_key(Key::alt(Code::Char('0')));
        app.handle_key(Key::ch('c'));
        assert_eq!(app.cursor().0, 3);
        assert_eq!(slot_event(&app, 3, 0), note(64, 100));
        app.handle_key(Key::ch('d'));
        assert_eq!(app.cursor().0, 3);
        assert_eq!(slot_event(&app, 3, 0), note(63, 100));
    }

    #[test]
    fn each_recorded_note_is_one_independent_undo_step() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        app.handle_key(Key::ch('x'));
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!(slot_event(&app, 1, 0), note(62, 100));
        app.handle_key(Key::ctrl(Code::Char('z')));
        assert_eq!(slot_event(&app, 1, 0), Event::Empty);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        app.handle_key(Key::ctrl(Code::Char('z')));
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
        app.handle_key(Key::ctrl(Code::Char('y')));
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
    }

    #[test]
    fn play_auditions_without_mutating_or_advancing() {
        let mut app = app_with(8, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        assert_eq!(app.mode(), Mode::Edit);
        app.handle_key(Key::ctrl(Code::Char('p')));
        assert_eq!(app.mode(), Mode::Play);
        app.handle_key(Key::ch('z'));
        assert_eq!(sent.borrow().as_slice(), &[vec![0x90, 60, 100]]);
        assert_eq!(app.cursor(), (0, 0, 0));
        assert!(slot_event(&app, 0, 0).is_empty());
        assert!(!app.is_dirty(), "PLAY never mutates pattern data");
    }

    #[test]
    fn play_and_rec_use_the_selected_scale() {
        let mut app = app_with(4, 1);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('s'));
        assert_eq!(slot_event(&app, 0, 0), note(61, 100), "chromatic s = C#");

        let mut app = app_with(4, 1);
        app.project.composition.global_settings_mut().scale =
            mtrak_core::settings::lookup_builtin("major").unwrap();
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('x'));
        assert_eq!(slot_event(&app, 0, 0), note(62, 100), "major degree 1 = D");
    }

    #[test]
    fn channel_and_velocity_inherit_independently_from_earlier_rows() {
        let mut app = app_with(8, 1);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(
                0,
                0,
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::ControlChange,
                    channel: Some(5),
                    data1: Some(7),
                    data2: Some(99),
                }),
            );
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(
                1,
                0,
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::NoteOn,
                    channel: None,
                    data1: Some(60),
                    data2: Some(42),
                }),
            );
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.apply(Action::MoveDown);
        app.apply(Action::MoveDown);
        app.handle_key(Key::ch('z'));
        let expected = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            5,
            60,
            Some(42),
        ));
        assert_eq!(slot_event(&app, 2, 0), expected);
    }

    #[test]
    fn play_and_rec_navigate_cell_wide_and_keep_the_nibble() {
        let mut app = app_with(8, 3);
        app.apply(Action::MoveRight);
        app.apply(Action::MoveRight);
        assert_eq!(app.cursor(), (0, 0, 2));
        app.handle_key(Key::ctrl(Code::Char('r')));
        assert_eq!(app.mode(), Mode::Rec);
        assert_eq!(app.cursor(), (0, 0, 2));

        app.handle_key(Key::new(Code::Right));
        assert_eq!(app.cursor(), (0, 1, 2), "Right selects the next track");
        app.handle_key(Key::new(Code::Tab));
        assert_eq!(app.cursor(), (0, 2, 2));
        app.handle_key(Key::shift(Code::Tab));
        assert_eq!(app.cursor(), (0, 1, 2));
        app.handle_key(Key::new(Code::Left));
        assert_eq!(app.cursor(), (0, 0, 2));

        app.handle_key(Key::ctrl(Code::Char('e')));
        assert_eq!(app.mode(), Mode::Edit);
        assert_eq!(app.cursor(), (0, 0, 2), "the nibble was preserved");
        app.handle_key(Key::new(Code::Right));
        assert_eq!(app.cursor(), (0, 0, 3), "EDIT moves by one nibble");
    }

    #[test]
    fn play_and_rec_disable_pattern_editing_except_undo_redo() {
        let mut app = app_with(8, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.handle_key(Key::ctrl(Code::Char('r')));
        let before = app.pattern().clone();
        let cursor = app.cursor();
        let brush = (app.brush_width(), app.brush_height());
        for action in [
            Action::TrackInsert,
            Action::TrackDelete,
            Action::BrushWider,
            Action::BrushTaller,
            Action::SelectRight,
            Action::SelectDown,
            Action::InsertBlock,
            Action::DeleteBlock,
            Action::BackspaceBlock,
            Action::ClearBlock,
            Action::Cut,
            Action::Copy,
            Action::Paste,
            Action::ToggleOffsets,
            Action::OpenEventEditor,
        ] {
            app.apply(action);
        }
        app.handle_key(Key::ch('9'));
        app.handle_key(Key::new(Code::Backspace));
        assert_eq!(app.mode(), Mode::Rec);
        assert_eq!(app.pattern(), &before, "no editing action may mutate");
        assert_eq!(app.cursor(), cursor);
        assert_eq!((app.brush_width(), app.brush_height()), brush);
        assert!(app.selection().is_none());
        assert!(!app.offsets_visible());
    }

    #[test]
    fn undo_and_redo_work_in_play_and_rec() {
        for mode in [Mode::Play, Mode::Rec] {
            let mut app = app_with(4, 1);
            type_hex(&mut app, "903C64");
            assert_eq!(slot_event(&app, 0, 0), note(60, 100));
            app.handle_key(Key::ctrl(Code::Char('r')));
            app.set_mode(mode);
            app.handle_key(Key::ctrl(Code::Char('z')));
            assert_eq!(slot_event(&app, 0, 0), Event::Empty, "{mode:?}");
            app.handle_key(Key::ctrl(Code::Char('y')));
            assert_eq!(slot_event(&app, 0, 0), note(60, 100), "{mode:?}");
        }
    }

    #[test]
    fn mode_switches_finalize_one_partial_edit_and_preserve_coordinates() {
        let mut app = app_with(8, 3);
        app.apply(Action::MoveDown);
        app.handle_key(Key::ch('9'));
        assert_eq!(app.cursor(), (1, 0, 1));
        app.handle_key(Key::ctrl(Code::Char('p')));
        assert_eq!(app.mode(), Mode::Play);
        assert_eq!(app.cursor(), (1, 0, 1));
        assert_ne!(
            slot_event(&app, 1, 0),
            Event::Empty,
            "the partial edit was finalized, not discarded"
        );
        app.handle_key(Key::ctrl(Code::Char('e')));
        assert_eq!(app.cursor(), (1, 0, 1));

        // Switching without a partial edit must not mutate anything.
        let mut app = app_with(4, 1);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ctrl(Code::Char('e')));
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
    }

    #[test]
    fn live_recording_writes_the_playhead_offset_and_rebuilds_the_stream() {
        let mut app = app_with(8, 1);
        // An old event still pending later in the playhead row.
        {
            let slot = app
                .project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .slot_mut(0, 0)
                .unwrap();
            slot.event = note(67, 100);
            slot.offset = 3;
        }
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        app.start_playback();
        app.tick_playback(25_000);
        assert_eq!(app.play_tick, 1, "one tick into row 0");
        app.handle_key(Key::ctrl(Code::Char('r')));
        assert_eq!(app.mode(), Mode::Rec);
        app.handle_key(Key::ch('z'));

        assert_eq!(app.play_tick, 1, "live recording preserves the tick");
        assert_eq!(
            app.project
                .composition
                .pattern(PatternId(0))
                .unwrap()
                .slot(0, 0)
                .unwrap()
                .offset,
            1,
            "the intra-row offset is captured"
        );
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!(
            sent.borrow().as_slice(),
            &[vec![0x90, 60, 100]],
            "the audition is sent exactly once"
        );

        // Play past the replaced event's old offset; it must not sound.
        app.tick_playback(100_000);
        assert_eq!(
            sent.borrow().as_slice(),
            &[vec![0x90, 60, 100]],
            "the replaced pending event is gone"
        );
    }

    #[test]
    fn command_mode_pauses_and_resumes_playback() {
        let mut app = app_with(8, 1);
        app.start_playback();
        app.tick_playback(25_000);
        let tick = app.play_tick;
        let micros = app.play_micros;

        app.handle_key(Key::ch(':'));
        assert!(!app.is_playing(), "command mode pauses the transport");
        assert_eq!(app.mode(), Mode::Command);
        assert_eq!(app.play_tick, tick);
        assert_eq!(app.play_micros, micros);

        type_hex(&mut app, "bpm 140");
        app.handle_key(Key::new(Code::Enter));
        assert!(app.is_playing(), "successful command resumes playback");
        assert_eq!(app.mode(), Mode::Edit);
        assert_eq!(app.play_tick, tick, "resume keeps the exact tick");
        assert_eq!(app.project().composition.bpm(), Bpm::from_int(140).unwrap());
    }

    #[test]
    fn command_resume_rebuilds_against_project_changes() {
        let mut app = app_with(8, 1);
        app.start_playback();
        app.tick_playback(25_000);
        let old_end = app.play_end_tick;
        app.handle_key(Key::ch(':'));
        assert!(!app.is_playing());
        type_hex(&mut app, "rows 2");
        app.handle_key(Key::new(Code::Enter));
        assert!(app.is_playing());
        let row_len = mtrak_core::timing::row_ticks(24, 4).unwrap() as u64;
        assert_eq!(app.play_end_tick, 2 * row_len, "rows change is reflected");
        assert_ne!(app.play_end_tick, old_end);
        app.tick_playback(1_000_000);
        assert!(!app.is_playing(), "the rebuilt end stops playback");
    }

    #[test]
    fn command_cancellation_and_failure_preserve_the_pause() {
        // Cancellation resumes from the paused tick.
        let mut app = app_with(8, 1);
        app.start_playback();
        app.tick_playback(25_000);
        let tick = app.play_tick;
        app.handle_key(Key::ch(':'));
        assert!(!app.is_playing());
        app.handle_key(Key::new(Code::Esc));
        assert!(app.is_playing());
        assert_eq!(app.play_tick, tick);

        // A failed command keeps command mode open and playback paused.
        let mut app = app_with(8, 1);
        app.start_playback();
        app.handle_key(Key::ch(':'));
        type_hex(&mut app, "not-a-command");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Command);
        assert!(app.command_failed());
        assert!(!app.is_playing());
        app.handle_key(Key::new(Code::Esc));
        assert!(app.is_playing());

        // Command mode entered while stopped never starts playback.
        let mut app = app_with(8, 1);
        app.handle_key(Key::ch(':'));
        assert!(!app.is_playing());
        app.handle_key(Key::new(Code::Esc));
        assert!(!app.is_playing());
    }

    #[test]
    fn esc_stops_playback_and_s_sets_the_start_row_in_every_mode() {
        let mut app = app_with(8, 1);
        app.start_playback();
        assert!(app.is_playing());
        app.handle_key(Key::new(Code::Esc));
        assert!(!app.is_playing());

        for mode in [Mode::Play, Mode::Rec, Mode::Edit] {
            let mut app = app_with(8, 1);
            app.set_mode(mode);
            app.apply(Action::MoveDown);
            app.apply(Action::MoveDown);
            app.handle_key(Key::ch('S'));
            assert_eq!(app.start_row(), 2, "{mode:?}");
        }

        // With playback stopped, Esc keeps its selection-cancel behavior.
        let mut app = app_with(8, 1);
        app.apply(Action::SelectRight);
        assert!(app.selection().is_some());
        app.handle_key(Key::new(Code::Esc));
        assert!(app.selection().is_none());
    }

    #[test]
    fn play_and_rec_lock_the_cursor_row_to_the_playhead_but_edit_does_not() {
        let mut app = app_with(8, 1);
        app.set_mode(Mode::Play);
        app.start_playback();
        assert_eq!(app.cursor().0, 0);
        app.tick_playback(125_000);
        assert_eq!(app.play_current_row, 1);
        assert_eq!(app.cursor().0, 1, "PLAY follows the playhead");

        app.handle_key(Key::ctrl(Code::Char('r')));
        assert_eq!(app.cursor().0, 1);
        app.tick_playback(125_000);
        assert_eq!(app.play_current_row, 2);
        assert_eq!(app.cursor().0, 2, "REC follows the playhead");

        app.handle_key(Key::ctrl(Code::Char('e')));
        app.tick_playback(125_000);
        assert_eq!(app.play_current_row, 3);
        assert_eq!(app.cursor().0, 2, "EDIT leaves the cursor alone");
        app.handle_key(Key::new(Code::Esc));
        assert!(!app.is_playing(), "Esc stops the transport");
    }

    #[test]
    fn stopping_playback_leaves_the_cursor_at_the_stopping_row() {
        let mut app = app_with(8, 1);
        app.set_mode(Mode::Play);
        app.start_playback();
        app.tick_playback(2 * 125_000 + 1);
        assert_eq!(app.play_current_row, 2);
        app.handle_key(Key::new(Code::Esc));
        assert!(!app.is_playing());
        assert_eq!(app.cursor().0, 2);
    }

    #[test]
    fn mode_switches_do_not_start_or_stop_playback_and_space_still_toggles() {
        let mut app = app_with(8, 1);
        app.set_mode(Mode::Play);
        app.start_playback();
        for mode in [Mode::Rec, Mode::Edit, Mode::Play] {
            app.set_mode(mode);
            assert!(app.is_playing(), "{mode:?} does not stop playback");
        }

        for mode in [Mode::Play, Mode::Rec, Mode::Edit] {
            let mut app = app_with(8, 1);
            app.set_mode(mode);
            app.handle_key(Key::ch(' '));
            assert!(app.is_playing(), "space starts in {mode:?}");
            app.handle_key(Key::ch(' '));
            assert!(!app.is_playing(), "space stops in {mode:?}");
        }
    }

    #[test]
    fn step_recording_always_writes_offset_zero() {
        let mut app = app_with(4, 1);
        {
            let slot = app
                .project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .slot_mut(0, 0)
                .unwrap();
            slot.event = note(50, 10);
            slot.offset = 5;
        }
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        let slot = app
            .project
            .composition
            .pattern(PatternId(0))
            .unwrap()
            .slot(0, 0)
            .unwrap();
        assert_eq!(slot.offset, 0);
        assert_eq!(slot.event, note(60, 10));
    }

    #[test]
    fn command_pause_sends_all_notes_off() {
        let mut app = app_with(8, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);
        app.start_playback();
        app.handle_key(Key::ch(':'));
        let messages = sent.borrow().clone();
        assert_eq!(messages.len(), 16);
        for (channel, message) in messages.iter().enumerate() {
            assert_eq!(message, &[0xB0 | channel as u8, 123, 0]);
        }
    }

    #[test]
    fn note_entry_inherits_channel_and_velocity_from_the_cell() {
        let cases = [
            // Existing note: channel and velocity are kept.
            (
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    5,
                    72,
                    Some(42),
                )),
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    5,
                    60,
                    Some(42),
                )),
            ),
            // Non-note channel event: channel is kept, velocity is defaulted.
            (
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::ControlChange,
                    channel: Some(3),
                    data1: Some(7),
                    data2: Some(99),
                }),
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    3,
                    60,
                    Some(100),
                )),
            ),
            // Only a velocity: channel is defaulted.
            (
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::NoteOn,
                    channel: None,
                    data1: Some(60),
                    data2: Some(88),
                }),
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    0,
                    60,
                    Some(88),
                )),
            ),
            // Note-off velocities count too.
            (
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::NoteOff,
                    channel: Some(9),
                    data1: Some(60),
                    data2: Some(64),
                }),
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    9,
                    60,
                    Some(64),
                )),
            ),
            // Only a channel: velocity is defaulted.
            (
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::NoteOn,
                    channel: Some(9),
                    data1: Some(60),
                    data2: None,
                }),
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    9,
                    60,
                    Some(100),
                )),
            ),
        ];
        for (existing, expected) in cases {
            let mut app = app_with(8, 1);
            app.project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .set_event(0, 0, existing.clone());
            app.handle_key(Key::ctrl(Code::Char('r')));
            app.handle_key(Key::ch('z'));
            assert_eq!(
                slot_event(&app, 0, 0),
                expected,
                "existing event {existing:?}"
            );
        }
    }

    #[test]
    fn note_entry_auditions_with_the_inherited_channel_and_velocity() {
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(
                0,
                0,
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    5,
                    72,
                    Some(42),
                )),
            );
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(sent.borrow().as_slice(), &[vec![0x95, 60, 42]]);
    }

    #[test]
    fn audition_sends_through_the_selected_port() {
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);
        app.handle_key(Key::ctrl(Code::Char('r')));
        app.handle_key(Key::ch('z'));
        assert_eq!(sent.borrow().as_slice(), &[vec![0x90, 60, 100]]);
    }

    #[test]
    fn tilde_is_expanded_for_file_commands() {
        let app = app_with(4, 1);
        let Some(home) = crate::path::home_dir() else {
            return;
        };
        assert_eq!(
            app.resolve_path(Some("~/songs/live.mid".to_string()))
                .unwrap(),
            home.join("songs/live.mid")
        );
        assert_eq!(
            app.resolve_path(Some("/tmp/live.mid".to_string())).unwrap(),
            std::path::PathBuf::from("/tmp/live.mid")
        );
    }

    #[test]
    fn alt_period_sends_all_notes_off_on_every_channel() {
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        app.handle_key(Key::alt(Code::Char('.')));
        let messages = sent.borrow().clone();
        assert_eq!(messages.len(), 16);
        for (channel, message) in messages.iter().enumerate() {
            assert_eq!(message, &[0xB0 | channel as u8, 123, 0]);
        }

        // It is a panic button: it also works away from the pattern screen.
        sent.borrow_mut().clear();
        app.handle_key(Key::new(Code::Function(3)));
        app.handle_key(Key::alt(Code::Char('.')));
        assert_eq!(sent.borrow().len(), 16);
    }

    #[test]
    fn user_stop_releases_every_channel() {
        // A user-initiated stop can land mid-note, so it must not leave notes
        // hanging.
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        app.start_playback();
        assert!(app.is_playing());
        app.stop_playback_and_release();
        assert!(!app.is_playing());

        let messages = sent.borrow().clone();
        assert_eq!(messages.len(), 16);
        for (channel, message) in messages.iter().enumerate() {
            assert_eq!(message, &[0xB0 | channel as u8, 123, 0]);
        }

        // Stopping an already stopped player sends nothing.
        sent.borrow_mut().clear();
        app.stop_playback_and_release();
        assert!(sent.borrow().is_empty());
    }

    #[test]
    fn playback_end_releases_when_nothing_else_plays() {
        // Reaching the end of the only playing pattern is a stop like any
        // other, so a trailing note-on without a note-off must not ring.
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        app.start_playback();
        assert!(!app.other_patterns_playing());
        // 24 TPB / 4 LPB = 6 ticks per row, 4 rows = 24 ticks at 120 BPM.
        app.tick_playback(1_000_000);
        assert!(!app.is_playing());

        let messages = sent.borrow().clone();
        assert_eq!(messages.len(), 16);
        for (channel, message) in messages.iter().enumerate() {
            assert_eq!(message, &[0xB0 | channel as u8, 123, 0]);
        }
    }

    #[test]
    fn a_macro_child_keeps_playing_past_the_parents_last_row() {
        // A macro can trigger the playback of any pattern, and the child keeps
        // playing after the parent that started it ends. Playback must cover the
        // child instead of stopping at the parent's last row, and it must not
        // silence everything while the child is still sounding.
        let mut app = app_with(2, 1);
        let child = app.project.composition.create_pattern(4).unwrap();
        {
            let pattern = app.project.composition.pattern_mut(child).unwrap();
            pattern.resize_rows(4).unwrap();
            pattern.set_event(0, 0, note(60, 100));
            pattern.set_event(3, 0, note(64, 100));
        }
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(
                0,
                0,
                Event::Macro(MacroCall::new(
                    mtrak_core::event::macro_id::BUILTIN_TRIGGER,
                    child.value(),
                    0,
                )),
            );

        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);

        // 24 TPB / 4 LPB = 6 ticks per row: the parent is 2 rows (12 ticks), the
        // child 4 rows (24 ticks). At 120 BPM a tick is ~20833 µs.
        app.start_playback();
        app.tick_playback(250_000);
        assert!(app.is_playing(), "the child is still sounding");
        assert!(app.other_patterns_playing());
        assert!(
            sent.borrow().iter().all(|message| message[1] != 123),
            "the child must not be silenced when the parent ends"
        );

        app.tick_playback(250_000);
        assert!(!app.is_playing(), "stops once the child is done");
        assert!(!app.other_patterns_playing());
        assert!(
            sent.borrow().iter().any(|message| message[1] == 123),
            "everything is released when the last pattern ends"
        );
    }

    #[test]
    fn enter_on_an_empty_cell_does_nothing() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Edit);
    }

    #[test]
    fn smf_escape_round_trips_through_the_command_editor() {
        let mut app = app_with(4, 2);
        let escape = Event::SmfEscape(vec![0x01, 0x02, 0x03]);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, escape.clone());
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.input_text(), "event F703010203");
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(slot_event(&app, 0, 0), escape);
    }

    #[test]
    fn copy_paste_includes_offsets_when_visible() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .slot_mut(0, 0)
            .unwrap()
            .offset = 3;
        app.apply(Action::ToggleOffsets);
        app.apply(Action::TopLeft);
        for _ in 0..3 {
            app.apply(Action::BrushWider); // Full width includes offsets
        }
        app.apply(Action::Copy);
        app.row = 2;
        app.track = 0;
        app.nibble = 0;
        app.apply(Action::Paste);
        assert_eq!(slot_event(&app, 2, 0), note(60, 100));
        assert_eq!(app.pattern().slot(2, 0).unwrap().offset, 3);
    }

    #[test]
    fn chooser_module_copy_paste_is_type_specific() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        app.select_module(0x40);
        app.handle_key(Key::ch('X'));
        app.apply(Action::OpenChooser);
        app.chooser.as_mut().unwrap().cursor = 0x40;
        app.handle_key(Key::ctrl(Code::Char('c')));
        app.chooser.as_mut().unwrap().cursor = 0x41;
        app.handle_key(Key::ctrl(Code::Char('v')));
        app.handle_key(Key::new(Code::Esc));
        assert!(app.module_source(0x41).unwrap().contains('X'));
        // A pattern clipboard cannot be pasted into a module cell.
        app.apply(Action::OpenChooser);
        app.chooser.as_mut().unwrap().cursor = 0x42;
        app.chooser_clipboard = Some(ChooserClipboard::Pattern(
            app.project()
                .composition
                .pattern(PatternId(0))
                .unwrap()
                .clone(),
        ));
        app.handle_key(Key::ctrl(Code::Char('v')));
        assert!(app.module_source(0x42).is_none());
        app.handle_key(Key::new(Code::Esc));
    }

    #[test]
    fn per_module_undo_histories_are_isolated() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        app.select_module(0x40);
        app.handle_key(Key::ch('A'));
        app.select_module(0x41);
        app.handle_key(Key::ch('B'));
        app.select_module(0x40);
        app.handle_key(Key::ctrl(Code::Char('z')));
        assert!(!app.module_source(0x40).unwrap().contains('A'));
        assert!(app.module_source(0x41).unwrap().contains('B'));
    }

    #[test]
    fn pattern_content_history_is_discarded_on_replacement() {
        let mut app = app_with(4, 2);
        type_hex(&mut app, "903C64");
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.apply(Action::OpenChooser);
        app.chooser.as_mut().unwrap().cursor = 0;
        app.handle_key(Key::ctrl(Code::Char('c')));
        app.chooser.as_mut().unwrap().cursor = 0x10;
        app.handle_key(Key::ctrl(Code::Char('v')));
        app.handle_key(Key::new(Code::Esc));
        assert!(app.pattern_exists(0x10));
    }

    // --- Selection and brush fixes -----------------------------------------

    fn bounds(app: &App) -> Rect {
        app.selection_bounds().expect("a selection is active")
    }

    fn set_offset(app: &mut App, row: usize, track: usize, offset: u32) {
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .slot_mut(row, track)
            .unwrap()
            .offset = offset;
    }

    #[test]
    fn brush_width_presets_track_offset_visibility() {
        let mut app = app_with(4, 2);
        assert_eq!(app.brush_width(), BrushWidth::One);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Two);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Six);
        app.apply(Action::BrushWider);
        assert_eq!(
            app.brush_width(),
            BrushWidth::Full,
            "ten is skipped when offsets are hidden"
        );

        let mut app = app_with(4, 2);
        app.apply(Action::ToggleOffsets);
        app.brush_width = BrushWidth::Six;
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Ten);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Full);
        app.apply(Action::BrushNarrower);
        assert_eq!(app.brush_width(), BrushWidth::Ten);
        app.apply(Action::ToggleOffsets);
        assert_eq!(
            app.brush_width(),
            BrushWidth::Six,
            "hiding offsets converts ten to six"
        );
    }

    #[test]
    fn brush_shifts_origin_at_edges_without_moving_the_cursor() {
        let mut app = app_with(4, 2);
        app.track = 1;
        app.nibble = 0;
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Six);
        assert_eq!(
            app.brush_rect(),
            Rect {
                top: 0,
                bottom: 0,
                left: 6,
                right: 11
            }
        );
        assert_eq!(app.cursor(), (0, 1, 0), "the logical cursor is unchanged");

        app.apply(Action::BrushTaller);
        app.apply(Action::BrushTaller);
        app.row = 3;
        let brush = app.brush_rect();
        assert_eq!((brush.top, brush.bottom), (0, 3));
        assert_eq!(app.cursor().0, 3, "the cursor stays at the requested row");
    }

    #[test]
    fn documented_horizontal_selection_examples() {
        // First nibble of AA (column 0).
        let mut app = app_with(4, 2);
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 1
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 3
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 5
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 11
            }
        );

        // First nibble of BB (column 2).
        let mut app = app_with(4, 2);
        app.nibble = 2;
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 3
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 5
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 11
            }
        );

        // Second nibble of BB (column 3).
        let mut app = app_with(4, 2);
        app.nibble = 3;
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 5
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 11
            }
        );
    }

    #[test]
    fn negative_selection_grows_left_and_reverses_through_the_anchor() {
        let mut app = app_with(4, 2);
        app.nibble = 3;
        app.apply(Action::SelectLeft);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 3
            }
        );
        app.apply(Action::SelectLeft);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 3
            }
        );
        // Reversing shrinks toward the anchor before growing the other way.
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 3
            }
        );
        app.apply(Action::SelectRight);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 5
            }
        );
    }

    #[test]
    fn vertical_selection_growth_uses_the_brush_height() {
        let mut app = app_with(8, 2);
        app.apply(Action::SelectDown);
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 1
            }
        );
        app.apply(Action::SelectDown);
        assert_eq!(bounds(&app).bottom, 1);

        let mut app = app_with(8, 2);
        app.apply(Action::BrushTaller); // LPB = 4
        app.apply(Action::SelectDown);
        assert_eq!((bounds(&app).top, bounds(&app).bottom), (0, 3));
        app.apply(Action::SelectDown);
        assert_eq!(bounds(&app).bottom, 7);
    }

    #[test]
    fn unmodified_navigation_clears_the_selection() {
        let actions = [
            Action::MoveLeft,
            Action::MoveRight,
            Action::MoveUp,
            Action::MoveDown,
            Action::PageUp,
            Action::PageDown,
            Action::Home,
            Action::End,
            Action::TopLeft,
            Action::BottomRight,
            Action::NextTrack,
            Action::PrevTrack,
        ];
        for action in actions {
            let mut app = app_with(8, 3);
            app.row = 3;
            app.track = 1;
            app.apply(Action::SelectDown);
            assert!(app.selection().is_some(), "selection starts for {action:?}");
            app.apply(action);
            assert!(app.selection().is_none(), "{action:?} clears the selection");
        }
    }

    #[test]
    fn brush_resize_clears_the_selection() {
        for action in [
            Action::BrushWider,
            Action::BrushNarrower,
            Action::BrushTaller,
            Action::BrushShorter,
        ] {
            let mut app = app_with(8, 3);
            app.apply(Action::SelectDown);
            app.apply(action);
            assert!(app.selection().is_none(), "{action:?} clears the selection");
        }
    }

    #[test]
    fn copy_rounds_the_brush_outward_to_bytes() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        // A one-nibble brush on the first nibble of the data byte acts on the
        // whole `3C` byte.
        app.nibble = 2;
        assert_eq!(
            app.operation_rect(),
            Rect {
                top: 0,
                bottom: 0,
                left: 2,
                right: 3
            }
        );
        app.apply(Action::Copy);
        assert_eq!(app.clipboard.width(), 2);
        // A one-nibble brush on the last nibble of a cell still rounds to the
        // containing byte rather than the next cell.
        app.nibble = 5;
        assert_eq!(
            app.operation_rect(),
            Rect {
                top: 0,
                bottom: 0,
                left: 4,
                right: 5
            }
        );
    }

    #[test]
    fn partial_clear_zeroes_only_the_selected_data_byte() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.nibble = 2;
        app.apply(Action::ClearBlock);
        assert_eq!(slot_event(&app, 0, 0), note(0, 100));
        // Undo restores the original event.
        app.apply(Action::Undo);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
    }

    #[test]
    fn clearing_the_status_byte_empties_the_cell() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.nibble = 0;
        app.apply(Action::ClearBlock);
        assert_eq!(slot_event(&app, 0, 0), Event::Empty);
    }

    #[test]
    fn byte_precise_operations_reject_non_inline_events() {
        let mut app = app_with(4, 2);
        let sysex = Event::SysEx(vec![0x01, 0x02]);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, sysex.clone());
        app.apply(Action::ClearBlock);
        assert!(app.message().unwrap().is_error);
        assert_eq!(slot_event(&app, 0, 0), sysex);
        app.apply(Action::Cut);
        assert!(app.message().unwrap().is_error);
        assert_eq!(slot_event(&app, 0, 0), sysex);
    }

    #[test]
    fn blank_clipboard_positions_paste_as_zeroes() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(2, 0, note(60, 100));
        // Copy an empty cell with a six-nibble brush.
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::Copy);
        app.row = 2;
        app.apply(Action::Paste);
        assert_eq!(
            slot_event(&app, 2, 0),
            Event::Empty,
            "in-rectangle blanks paste as explicit zeroes"
        );
    }

    #[test]
    fn clipboard_maps_offsets_across_visibility() {
        // Copy with offsets hidden, paste with offsets visible: event bytes map
        // to event columns and do not spill into the offset column.
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::Copy);
        app.apply(Action::ToggleOffsets);
        app.row = 2;
        app.nibble = 0;
        app.apply(Action::Paste);
        assert_eq!(slot_event(&app, 2, 0), note(60, 100));
        assert_eq!(app.pattern().slot(2, 0).unwrap().offset, 0);

        // Copy with offsets visible, paste with offsets hidden: offset nibbles
        // are clipped rather than spilling into the next event.
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        set_offset(&mut app, 0, 0, 3);
        app.apply(Action::ToggleOffsets);
        app.brush_width = BrushWidth::Ten;
        app.apply(Action::Copy);
        app.apply(Action::ToggleOffsets);
        app.row = 2;
        app.nibble = 0;
        app.apply(Action::Paste);
        assert_eq!(slot_event(&app, 2, 0), note(60, 100));
        assert_eq!(app.pattern().slot(2, 0).unwrap().offset, 0);
    }

    #[test]
    fn paste_with_selection_uses_top_left_and_clips() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::Copy);
        app.brush_width = BrushWidth::One;
        // A one-byte selection at row 2 clips the six-nibble clipboard to the
        // status byte only.
        app.row = 2;
        app.apply(Action::SelectRight);
        app.apply(Action::Paste);
        assert_eq!(slot_event(&app, 2, 0), note(0, 0));
        assert!(app.selection().is_none(), "paste clears the selection");
        assert_eq!(slot_event(&app, 0, 0), note(60, 100), "source unchanged");
    }

    #[test]
    fn copy_preserves_and_cut_clears_the_selection() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.apply(Action::SelectRight);
        app.apply(Action::Copy);
        assert!(app.selection().is_some(), "copy preserves the selection");
        app.apply(Action::Cut);
        assert!(app.selection().is_none(), "cut clears the selection");
    }

    #[test]
    fn row_shifts_expand_the_selection_to_whole_cells() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 1, note(62, 100));
        // A one-byte selection at the first track still shifts the whole cell.
        app.apply(Action::SelectRight);
        app.apply(Action::InsertBlock);
        assert_eq!(slot_event(&app, 1, 0), note(60, 100));
        assert_eq!(slot_event(&app, 0, 1), note(62, 100), "track 1 unmoved");
        let expanded = bounds(&app);
        assert_eq!(
            (expanded.left, expanded.right),
            (0, 5),
            "the visible selection expands to the whole cell"
        );
        assert!(
            app.selection().is_some(),
            "the expanded selection stays active"
        );
    }

    #[test]
    fn initial_selection_matches_the_brush_size() {
        let mut app = app_with(4, 2);
        app.apply(Action::SelectRight);
        assert_eq!(bounds(&app).right, 1, "one nibble rounds to one byte");

        let mut app = app_with(4, 2);
        app.apply(Action::BrushWider);
        app.apply(Action::SelectRight);
        assert_eq!(bounds(&app).right, 1, "two nibbles is one byte");

        let mut app = app_with(4, 2);
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::SelectRight);
        assert_eq!(bounds(&app).right, 5, "six nibbles is one cell");

        let mut app = app_with(4, 2);
        app.apply(Action::ToggleOffsets);
        app.brush_width = BrushWidth::Ten;
        app.apply(Action::SelectRight);
        assert_eq!(bounds(&app).right, 9, "ten nibbles includes the offset");

        let mut app = app_with(8, 2);
        app.apply(Action::SelectDown);
        assert_eq!((bounds(&app).top, bounds(&app).bottom), (0, 0));
        app.apply(Action::BrushTaller);
        app.apply(Action::SelectDown);
        assert_eq!((bounds(&app).top, bounds(&app).bottom), (0, 3), "LPB rows");
    }

    #[test]
    fn cursor_brush_and_selection_are_distinct() {
        let mut app = app_with(4, 2);
        app.track = 1;
        app.nibble = 2;
        app.apply(Action::BrushWider);
        app.apply(Action::BrushWider);
        app.apply(Action::SelectRight);
        // The brush shifts its origin back to stay inside the pattern, the
        // selection starts at the anchor, and the cursor follows the head.
        let selection = app.selection().expect("selection");
        assert_eq!(selection.anchor, (0, 8));
        assert_eq!(app.cursor(), (0, 1, 5), "the cursor follows the head");
        assert_eq!(
            app.brush_rect(),
            Rect {
                top: 0,
                bottom: 0,
                left: 6,
                right: 11
            }
        );
        assert_eq!(
            bounds(&app),
            Rect {
                top: 0,
                bottom: 0,
                left: 8,
                right: 11
            }
        );
    }

    #[test]
    fn delete_and_backspace_expand_the_selection_to_cells() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(1, 0, note(60, 100));
        app.apply(Action::SelectRight);
        app.apply(Action::BackspaceBlock);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!((bounds(&app).left, bounds(&app).right), (0, 5));

        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(1, 0, note(60, 100));
        app.apply(Action::SelectRight);
        app.apply(Action::DeleteBlock);
        assert_eq!(slot_event(&app, 0, 0), note(60, 100));
        assert_eq!((bounds(&app).left, bounds(&app).right), (0, 5));
    }

    #[test]
    fn partial_clear_serializes_canonically() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.nibble = 2;
        app.apply(Action::ClearBlock);
        assert_eq!(slot_event(&app, 0, 0), note(0, 100));
        let bytes = mtrak_midi_io::smf::save_project(&app.project, None).unwrap();
        let loaded = mtrak_midi_io::smf::load(&bytes).unwrap();
        assert_eq!(
            loaded
                .project
                .composition
                .pattern(PatternId(0))
                .unwrap()
                .slot(0, 0)
                .unwrap()
                .event,
            note(0, 100),
            "partial operations remain canonical through save/load"
        );
    }

    #[test]
    fn partial_cut_zeroes_data_and_clears_the_selection() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.nibble = 2;
        app.apply(Action::Cut);
        assert_eq!(slot_event(&app, 0, 0), note(0, 100));
        assert!(!app.clipboard.is_empty());
        assert!(app.selection().is_none());

        // Clear leaves the selection cleared as well.
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.apply(Action::SelectRight);
        app.apply(Action::ClearBlock);
        assert!(app.selection().is_none());
    }

    #[test]
    fn no_selection_row_shifts_expand_the_brush_to_cells() {
        for action in [Action::DeleteBlock, Action::BackspaceBlock] {
            let mut app = app_with(4, 2);
            app.project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .set_event(1, 0, note(60, 100));
            app.project
                .composition
                .pattern_mut(PatternId(0))
                .unwrap()
                .set_event(1, 1, note(62, 100));
            // A two-nibble brush only covers part of track 0's cell.
            app.apply(Action::BrushWider);
            app.apply(action);
            assert_eq!(slot_event(&app, 0, 0), note(60, 100), "{action:?}");
            assert_eq!(slot_event(&app, 1, 1), note(62, 100), "track 1 unmoved");
        }
    }

    #[test]
    fn full_height_brush_selection_covers_the_pattern() {
        let mut app = app_with(8, 2);
        app.apply(Action::BrushTaller);
        app.apply(Action::BrushTaller);
        app.apply(Action::SelectDown);
        assert_eq!((bounds(&app).top, bounds(&app).bottom), (0, 7));
    }

    #[test]
    fn partial_operations_export_canonically() {
        let mut app = app_with(4, 2);
        app.project
            .composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, note(60, 100));
        app.nibble = 2;
        app.apply(Action::ClearBlock);
        let bytes = mtrak_midi_io::smf::export_standard(&app.project.composition, None).unwrap();
        let loaded = mtrak_midi_io::smf::load(&bytes).unwrap();
        assert_eq!(
            loaded
                .project
                .composition
                .pattern(PatternId(0))
                .unwrap()
                .slot(0, 0)
                .unwrap()
                .event,
            note(0, 100),
            "partial operations remain canonical through export/import"
        );
    }
}
