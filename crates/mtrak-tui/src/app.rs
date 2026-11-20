//! Application state: the tracker grid, editing operations, note entry, the
//! command layer, and playback. This is terminal-independent so it can be fully
//! unit-tested; [`crate::ui`] only draws it and feeds it keys.

use std::path::PathBuf;

use mtrak_core::composition::{Composition, PatternId, Slot};
use mtrak_core::event::{ChannelKind, ChannelMessage, Event};
use mtrak_core::settings::{EffectiveSettings, Setting, Settings};
use mtrak_core::timing::{row_ticks, Bpm};
use mtrak_engine::macros::MacroFactory;
use mtrak_engine::scheduler::TimedEvent;
use mtrak_midi::{MidiBackend, MidiError, OutputManager, PortInfo};

use crate::clipboard::Clipboard;
use crate::command::{self, Command, ScaleArgs};
use crate::editor::EventEditor;
use crate::key::{self, Action, Code, Key};
use crate::undo::UndoStack;

/// The mode the editor is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Normal editing.
    Normal,
    /// Note entry.
    Note,
    /// The colon command prompt.
    Command,
    /// The full event editor.
    EventEditor,
}

/// A single-line status message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// The message text.
    pub text: String,
    /// Whether it reports an error.
    pub is_error: bool,
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

/// How the brush width cycles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrushWidth {
    /// One cell.
    One,
    /// Two cells.
    Two,
    /// Six cells (one three-byte event).
    Six,
    /// The full pattern width.
    Full,
}

impl BrushWidth {
    /// The next wider setting.
    pub fn grow(self) -> Self {
        match self {
            BrushWidth::One => BrushWidth::Two,
            BrushWidth::Two => BrushWidth::Six,
            BrushWidth::Six => BrushWidth::Full,
            BrushWidth::Full => BrushWidth::Full,
        }
    }

    /// The next narrower setting.
    pub fn shrink(self) -> Self {
        match self {
            BrushWidth::One => BrushWidth::One,
            BrushWidth::Two => BrushWidth::One,
            BrushWidth::Six => BrushWidth::Two,
            BrushWidth::Full => BrushWidth::Six,
        }
    }

    /// The number of cells across, given the pattern width.
    pub fn cells(self, width: usize) -> usize {
        match self {
            BrushWidth::One => 1,
            BrushWidth::Two => 2,
            BrushWidth::Six => 6,
            BrushWidth::Full => width,
        }
        .min(width.max(1))
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

/// An inclusive rectangular region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    /// First row.
    pub top: usize,
    /// Last row (inclusive).
    pub bottom: usize,
    /// First track.
    pub left: usize,
    /// Last track (inclusive).
    pub right: usize,
}

impl Rect {
    /// The number of rows.
    pub fn height(&self) -> usize {
        self.bottom.saturating_sub(self.top) + 1
    }

    /// The number of tracks.
    pub fn width(&self) -> usize {
        self.right.saturating_sub(self.left) + 1
    }
}

/// A rectangular selection anchored at a point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The fixed anchor.
    pub anchor: (usize, usize),
    /// The moving head.
    pub head: (usize, usize),
}

impl Selection {
    /// The selection as a rectangle.
    pub fn rect(&self) -> Rect {
        Rect {
            top: self.anchor.0.min(self.head.0),
            bottom: self.anchor.0.max(self.head.0),
            left: self.anchor.1.min(self.head.1),
            right: self.anchor.1.max(self.head.1),
        }
    }
}

/// Provides a fresh macro factory for rendering (e.g. a Lua runtime).
pub trait MacroSource {
    /// Builds a macro factory, or `None` when there are no macros.
    fn factory(&self) -> Option<Box<dyn MacroFactory>>;
}

/// The whole application state.
pub struct App {
    project: mtrak_midi_io::TrackerProject,
    current: PatternId,
    /// Cursor row.
    pub row: usize,
    /// Cursor track.
    pub track: usize,
    brush_width: BrushWidth,
    brush_height: BrushHeight,
    selection: Option<Selection>,
    mode: Mode,
    previous_mode: Mode,
    input: String,
    message: Option<Message>,
    note_chromatic: bool,
    clipboard: Clipboard,
    undo: UndoStack<Composition>,
    event_editor: Option<EventEditor>,
    midi: Option<OutputManager<Box<dyn MidiBackend>>>,
    macro_source: Option<Box<dyn MacroSource>>,
    lua_commands: Vec<LuaCommand>,
    path: Option<PathBuf>,
    quit: bool,
    dirty: bool,
    project_revision: u64,
    // Playback state.
    playing: bool,
    play_events: Vec<TimedEvent>,
    play_next: usize,
    play_tick: u64,
    play_micros: f64,
    play_start_row: usize,
    play_current_row: usize,
}

