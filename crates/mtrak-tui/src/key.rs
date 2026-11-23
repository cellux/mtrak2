//! Key events and the keymap that turns them into [`Action`]s.
//!
//! The keymap is defined here, independent of any terminal crate, so it can be
//! tested and remapped without touching the engine. The binary translates
//! `crossterm` events into [`Key`] values.

/// A logical key code.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Code {
    /// A printable character.
    Char(char),
    /// Enter/Return.
    Enter,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Tab.
    Tab,
    /// A function key (`1`–`12`).
    Function(u8),
}

/// A key press with modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Key {
    /// The logical key.
    pub code: Code,
    /// Whether Control was held.
    pub ctrl: bool,
    /// Whether Shift was held.
    pub shift: bool,
    /// Whether Alt was held.
    pub alt: bool,
}

impl Key {
    /// A plain key with no modifiers.
    pub fn new(code: Code) -> Self {
        Key {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// A Control-modified key.
    pub fn ctrl(code: Code) -> Self {
        Key {
            code,
            ctrl: true,
            shift: false,
            alt: false,
        }
    }

    /// A Shift-modified key.
    pub fn shift(code: Code) -> Self {
        Key {
            code,
            ctrl: false,
            shift: true,
            alt: false,
        }
    }

    /// A Control+Shift-modified key.
    pub fn ctrl_shift(code: Code) -> Self {
        Key {
            code,
            ctrl: true,
            shift: true,
            alt: false,
        }
    }

    /// An Alt-modified key.
    pub fn alt(code: Code) -> Self {
        Key {
            code,
            ctrl: false,
            shift: false,
            alt: true,
        }
    }

    /// A plain character key.
    pub fn ch(c: char) -> Self {
        Key::new(Code::Char(c))
    }
}

/// An editing or navigation action selected by the default keymap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Quit the application.
    Quit,
    /// Move the cursor left.
    MoveLeft,
    /// Move the cursor right.
    MoveRight,
    /// Move the cursor up.
    MoveUp,
    /// Move the cursor down.
    MoveDown,
    /// Move by a page.
    PageUp,
    /// Move by a page.
    PageDown,
    /// Move to the first row.
    Home,
    /// Move to the last row.
    End,
    /// Move to the top-left.
    TopLeft,
    /// Move to the bottom-right.
    BottomRight,
    /// Select the next track.
    NextTrack,
    /// Select the previous track.
    PrevTrack,
    /// Insert a track.
    TrackInsert,
    /// Delete a track.
    TrackDelete,
    /// Grow the brush width.
    BrushWider,
    /// Shrink the brush width.
    BrushNarrower,
    /// Grow the brush height.
    BrushTaller,
    /// Shrink the brush height.
    BrushShorter,
    /// Extend the selection left.
    SelectLeft,
    /// Extend the selection right.
    SelectRight,
    /// Extend the selection up.
    SelectUp,
    /// Extend the selection down.
    SelectDown,
    /// Insert a block.
    InsertBlock,
    /// Delete a block.
    DeleteBlock,
    /// Delete a block backward.
    BackspaceBlock,
    /// Clear the selection.
    CancelSelection,
    /// Clear every whole slot touched by the selection/brush.
    ClearBlock,
    /// Toggle offset visibility.
    ToggleOffsets,
    /// Switch to the Help screen.
    HelpScreen,
    /// Switch to the file browser.
    FileBrowserScreen,
    /// Switch to the Pattern screen.
    PatternScreen,
    /// Switch to the Code screen.
    CodeScreen,
    /// Open the pattern/module chooser.
    OpenChooser,
    /// Validate/reload the project Lua runtime.
    ReloadLua,
    /// Cut the selection.
    Cut,
    /// Copy the selection.
    Copy,
    /// Paste the clipboard.
    Paste,
    /// Toggle playback.
    TogglePlay,
    /// Set the playback start row.
    SetStartRow,
    /// Enter PLAY mode.
    PlayMode,
    /// Enter REC mode.
    RecMode,
    /// Enter EDIT mode.
    EditMode,
    /// Set the recording step (`Alt+0`..`Alt+9`).
    SetRecordStep(u8),
    /// Decrease the keyboard transpose by an octave.
    TransposeDown,
    /// Increase the keyboard transpose by an octave.
    TransposeUp,
    /// Enter command mode.
    CommandMode,
    /// Silence every channel.
    AllNotesOff,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Save the project.
    Save,
    /// Open the full event editor on the current slot.
    OpenEventEditor,
}

/// Maps a key to its action, if any (see `PLAN.md` §2.10).
pub fn lookup(key: Key) -> Option<Action> {
    use Action as A;
    use Code::*;
    // Alt-modified bindings: unlike Ctrl-Enter or Ctrl-. these are
    // distinguishable from the plain keys in every common terminal.
    if key.alt {
        match key.code {
            Enter => return Some(A::OpenChooser),
            Char('.') => return Some(A::AllNotesOff),
            Char(c @ '0'..='9') => return Some(A::SetRecordStep(c as u8 - b'0')),
            _ => {}
        }
    }
    let action = match (key.code, key.ctrl, key.shift) {
        (Char('q'), true, _) => A::Quit,
        (Left, false, false) => A::MoveLeft,
        (Right, false, false) => A::MoveRight,
        (Up, false, false) => A::MoveUp,
        (Down, false, false) => A::MoveDown,
        (PageUp, _, _) => A::PageUp,
        (PageDown, _, _) => A::PageDown,
        (Home, false, _) => A::Home,
        (End, false, _) => A::End,
        (Home, true, _) => A::TopLeft,
        (End, true, _) => A::BottomRight,
        (Tab, false, false) => A::NextTrack,
        (Tab, false, true) => A::PrevTrack,
        (Right, true, true) => A::TrackInsert,
        (Left, true, true) => A::TrackDelete,
        (Right, true, false) => A::BrushWider,
        (Left, true, false) => A::BrushNarrower,
        (Down, true, false) => A::BrushTaller,
        (Up, true, false) => A::BrushShorter,
        (Left, false, true) => A::SelectLeft,
        (Right, false, true) => A::SelectRight,
        (Up, false, true) => A::SelectUp,
        (Down, false, true) => A::SelectDown,
        (Insert, _, _) => A::InsertBlock,
        (Down, true, true) => A::InsertBlock,
        (Delete, false, _) => A::DeleteBlock,
        (Up, true, true) => A::DeleteBlock,
        (Backspace, false, false) => A::BackspaceBlock,
        (Char('.'), false, _) => A::ClearBlock,
        (Esc, false, _) => A::CancelSelection,
        (Char('o'), false, _) => A::ToggleOffsets,
        (Char('x'), true, _) => A::Cut,
        (Char('c'), true, _) => A::Copy,
        (Char('v'), true, _) => A::Paste,
        (Char(' '), false, _) => A::TogglePlay,
        (Char('S'), false, _) => A::SetStartRow,
        (Char(':'), false, _) => A::CommandMode,
        (Char('p'), true, _) => A::PlayMode,
        (Char('r'), true, _) => A::RecMode,
        (Char('e'), true, _) => A::EditMode,
        (Char('z'), true, _) => A::Undo,
        (Char('y'), true, _) => A::Redo,
        (Char('s'), true, _) => A::Save,
        // Ctrl-Enter is indistinguishable from Enter in many terminals, so it
        // deliberately keeps the plain-Enter behavior.
        (Enter, _, _) => A::OpenEventEditor,
        (Function(1), false, _) => A::HelpScreen,
        (Function(2), false, _) => A::FileBrowserScreen,
        (Function(3), false, _) => A::PatternScreen,
        (Function(4), false, _) => A::CodeScreen,
        (Function(5), false, _) => A::ReloadLua,
        (Function(7), false, _) => A::TransposeDown,
        (Function(8), false, _) => A::TransposeUp,
        _ => return None,
    };
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_match_the_spec() {
        assert_eq!(lookup(Key::ctrl(Code::Char('q'))), Some(Action::Quit));
        assert_eq!(lookup(Key::new(Code::Left)), Some(Action::MoveLeft));
        assert_eq!(lookup(Key::new(Code::PageUp)), Some(Action::PageUp));
        assert_eq!(lookup(Key::ctrl(Code::Home)), Some(Action::TopLeft));
        assert_eq!(lookup(Key::new(Code::Tab)), Some(Action::NextTrack));
        assert_eq!(lookup(Key::shift(Code::Tab)), Some(Action::PrevTrack));
        assert_eq!(
            lookup(Key::ctrl_shift(Code::Right)),
            Some(Action::TrackInsert)
        );
        assert_eq!(lookup(Key::ctrl(Code::Right)), Some(Action::BrushWider));
        assert_eq!(lookup(Key::ctrl(Code::Down)), Some(Action::BrushTaller));
        assert_eq!(lookup(Key::shift(Code::Down)), Some(Action::SelectDown));
        assert_eq!(lookup(Key::new(Code::Insert)), Some(Action::InsertBlock));
        assert_eq!(
            lookup(Key::ctrl_shift(Code::Down)),
            Some(Action::InsertBlock)
        );
        assert_eq!(lookup(Key::new(Code::Delete)), Some(Action::DeleteBlock));
        assert_eq!(lookup(Key::ctrl_shift(Code::Up)), Some(Action::DeleteBlock));
        assert_eq!(
            lookup(Key::new(Code::Backspace)),
            Some(Action::BackspaceBlock)
        );
        assert_eq!(lookup(Key::ch('.')), Some(Action::ClearBlock));
        assert_eq!(lookup(Key::ctrl(Code::Char('x'))), Some(Action::Cut));
        assert_eq!(lookup(Key::ch(' ')), Some(Action::TogglePlay));
        assert_eq!(
            lookup(Key::new(Code::Function(7))),
            Some(Action::TransposeDown)
        );
        assert_eq!(
            lookup(Key::new(Code::Function(8))),
            Some(Action::TransposeUp)
        );
        assert_eq!(lookup(Key::ch('S')), Some(Action::SetStartRow));
        assert_eq!(lookup(Key::ch('s')), None, "lowercase s is a piano key");
        assert_eq!(lookup(Key::ch(':')), Some(Action::CommandMode));
        assert_eq!(lookup(Key::ctrl(Code::Char('p'))), Some(Action::PlayMode));
        assert_eq!(lookup(Key::ctrl(Code::Char('r'))), Some(Action::RecMode));
        assert_eq!(lookup(Key::ctrl(Code::Char('e'))), Some(Action::EditMode));
        assert_eq!(
            lookup(Key::alt(Code::Char('0'))),
            Some(Action::SetRecordStep(0))
        );
        assert_eq!(
            lookup(Key::alt(Code::Char('9'))),
            Some(Action::SetRecordStep(9))
        );
        assert_eq!(lookup(Key::ctrl(Code::Char('z'))), Some(Action::Undo));
        assert_eq!(lookup(Key::ctrl(Code::Char('y'))), Some(Action::Redo));
        assert_eq!(lookup(Key::ctrl(Code::Char('s'))), Some(Action::Save));
        assert_eq!(lookup(Key::new(Code::Enter)), Some(Action::OpenEventEditor));
        assert_eq!(
            lookup(Key::ctrl(Code::Enter)),
            Some(Action::OpenEventEditor),
            "Ctrl-Enter is not a supported binding"
        );
        assert_eq!(lookup(Key::alt(Code::Enter)), Some(Action::OpenChooser));
        assert_eq!(
            lookup(Key::new(Code::Function(1))),
            Some(Action::HelpScreen)
        );
        assert_eq!(
            lookup(Key::new(Code::Function(2))),
            Some(Action::FileBrowserScreen)
        );
        assert_eq!(
            lookup(Key::new(Code::Function(3))),
            Some(Action::PatternScreen)
        );
        assert_eq!(
            lookup(Key::new(Code::Function(4))),
            Some(Action::CodeScreen)
        );
    }

    #[test]
    fn removed_note_and_follow_bindings_have_no_action() {
        assert_eq!(lookup(Key::ctrl(Code::Char('n'))), None);
        assert_eq!(lookup(Key::ctrl(Code::Char('f'))), None);
        assert_eq!(lookup(Key::ch('N')), None);
    }
}
