//! Filesystem browser state for the `F2` workspace.
//!
//! The module owns directory listing, selection, filtering, and the
//! parent/child navigation memory. It deliberately does not depend on
//! `ratatui`; [`crate::app::App`] coordinates workspace switching, MIDI loading,
//! and status messages, while [`crate::ui`] only draws this state.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::line::LineEditor;

/// What a directory entry is, after following a symlink when possible.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// A directory (or a symlink that resolves to one).
    Directory,
    /// A regular file (or a symlink that resolves to one).
    File,
    /// A broken or unreadable entry. It stays visible but cannot be used.
    Unreadable,
}

impl EntryKind {
    /// Whether this is a directory.
    pub fn is_dir(self) -> bool {
        self == EntryKind::Directory
    }
}

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The entry name as it appears in its parent directory.
    pub name: String,
    /// The absolute path to the entry.
    pub path: PathBuf,
    /// What kind of entry this is.
    pub kind: EntryKind,
}

impl Entry {
    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }

    /// The displayed label: directory names carry a trailing `/`.
    pub fn label(&self) -> String {
        if self.is_dir() {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The browser's filesystem state.
pub struct Browser {
    dir: PathBuf,
    entries: Vec<Entry>,
    selected: Option<String>,
    filter: LineEditor,
    /// The parent-directory entry name used to enter each child directory.
    child_memory: HashMap<PathBuf, String>,
}

impl Browser {
    /// Opens `dir` and makes it the process current directory.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        let dir = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir()
                .map_err(|e| format!("cannot determine the current directory: {e}"))?
                .join(dir)
        };
        let entries = read_entries(&dir)?;
        std::env::set_current_dir(&dir)
            .map_err(|e| format!("cannot change directory to {}: {e}", dir.display()))?;
        Ok(Browser {
            dir,
            entries,
            selected: None,
            filter: LineEditor::new(),
            child_memory: HashMap::new(),
        })
    }

    /// The current absolute directory.
    pub fn directory(&self) -> &Path {
        &self.dir
    }

    /// Every entry in the current directory, sorted.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The filter text.
    pub fn filter(&self) -> String {
        self.filter.text()
    }

    /// Whether the filter is empty.
    pub fn filter_is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    /// The selected entry name, if one is visible.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The selected entry, if one is visible.
    pub fn selected_entry(&self) -> Option<&Entry> {
        let selected = self.selected.as_deref()?;
        self.entries.iter().find(|entry| entry.name == selected)
    }

    /// Whether `name` is the current selection.
    pub fn is_selected(&self, name: &str) -> bool {
        self.selected.as_deref() == Some(name)
    }

    /// The entries matching the current filter, in listing order.
    pub fn visible_entries(&self) -> Vec<&Entry> {
        let filter = self.filter.text();
        self.entries
            .iter()
            .filter(|entry| matches(&entry.name, &filter))
            .collect()
    }

    /// Matching character spans for highlighting a name.
    pub fn highlights(&self, name: &str) -> Vec<(usize, usize)> {
        match_spans(name, &self.filter.text())
    }

    /// Selects the first visible entry, or clears the selection when none
    /// match.
    pub fn select_first_visible(&mut self) {
        let first = self
            .visible_entries()
            .first()
            .map(|entry| entry.name.clone());
        self.selected = first;
    }

    /// Selects `name` when it is visible, otherwise the first visible entry.
    pub fn select_name(&mut self, name: &str) {
        let visible = self.visible_entries();
        let found = visible
            .iter()
            .any(|entry| entry.name == name)
            .then(|| name.to_string());
        drop(visible);
        self.selected = found;
        self.clamp_selection();
    }

    /// Keeps the selection on a visible entry, selecting the first match when
    /// the previous selection was filtered out.
    pub fn clamp_selection(&mut self) {
        let visible = self.visible_entries();
        if visible.is_empty() {
            self.selected = None;
            return;
        }
        let keep = self
            .selected
            .as_deref()
            .is_some_and(|name| visible.iter().any(|entry| entry.name == name));
        let first = visible.first().map(|entry| entry.name.clone());
        drop(visible);
        if !keep {
            self.selected = first;
        }
    }

    /// Moves the selection through the visible list by `delta`, clamping at
    /// both ends.
    pub fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_entries();
        if visible.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|name| visible.iter().position(|entry| entry.name == name));
        let last = visible.len() - 1;
        let target = match current {
            Some(index) if delta < 0 => index.saturating_sub(delta.unsigned_abs()),
            Some(index) => index.saturating_add(delta as usize).min(last),
            None => 0,
        };
        let chosen = visible.get(target).map(|entry| entry.name.clone());
        drop(visible);
        self.selected = chosen;
    }

    /// Appends a character to the filter.
    pub fn push_filter(&mut self, ch: char) {
        self.filter.insert(ch);
        self.clamp_selection();
    }

    /// Removes the last filter character.
    pub fn backspace_filter(&mut self) {
        self.filter.backspace();
        self.clamp_selection();
    }

    /// Removes the word before the filter cursor.
    pub fn kill_word_filter(&mut self) {
        self.filter.kill_word_backward();
        self.clamp_selection();
    }

    /// Clears the filter and selects the first visible entry.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.clamp_selection();
    }

    /// Reads the current directory again, keeping the selection by name when
    /// possible.
    pub fn refresh(&mut self) -> Result<(), String> {
        self.entries = read_entries(&self.dir)?;
        self.clamp_selection();
        Ok(())
    }

    /// Enters the selected directory, returning `false` when the selection is
    /// not a directory.
    pub fn enter_selected_dir(&mut self) -> Result<bool, String> {
        let Some(entry) = self.selected_entry() else {
            return Ok(false);
        };
        if !entry.is_dir() {
            return Ok(false);
        }
        let path = entry.path.clone();
        let name = entry.name.clone();
        self.change_dir(path.clone())?;
        self.child_memory.insert(path, name);
        self.select_first_visible();
        Ok(true)
    }

    /// Moves to the parent directory, restoring the child entry as the
    /// selection. Returns `false` at the filesystem root.
    pub fn go_parent(&mut self) -> Result<bool, String> {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return Ok(false);
        };
        if parent == self.dir {
            return Ok(false);
        }
        let child = self.dir.clone();
        self.change_dir(parent)?;
        let wanted = self.child_memory.get(&child).cloned().or_else(|| {
            child
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        match wanted {
            Some(name) => self.select_name(&name),
            None => self.select_first_visible(),
        }
        Ok(true)
    }

    /// The selected file when it starts with `MThd`; `None` otherwise.
    pub fn selected_midi(&self) -> Option<PathBuf> {
        let entry = self.selected_entry()?;
        if entry.kind != EntryKind::File {
            return None;
        }
        is_midi_file(&entry.path).then(|| entry.path.clone())
    }

    fn change_dir(&mut self, dir: PathBuf) -> Result<(), String> {
        let entries = read_entries(&dir)?;
        std::env::set_current_dir(&dir)
            .map_err(|e| format!("cannot change directory to {}: {e}", dir.display()))?;
        self.dir = dir;
        self.entries = entries;
        self.filter.clear();
        self.selected = None;
        Ok(())
    }
}