impl App {
    /// Creates an app around a project.
    pub fn new(project: mtrak_midi_io::TrackerProject) -> Self {
        let current = project.composition.selected_main();
        let scale_aware = project
            .composition
            .effective_settings(current)
            .map(|s| s.scale_aware)
            .unwrap_or(true);
        App {
            project,
            current,
            row: 0,
            track: 0,
            brush_width: BrushWidth::One,
            brush_height: BrushHeight::One,
            selection: None,
            mode: Mode::Normal,
            previous_mode: Mode::Normal,
            input: String::new(),
            message: None,
            note_chromatic: !scale_aware,
            clipboard: Clipboard::default(),
            undo: UndoStack::new(64),
            event_editor: None,
            midi: None,
            macro_source: None,
            lua_commands: Vec::new(),
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

    /// The current mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The current command line / editor buffer.
    pub fn input(&self) -> &str {
        &self.input
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
    pub fn pattern(&self) -> &mtrak_core::composition::Pattern {
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
    }

    /// Sets the current file path.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// The current file path, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// The effective settings of the current pattern.
    pub fn effective_settings(&self) -> EffectiveSettings {
        self.project
            .composition
            .effective_settings(self.current)
            .unwrap_or_else(|| Settings::local().resolve(&Settings::global_default()))
    }

    /// The brush/selection rectangle the editing operations act on.
    pub fn affected_rect(&self) -> Rect {
        if let Some(selection) = self.selection {
            return selection.rect();
        }
        let rows = self.pattern().row_count();
        let tracks = self.pattern().track_count();
        let height = self.brush_height.cells(self.pattern().lpb() as usize, rows);
        let width = self.brush_width.cells(tracks);
        Rect {
            top: self.row.min(rows.saturating_sub(1)),
            bottom: (self.row + height - 1).min(rows.saturating_sub(1)),
            left: self.track.min(tracks.saturating_sub(1)),
            right: (self.track + width - 1).min(tracks.saturating_sub(1)),
        }
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

    /// Whether note entry is chromatic.
    pub fn note_chromatic(&self) -> bool {
        self.note_chromatic
    }

    /// Renders inline text for a cell (six characters or a complex marker).
    pub fn cell_text(&self, row: usize, track: usize) -> String {
        match self.pattern().slot(row, track) {
            Some(slot) if !slot.is_empty() => match slot.event.inline_hex() {
                Some(hex) => format!("{hex:<6}").chars().take(6).collect(),
                None => "◈·····".to_string(),
            },
            _ => "······".to_string(),
        }
    }

    // --- Navigation -----------------------------------------------------------

    fn pattern_geometry(&self) -> (usize, usize) {
        (self.pattern().row_count(), self.pattern().track_count())
    }

    fn clamp_cursor(&mut self) {
        let (rows, tracks) = self.pattern_geometry();
        self.row = self.row.min(rows.saturating_sub(1));
        self.track = self.track.min(tracks.saturating_sub(1));
    }

    /// Applies a navigation or editing action.
    pub fn apply(&mut self, action: Action) {
        let page = self.pattern().lpb() as usize;
        let page = page.max(self.affected_rect().height());
        let (rows, tracks) = self.pattern_geometry();
        match action {
            Action::Quit => self.quit = true,
            Action::MoveLeft => self.track = self.track.saturating_sub(1),
            Action::MoveRight => self.track = (self.track + 1).min(tracks.saturating_sub(1)),
            Action::MoveUp => self.row = self.row.saturating_sub(1),
            Action::MoveDown => self.row = (self.row + 1).min(rows.saturating_sub(1)),
            Action::PageUp => self.row = self.row.saturating_sub(page),
            Action::PageDown => self.row = (self.row + page).min(rows.saturating_sub(1)),
            Action::Home => self.row = 0,
            Action::End => self.row = rows.saturating_sub(1),
            Action::TopLeft => {
                self.row = 0;
                self.track = 0;
            }
            Action::BottomRight => {
                self.row = rows.saturating_sub(1);
                self.track = tracks.saturating_sub(1);
            }
            Action::NextTrack => self.track = (self.track + 1).min(tracks.saturating_sub(1)),
            Action::PrevTrack => self.track = self.track.saturating_sub(1),
            Action::TrackInsert => self.insert_track(),
            Action::TrackDelete => self.delete_track(),
            Action::BrushWider => self.brush_width = self.brush_width.grow(),
            Action::BrushNarrower => self.brush_width = self.brush_width.shrink(),
            Action::BrushTaller => self.brush_height = self.brush_height.grow(),
            Action::BrushShorter => self.brush_height = self.brush_height.shrink(),
            Action::SelectLeft => self.extend_selection(-1, 0),
            Action::SelectRight => self.extend_selection(1, 0),
            Action::SelectUp => self.extend_selection(0, -1),
            Action::SelectDown => self.extend_selection(0, 1),
            Action::InsertBlock => self.insert_block(),
            Action::DeleteBlock => self.delete_block(),
            Action::BackspaceBlock => self.backspace_block(),
            Action::ClearSelection => {
                self.selection = None;
            }
            Action::Cut => self.cut(),
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::TogglePlay => self.toggle_playback(),
            Action::SetStartRow => self.play_start_row = self.row,
            Action::CommandMode => self.enter_command_mode(),
            Action::ToggleNoteMode => {
                self.mode = if self.mode == Mode::Note {
                    Mode::Normal
                } else {
                    Mode::Note
                };
            }
            Action::ToggleChromatic => self.note_chromatic = !self.note_chromatic,
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Save => {
                let _ = self.save(None);
            }
            Action::OpenEventEditor => self.open_event_editor(),
        }
    }

    fn extend_selection(&mut self, dx: isize, dy: isize) {
        let (rows, tracks) = self.pattern_geometry();
        let (base_row, base_track) = match self.selection {
            Some(selection) => (selection.head.0, selection.head.1),
            None => (self.row, self.track),
        };
        let head_row = (base_row as isize + dy).clamp(0, rows.saturating_sub(1) as isize) as usize;
        let head_track =
            (base_track as isize + dx).clamp(0, tracks.saturating_sub(1) as isize) as usize;
        let anchor = self
            .selection
            .map(|s| s.anchor)
            .unwrap_or((self.row, self.track));
        self.selection = Some(Selection {
            anchor,
            head: (head_row, head_track),
        });
        self.row = head_row;
        self.track = head_track;
    }

    // --- Blocks ---------------------------------------------------------------

    fn snapshot(&mut self) {
        let snapshot = self.project.composition.clone();
        self.undo.record(snapshot);
        self.dirty = true;
    }

    fn insert_block(&mut self) {
        let rect = self.affected_rect();
        let height = rect.height();
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let rows = pattern.row_count();
        for track in rect.left..=rect.right {
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
    }

    fn delete_block(&mut self) {
        let rect = self.affected_rect();
        self.delete_rows(rect.top, rect.height(), rect.left, rect.right);
    }

    fn backspace_block(&mut self) {
        let rect = self.affected_rect();
        let start = rect.top.saturating_sub(rect.height());
        self.delete_rows(start, rect.height(), rect.left, rect.right);
    }

    fn delete_rows(&mut self, start: usize, height: usize, left: usize, right: usize) {
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let rows = pattern.row_count();
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
    }

    fn copy(&mut self) {
        let rect = self.affected_rect();
        self.clipboard = Clipboard::from_pattern(self.pattern(), rect);
        self.set_message(
            format!(
                "copied {}x{}",
                self.clipboard.height(),
                self.clipboard.width()
            ),
            false,
        );
    }

    fn cut(&mut self) {
        let rect = self.affected_rect();
        self.clipboard = Clipboard::from_pattern(self.pattern(), rect);
        self.clear_rect(rect);
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let origin_row = self.row;
        let origin_track = self.track;
        let cells = self.clipboard.cells.clone();
        let (rows, tracks) = self.pattern_geometry();
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for (dr, row) in cells.iter().enumerate() {
            for (dt, cell) in row.iter().enumerate() {
                let target_row = origin_row + dr;
                let target_track = origin_track + dt;
                if target_row < rows && target_track < tracks {
                    if let Some(slot) = pattern.slot_mut(target_row, target_track) {
                        *slot = cell.clone();
                    }
                }
            }
        }
    }

    fn clear_rect(&mut self, rect: Rect) {
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        for row in rect.top..=rect.bottom {
            for track in rect.left..=rect.right {
                if let Some(slot) = pattern.slot_mut(row, track) {
                    *slot = Slot::empty();
                }
            }
        }
    }

    fn insert_track(&mut self) {
        let tracks = self.pattern().track_count();
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let _ = pattern.resize_tracks(tracks + 1);
    }

    fn delete_track(&mut self) {
        let tracks = self.pattern().track_count();
        if tracks <= 1 {
            return;
        }
        self.snapshot();
        let pattern = self
            .project
            .composition
            .pattern_mut(self.current)
            .expect("current pattern");
        let _ = pattern.resize_tracks(tracks - 1);
        self.clamp_cursor();
    }

    // --- Note entry -----------------------------------------------------------

    /// Handles a key in note mode, returning whether it was consumed.
    pub fn note_key(&mut self, c: char) -> bool {
        let mapping = if self.note_chromatic {
            chromatic_map()
        } else {
            scale_map()
        };
        let Some(degree) = mapping
            .iter()
            .find(|(key, _)| *key == c)
            .map(|(_, degree)| *degree)
        else {
            return false;
        };
        let settings = self.effective_settings();
        let mut effective = settings;
        effective.scale_aware = !self.note_chromatic;
        let note = effective.degree_to_note(degree);
        self.snapshot();
        let event = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            note,
            Some(100),
        ));
        if let Some(pattern) = self.project.composition.pattern_mut(self.current) {
            pattern.set_event(self.row, self.track, event);
        }
        self.audition(note);
        let (rows, _) = self.pattern_geometry();
        self.row = (self.row + 1).min(rows.saturating_sub(1));
        true
    }

    fn audition(&mut self, note: u8) {
        let on = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            note,
            Some(100),
        ));
        let off = Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOff,
            0,
            note,
            Some(0),
        ));
        self.send(&on);
        self.send(&off);
    }

    fn send(&mut self, event: &Event) {
        if let Some(midi) = self.midi.as_mut() {
            let _ = midi.send_event(event);
        }
    }

    // --- Undo ---------------------------------------------------------------

    fn undo(&mut self) {
        let current = self.project.composition.clone();
        if let Some(previous) = self.undo.undo(current) {
            self.project.composition = previous;
            self.after_composition_change();
        }
    }

    fn redo(&mut self) {
        let current = self.project.composition.clone();
        if let Some(next) = self.undo.redo(current) {
            self.project.composition = next;
            self.after_composition_change();
        }
    }

    fn after_composition_change(&mut self) {
        if self.project.composition.pattern(self.current).is_none() {
            self.current = self.project.composition.selected_main();
        }
        self.clamp_cursor();
        self.selection = None;
        self.dirty = true;
    }

    // --- Event editor ---------------------------------------------------------

    fn open_event_editor(&mut self) {
        let slot = self.pattern().slot(self.row, self.track).cloned();
        if let Some(slot) = slot {
            if let Some(editor) = EventEditor::from_event(self.row, self.track, &slot.event) {
                self.input = editor.text();
                self.event_editor = Some(editor);
                self.previous_mode = self.mode;
                self.mode = Mode::EventEditor;
            } else {
                self.set_message("nothing to edit".to_string(), true);
            }
        }
    }

    fn commit_event_editor(&mut self) {
        if let Some(editor) = self.event_editor.take() {
            let event = editor.commit();
            let (row, track) = (editor.row, editor.track);
            self.snapshot();
            if let Some(pattern) = self.project.composition.pattern_mut(self.current) {
                pattern.set_event(row, track, event);
            }
        }
        self.mode = self.previous_mode;
        self.input.clear();
    }

    // --- Command layer --------------------------------------------------------

    fn enter_command_mode(&mut self) {
        self.previous_mode = if self.mode == Mode::Note {
            Mode::Normal
        } else {
            self.mode
        };
        self.mode = Mode::Command;
        self.input.clear();
    }

    /// Executes a parsed command.
    pub fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::Read(path) => return self.load(path),
            Command::Write(path) => return self.save(path),
            Command::Export(path) => return self.export(path),
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
                self.project
                    .composition
                    .set_pattern_lpb(self.current, value)
                    .map_err(|e| e.to_string())?;
            }
            Command::Root(note) => {
                let midi = mtrak_core::settings::parse_note(&note)
                    .ok_or_else(|| format!("invalid note: {note}"))?;
                self.project.composition.global_settings_mut().root = Setting::Set(midi);
            }
            Command::Scale(args) => return self.execute_scale(args),
            Command::Mode(value) => {
                self.project.composition.global_settings_mut().modal_degree =
                    Setting::Set(value as u8);
            }
            Command::Rows(value) => {
                let value = value as usize;
                self.project
                    .composition
                    .pattern_mut(self.current)
                    .ok_or_else(|| "no pattern".to_string())?
                    .resize_rows(value)
                    .map_err(|e| e.to_string())?;
                self.clamp_cursor();
            }
            Command::Tracks(value) => {
                let value = value as usize;
                self.project
                    .composition
                    .pattern_mut(self.current)
                    .ok_or_else(|| "no pattern".to_string())?
                    .resize_tracks(value)
                    .map_err(|e| e.to_string())?;
                self.clamp_cursor();
            }
            Command::PatternNew(lpb) => {
                let id = self
                    .project
                    .composition
                    .create_pattern(lpb.unwrap_or(4))
                    .map_err(|e| e.to_string())?;
                self.current = id;
                self.row = 0;
                self.track = 0;
            }
            Command::PatternSelect(id) => {
                if self.project.composition.pattern(id).is_none() {
                    return Err(format!("pattern {id} does not exist"));
                }
                self.current = id;
                self.clamp_cursor();
            }
            Command::PatternDelete(id) => {
                self.project
                    .composition
                    .delete_pattern(id)
                    .map_err(|e| e.to_string())?;
                if self.current == id {
                    self.current = self.project.composition.selected_main();
                }
                self.clamp_cursor();
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
            Command::Help => {
                self.set_message(
                    "read write export bpm tpb lpb root scale mode rows tracks pattern port"
                        .to_string(),
                    false,
                );
            }
        }
        self.dirty = true;
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
            self.project.composition.global_settings_mut().scale = Setting::Set(scale);
        }
        if let Some(name) = args.use_name {
            let scale = self
                .project
                .composition
                .scales()
                .get(&name)
                .ok_or_else(|| format!("unknown scale: {name}"))?;
            self.project.composition.global_settings_mut().scale = Setting::Set(scale);
        }
        if let Some(note) = args.root {
            let midi = mtrak_core::settings::parse_note(&note)
                .ok_or_else(|| format!("invalid note: {note}"))?;
            self.project.composition.global_settings_mut().root = Setting::Set(midi);
        }
        if let Some(mode) = args.mode {
            self.project.composition.global_settings_mut().modal_degree = Setting::Set(mode as u8);
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

    fn execute_command_line(&mut self) {
        let input = std::mem::take(&mut self.input);
        self.mode = self.previous_mode;
        match command::parse(&input) {
            Ok(command) => {
                if let Err(error) = self.execute(command) {
                    self.set_message(error, true);
                }
            }
            Err(error) => self.set_message(error, true),
        }
    }

    fn set_message(&mut self, text: String, is_error: bool) {
        self.message = Some(Message { text, is_error });
    }

    // --- File operations ------------------------------------------------------

    fn resolve_path(&self, given: Option<String>) -> Result<PathBuf, String> {
        match given {
            Some(path) => Ok(PathBuf::from(path)),
            None => self
                .path
                .clone()
                .ok_or_else(|| "no filename given".to_string()),
        }
    }

    /// Saves the project as a tracker file.
    pub fn save(&mut self, path: Option<String>) -> Result<(), String> {
        let path = self.resolve_path(path)?;
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
        self.current = self.project.composition.selected_main();
        self.row = 0;
        self.track = 0;
        self.selection = None;
        self.undo = UndoStack::new(64);
        self.path = Some(path.clone());
        self.dirty = false;
        self.project_revision += 1;
        self.lua_commands.clear();
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
            self.stop_playback();
        } else {
            self.start_playback();
        }
    }

    /// Starts playback from the configured start row.
    pub fn start_playback(&mut self) {
        let events = self.render();
        let tpb = self.project.composition.tpb();
        let row_len = row_ticks(tpb, self.pattern().lpb()).unwrap_or(1) as u64;
        let base = self.play_start_row as u64 * row_len;
        self.play_tick = base;
        self.play_current_row = self.play_start_row;
        self.play_next = events
            .iter()
            .position(|e| e.tick >= base)
            .unwrap_or(events.len());
        self.play_events = events;
        self.play_micros = 0.0;
        self.playing = true;
    }

    /// Stops playback.
    pub fn stop_playback(&mut self) {
        self.playing = false;
        self.play_events.clear();
        self.play_next = 0;
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
        if whole_ticks == 0 {
            return;
        }
        self.play_micros -= whole_ticks as f64 * micros_per_tick;
        self.play_tick += whole_ticks;

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
        if self.play_next >= self.play_events.len() {
            self.stop_playback();
        }
    }

    // --- Key dispatch ---------------------------------------------------------

    /// Handles a key press according to the current mode.
    pub fn handle_key(&mut self, key: Key) {
        match self.mode {
            Mode::Command => self.handle_command_key(key),
            Mode::EventEditor => self.handle_editor_key(key),
            _ => self.handle_normal_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: Key) {
        if self.mode == Mode::Note {
            if let Code::Char(c) = key.code {
                if !key.ctrl && !key.alt && self.note_key(c) {
                    return;
                }
            }
        }
        if let Some(action) = key::lookup(key) {
            self.apply(action);
        }
    }

    fn handle_command_key(&mut self, key: Key) {
        match key.code {
            Code::Esc => {
                self.input.clear();
                self.mode = self.previous_mode;
            }
            Code::Enter => self.execute_command_line(),
            Code::Backspace => {
                self.input.pop();
            }
            Code::Char(c) if !key.ctrl && !key.alt => self.input.push(c),
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: Key) {
        match key.code {
            Code::Esc => {
                self.event_editor = None;
                self.input.clear();
                self.mode = self.previous_mode;
            }
            Code::Enter => self.commit_event_editor(),
            Code::Left => {
                if let Some(editor) = self.event_editor.as_mut() {
                    editor.move_left();
                    self.input = editor.text();
                }
            }
            Code::Right => {
                if let Some(editor) = self.event_editor.as_mut() {
                    editor.move_right();
                    self.input = editor.text();
                }
            }
            Code::Char(c) if !key.ctrl && !key.alt => {
                if let Some(value) = c.to_digit(16) {
                    if let Some(editor) = self.event_editor.as_mut() {
                        editor.set_nibble(value as u8);
                        editor.move_right();
                        self.input = editor.text();
                    }
                }
            }
            _ => {}
        }
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
    use mtrak_core::event::ChannelKind;
    use mtrak_midi::MockBackend;

    fn app_with(rows: usize, tracks: usize) -> App {
        let mut project = mtrak_midi_io::TrackerProject {
            composition: Composition::new(),
            lua_modules: Default::default(),
            setup_source: None,
        };
        {
            let pattern = project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(rows).unwrap();
            pattern.resize_tracks(tracks).unwrap();
        }
        App::new(project)
    }

    fn slot_data1(app: &App, row: usize, track: usize) -> Option<u8> {
        match &app.pattern().slot(row, track).unwrap().event {
            Event::Channel(m) => m.data1,
            _ => None,
        }
    }

    fn note_event(key: u8) -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            key,
            Some(100),
        ))
    }

    #[test]
    fn cursor_navigation_stays_in_bounds() {
        let mut app = app_with(4, 2);
        app.apply(Action::MoveDown);
        app.apply(Action::MoveDown);
        app.apply(Action::MoveDown);
        app.apply(Action::MoveDown);
        assert_eq!(app.row, 3);
        app.apply(Action::MoveRight);
        app.apply(Action::MoveRight);
        assert_eq!(app.track, 1);
        app.apply(Action::Home);
        assert_eq!(app.row, 0);
        app.apply(Action::End);
        assert_eq!(app.row, 3);
    }

    #[test]
    fn brush_settings_cycle_through_their_values() {
        let mut app = app_with(64, 16);
        assert_eq!(app.brush_width(), BrushWidth::One);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Two);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Six);
        app.apply(Action::BrushWider);
        assert_eq!(app.brush_width(), BrushWidth::Full);
        app.apply(Action::BrushNarrower);
        assert_eq!(app.brush_width(), BrushWidth::Six);
        app.apply(Action::BrushTaller);
        assert_eq!(app.brush_height(), BrushHeight::Lpb);
        app.apply(Action::BrushTaller);
        assert_eq!(app.brush_height(), BrushHeight::Full);
        assert_eq!(app.affected_rect().height(), 64);
    }

    #[test]
    fn selection_extends_and_clears() {
        let mut app = app_with(8, 4);
        app.apply(Action::SelectDown);
        app.apply(Action::SelectRight);
        assert_eq!(
            app.selection(),
            Some(Selection {
                anchor: (0, 0),
                head: (1, 1)
            })
        );
        assert_eq!(app.affected_rect().height(), 2);
        assert_eq!(app.affected_rect().width(), 2);
        app.apply(Action::ClearSelection);
        assert!(app.selection().is_none());
    }

    #[test]
    fn insert_delete_and_clear_shift_columns() {
        let mut app = app_with(4, 1);
        {
            let pattern = app.project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.set_event(0, 0, note_event(60));
            pattern.set_event(1, 0, note_event(61));
            pattern.set_event(2, 0, note_event(62));
        }
        // Select the first row (one cell), then insert.
        app.apply(Action::InsertBlock);
        assert!(app.pattern().slot(0, 0).unwrap().is_empty());
        assert_eq!(app.pattern().slot(1, 0).unwrap().event, note_event(60));
        assert_eq!(app.pattern().slot(2, 0).unwrap().event, note_event(61));
        // Delete moves them back up.
        app.apply(Action::DeleteBlock);
        assert_eq!(app.pattern().slot(0, 0).unwrap().event, note_event(60));
        assert_eq!(app.pattern().slot(1, 0).unwrap().event, note_event(61));
        assert_eq!(app.pattern().slot(2, 0).unwrap().event, note_event(62));
        // Clear empties the selection.
        app.apply(Action::ClearSelection);
        app.apply(Action::ClearSelection);
    }

    #[test]
    fn cut_copy_paste_round_trip() {
        let mut app = app_with(4, 2);
        {
            let pattern = app.project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.set_event(0, 0, note_event(60));
            pattern.set_event(1, 0, note_event(61));
        }
        app.apply(Action::SelectDown); // select rows 0..1
        app.apply(Action::Copy);
        app.apply(Action::ClearSelection);
        // Paste at row 2.
        app.row = 2;
        app.apply(Action::Paste);
        assert_eq!(app.pattern().slot(2, 0).unwrap().event, note_event(60));
        assert_eq!(app.pattern().slot(3, 0).unwrap().event, note_event(61));
        // Cut clears the source.
        app.row = 0;
        app.apply(Action::SelectDown);
        app.apply(Action::Cut);
        assert!(app.pattern().slot(0, 0).unwrap().is_empty());
    }

    #[test]
    fn undo_redo_restores_state_and_is_capped() {
        let mut app = app_with(6, 1);
        for i in 0..5u8 {
            app.row = i as usize;
            app.note_key('z');
        }
        assert!(app.undo.can_undo());
        app.apply(Action::Undo);
        assert!(app.pattern().slot(4, 0).unwrap().is_empty());
        app.apply(Action::Redo);
        assert_eq!(slot_data1(&app, 4, 0), Some(60));
        // Drive past the cap and confirm it is bounded.
        for i in 0..80u8 {
            app.row = (i as usize) % 6;
            app.note_key('z');
        }
        assert!(app.undo.undo_len() <= 64);
    }

    #[test]
    fn note_entry_maps_degrees_and_clamps() {
        let mut app = app_with(8, 1);
        app.note_key('z'); // degree 0 -> C4 (60)
        app.note_key('x'); // degree 1 -> D4 (62)
        assert_eq!(slot_data1(&app, 0, 0), Some(60));
        assert_eq!(slot_data1(&app, 1, 0), Some(62));
        // Clamp at the top.
        app.project.composition.global_settings_mut().root = Setting::Set(120);
        app.note_chromatic = true;
        app.row = 2;
        app.note_key('i'); // semitone 24 -> 144 clamped to 127
        assert_eq!(slot_data1(&app, 2, 0), Some(127));
    }

    #[test]
    fn note_entry_auditions_through_the_selected_port() {
        let mut app = app_with(4, 1);
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);
        app.note_key('z');
        assert_eq!(
            sent.borrow().as_slice(),
            &[vec![0x90, 60, 100], vec![0x80, 60, 0]]
        );
    }

    #[test]
    fn retained_commands_mutate_expected_state() {
        let mut app = app_with(4, 1);
        app.execute(Command::Bpm(140)).unwrap();
        assert_eq!(app.project.composition.bpm(), Bpm::from_int(140).unwrap());
        app.execute(Command::Rows(8)).unwrap();
        assert_eq!(app.pattern().row_count(), 8);
        app.execute(Command::Tracks(3)).unwrap();
        assert_eq!(app.pattern().track_count(), 3);
        app.execute(Command::Root("C-5".to_string())).unwrap();
        assert_eq!(app.effective_settings().root, 72);
        app.execute(Command::Lpb(8)).unwrap();
        assert_eq!(app.pattern().lpb(), 8);
    }

    #[test]
    fn invalid_command_values_do_not_mutate_state() {
        let mut app = app_with(4, 1);
        assert!(app.execute(Command::Tracks(0)).is_err());
        assert_eq!(app.pattern().track_count(), 1);
        assert!(app.execute(Command::Root("nonsense".to_string())).is_err());
        assert_eq!(app.effective_settings().root, 60);
    }

    #[test]
    fn pattern_management_uses_stable_ids() {
        let mut app = app_with(4, 1);
        app.execute(Command::PatternNew(Some(4))).unwrap();
        let created = app.current_pattern();
        assert_eq!(created.value(), 1);
        app.execute(Command::PatternSelect(PatternId(0))).unwrap();
        app.execute(Command::PatternDelete(created)).unwrap();
        assert!(app.project.composition.pattern(created).is_none());
    }

    #[test]
    fn event_editor_opens_commits_and_cancels() {
        let mut app = app_with(4, 1);
        {
            let pattern = app.project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.set_event(
                0,
                0,
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    0,
                    60,
                    Some(100),
                )),
            );
        }
        app.apply(Action::OpenEventEditor);
        assert_eq!(app.mode(), Mode::EventEditor);
        app.handle_key(Key::ctrl(Code::Char('x'))); // ignored in editor mode
        assert_eq!(app.mode(), Mode::EventEditor);
        app.handle_key(Key::new(Code::Enter));
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.pattern().slot(0, 0).unwrap().event, note_event(60));
    }

    #[test]
    fn project_commands_never_leak_into_export() {
        let mut app = app_with(4, 1);
        app.set_lua_commands(vec![LuaCommand {
            name: "sekrit".to_string(),
            description: Some("hidden".to_string()),
        }]);
        let path = std::env::temp_dir().join("mtrak_export_leak_test.mid");
        app.export(Some(path.to_string_lossy().to_string()))
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(6).any(|w| w == b"sekrit"),
            "project-only commands must not be serialized"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn playback_advances_and_sends_due_events() {
        let mut app = app_with(4, 1);
        {
            let pattern = app.project.composition.pattern_mut(PatternId(0)).unwrap();
            pattern.set_event(0, 0, note_event(60));
            pattern.set_event(1, 0, note_event(62));
        }
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent_handle();
        let mut manager = OutputManager::new(Box::new(backend) as Box<dyn MidiBackend>);
        manager.select(0).unwrap();
        app.set_midi(manager);
        app.start_playback();
        assert!(app.is_playing());
        // tpb 24, lpb 4 -> 6 ticks per row; 120 bpm -> 500000 us / 24 ticks.
        let us_per_tick = (500_000.0 / 24.0) as u64;
        app.tick_playback(us_per_tick * 6); // reach row 1 (tick 6)
        assert_eq!(sent.borrow().len(), 1, "first note sent");
        app.tick_playback(us_per_tick * 6);
        assert_eq!(sent.borrow().len(), 2, "second note sent");
    }
}