fn read_entries(dir: &Path) -> Result<Vec<Entry>, String> {
    let listing =
        fs::read_dir(dir).map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    for entry in listing {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let kind = entry_kind(&path);
        entries.push(Entry { name, path, kind });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

fn entry_kind(path: &Path) -> EntryKind {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return EntryKind::Unreadable;
    };
    if metadata.file_type().is_symlink() {
        return match fs::metadata(path) {
            Ok(target) if target.is_dir() => EntryKind::Directory,
            Ok(_) => EntryKind::File,
            Err(_) => EntryKind::Unreadable,
        };
    }
    if metadata.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

/// Sorts directories first, then by name within each group.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Whether a filter is case-sensitive because it contains an uppercase
/// character.
pub fn is_case_sensitive(filter: &str) -> bool {
    filter.chars().any(char::is_uppercase)
}

/// Whether `name` contains `filter`, using the required case rule.
pub fn matches(name: &str, filter: &str) -> bool {
    filter.is_empty() || !match_spans(name, filter).is_empty()
}

/// All non-overlapping character spans where `filter` matches `name`.
pub fn match_spans(name: &str, filter: &str) -> Vec<(usize, usize)> {
    let filter: Vec<char> = filter.chars().collect();
    if filter.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = name.chars().collect();
    let mut spans = Vec::new();
    if is_case_sensitive(&filter.iter().collect::<String>()) {
        let mut index = 0;
        while index + filter.len() <= chars.len() {
            if chars[index..index + filter.len()] == filter[..] {
                spans.push((index, index + filter.len()));
                index += filter.len();
            } else {
                index += 1;
            }
        }
        return spans;
    }

    let pattern: Vec<char> = filter.iter().flat_map(|c| c.to_lowercase()).collect();
    let folded: Vec<Vec<char>> = chars.iter().map(|c| c.to_lowercase().collect()).collect();
    let mut index = 0;
    while index < chars.len() {
        let mut pattern_index = 0;
        let mut char_index = index;
        let mut fold_index = 0;
        while pattern_index < pattern.len() {
            if char_index >= chars.len() {
                break;
            }
            if fold_index >= folded[char_index].len() {
                char_index += 1;
                fold_index = 0;
                continue;
            }
            if folded[char_index][fold_index] == pattern[pattern_index] {
                pattern_index += 1;
                fold_index += 1;
            } else {
                break;
            }
        }
        if pattern_index == pattern.len() {
            spans.push((index, char_index + 1));
            index = char_index + 1;
        } else {
            index += 1;
        }
    }
    spans
}

/// Whether `path` starts with the exact four-byte MIDI header `MThd`.
pub fn is_midi_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 4];
    file.read_exact(&mut header).is_ok() && &header == b"MThd"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{cwd_guard, cwd_lock};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mtrak-browser-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            name
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from(name),
            kind,
        }
    }

    #[test]
    fn entries_sort_directories_first_then_by_name() {
        let mut entries = vec![
            entry("z.mid", EntryKind::File),
            entry("a", EntryKind::Directory),
            entry("A.mid", EntryKind::File),
            entry(".hidden", EntryKind::Directory),
            entry("b.mid", EntryKind::File),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec![".hidden", "a", "A.mid", "b.mid", "z.mid"]);
        assert_eq!(entries[0].label(), ".hidden/");
        assert_eq!(entries[2].label(), "A.mid");
    }

    #[test]
    fn listing_includes_hidden_entries_and_directories() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("listing");
        fs::write(dir.join("song.mid"), b"MThd").unwrap();
        fs::write(dir.join(".hidden.mid"), b"").unwrap();
        fs::create_dir(dir.join("samples")).unwrap();
        let browser = Browser::open(dir.clone()).unwrap();
        let names: Vec<&str> = browser
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["samples", ".hidden.mid", "song.mid"]);
        assert!(browser.entries()[0].is_dir());
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filtering_matches_files_and_directories_with_the_case_rule() {
        assert!(matches("Song.mid", "song"));
        assert!(matches("Song.mid", "Song"));
        assert!(!matches("song.mid", "Song"));
        assert!(matches("track-01.mid", "01"));
        assert!(matches("take_2.mid", "_2"));
        assert!(matches("abc", ""), "an empty filter shows every entry");
        assert!(!matches("abc", "A"));
        assert!(matches("ABC", "A"));
        assert!(!is_case_sensitive("123-."));
        assert!(is_case_sensitive("12A"));
    }

    #[test]
    fn match_spans_cover_repeated_and_unicode_matches() {
        assert_eq!(match_spans("banana", "ana"), vec![(1, 4)]);
        assert_eq!(match_spans("a-a-a", "a"), vec![(0, 1), (2, 3), (4, 5)]);
        assert_eq!(match_spans("aaaa", "aa"), vec![(0, 2), (2, 4)]);
        assert_eq!(match_spans("Ärger", "ärger"), vec![(0, 5)]);
        assert_eq!(match_spans("xÄyÄ", "ä"), vec![(1, 2), (3, 4)]);
        assert_eq!(match_spans("song.mid", "SONG"), Vec::new());
        assert_eq!(match_spans("song.mid", "Song"), Vec::new());
    }

    #[test]
    fn filter_editing_backspace_and_word_kill_update_the_selection() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("filter-edit");
        for name in ["alpha.mid", "beta.mid", "alphabet.mid"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        let mut browser = Browser::open(dir.clone()).unwrap();
        browser.select_first_visible();
        for ch in "alpha".chars() {
            browser.push_filter(ch);
        }
        assert_eq!(browser.filter(), "alpha");
        assert_eq!(browser.selected(), Some("alpha.mid"));
        browser.move_selection(1);
        assert_eq!(browser.selected(), Some("alphabet.mid"));
        browser.backspace_filter();
        assert_eq!(browser.filter(), "alph");
        assert!(browser.selected().is_some());
        browser.kill_word_filter();
        assert_eq!(browser.filter(), "");
        assert_eq!(browser.selected(), Some("alphabet.mid"));
        browser.push_filter('b');
        assert_eq!(browser.selected(), Some("alphabet.mid"));
        browser.clear_filter();
        assert_eq!(browser.selected(), Some("alphabet.mid"));
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selection_clamps_and_empty_or_unmatched_lists_have_no_selection() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("selection");
        let mut browser = Browser::open(dir.clone()).unwrap();
        browser.select_first_visible();
        assert_eq!(browser.selected(), None);
        browser.move_selection(1);
        browser.move_selection(-1);
        assert_eq!(browser.selected(), None);
        drop(browser);

        for name in ["a", "b", "c"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        let mut browser = Browser::open(dir.clone()).unwrap();
        browser.select_first_visible();
        browser.move_selection(-1);
        assert_eq!(browser.selected(), Some("a"));
        browser.move_selection(100);
        assert_eq!(browser.selected(), Some("c"));
        browser.push_filter('z');
        assert_eq!(browser.selected(), None);
        assert!(browser.visible_entries().is_empty());
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn navigation_enters_directories_and_returns_to_the_child_entry() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("navigation");
        fs::create_dir(dir.join("alpha")).unwrap();
        fs::write(dir.join("alpha").join("inner.mid"), b"").unwrap();
        fs::create_dir(dir.join("beta")).unwrap();
        fs::write(dir.join("zeta.mid"), b"").unwrap();

        let mut browser = Browser::open(dir.clone()).unwrap();
        browser.select_first_visible();
        assert_eq!(browser.selected(), Some("alpha"));
        assert!(browser.enter_selected_dir().unwrap());
        assert_eq!(browser.directory(), dir.join("alpha"));
        assert_eq!(std::env::current_dir().unwrap(), dir.join("alpha"));
        assert_eq!(browser.selected(), Some("inner.mid"));
        assert!(browser.filter_is_empty());

        assert!(browser.go_parent().unwrap());
        assert_eq!(browser.directory(), dir);
        assert_eq!(browser.selected(), Some("alpha"));

        browser.move_selection(1);
        assert_eq!(browser.selected(), Some("beta"));
        assert!(browser.enter_selected_dir().unwrap());
        assert!(browser.go_parent().unwrap());
        assert_eq!(browser.selected(), Some("beta"));

        browser.move_selection(-1);
        assert_eq!(browser.selected(), Some("alpha"));
        assert!(browser.enter_selected_dir().unwrap());
        assert!(browser.go_parent().unwrap());
        assert_eq!(browser.selected(), Some("alpha"));
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entering_a_directory_clears_the_filter() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("clear-filter");
        fs::create_dir(dir.join("samples")).unwrap();
        fs::write(dir.join("song.mid"), b"").unwrap();
        let mut browser = Browser::open(dir.clone()).unwrap();
        browser.select_first_visible();
        browser.push_filter('s');
        assert_eq!(browser.filter(), "s");
        browser.enter_selected_dir().unwrap();
        assert_eq!(browser.directory(), dir.join("samples"));
        assert!(browser.filter_is_empty());
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn midi_detection_uses_only_the_four_byte_header() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("midi");
        let midi = dir.join("song.mid");
        fs::write(&midi, b"MThd\0\0\0\x06").unwrap();
        assert!(is_midi_file(&midi));
        let other = dir.join("song.txt");
        fs::write(&other, b"RIFF").unwrap();
        assert!(!is_midi_file(&other));
        let short = dir.join("short.mid");
        fs::write(&short, b"MT").unwrap();
        assert!(!is_midi_file(&short));
        assert!(!is_midi_file(&dir.join("missing.mid")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn symlinks_are_classified_safely() {
        let _lock = cwd_lock();
        let _guard = cwd_guard();
        let dir = fixture("symlinks");
        fs::create_dir(dir.join("real-dir")).unwrap();
        fs::write(dir.join("real-file.mid"), b"MThd").unwrap();
        symlink(dir.join("real-dir"), dir.join("dir-link")).unwrap();
        symlink(dir.join("real-file.mid"), dir.join("file-link.mid")).unwrap();
        symlink(dir.join("missing"), dir.join("broken-link")).unwrap();

        let browser = Browser::open(dir.clone()).unwrap();
        let kind = |name: &str| {
            browser
                .entries()
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.kind)
                .unwrap_or(EntryKind::Unreadable)
        };
        assert_eq!(kind("dir-link"), EntryKind::Directory);
        assert_eq!(kind("file-link.mid"), EntryKind::File);
        assert_eq!(kind("broken-link"), EntryKind::Unreadable);
        drop(browser);
        let _ = fs::remove_dir_all(&dir);
    }
}
