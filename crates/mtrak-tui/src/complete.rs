//! Path completion for the command line.
//!
//! `:r`/`:read` and `:w`/`:write` take a file name. Pressing Tab completes the
//! path argument against the filesystem: a unique match is filled in, a
//! directory gets a trailing `/`, and ambiguous prefixes are extended to their
//! longest common prefix. The caller also gets the full candidate list so it
//! can offer a menu instead of leaving the user guessing.

use std::path::Path;

/// A completed command line and the cursor position that goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The whole command line after completion.
    pub text: String,
    /// The cursor position in characters after completion.
    pub cursor: usize,
}

/// A path argument waiting for the user to pick one of its completions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathCandidates {
    /// Character index where the path argument starts.
    pub start: usize,
    /// Character index one past the end of the path argument.
    pub end: usize,
    /// The part of the token before the cursor that was completed.
    pub fragment: String,
    /// Full replacement values, sorted and deduplicated.
    pub values: Vec<String>,
}

/// Completes the path argument of a `:r`/`:w` command line at `cursor`.
///
/// Returns `None` when the cursor is not on a path argument, the command does
/// not take one, or the completion would not change the text.
pub fn complete_command_line(text: &str, cursor: usize) -> Option<Completion> {
    let found = path_candidates(text, cursor)?;
    if found.values.is_empty() {
        return None;
    }
    let completed = if found.values.len() == 1 {
        found.values[0].clone()
    } else {
        shared_prefix(&found.values)
    };
    if completed == found.fragment {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let start = found.start.min(chars.len());
    let end = found.end.min(chars.len()).max(start);
    let mut out: Vec<char> = chars[..start].to_vec();
    out.extend(completed.chars());
    out.extend(chars[end..].iter());
    Some(Completion {
        text: out.into_iter().collect(),
        cursor: start + completed.chars().count(),
    })
}

/// The path token at `cursor` and every completion for it.
pub fn path_candidates(text: &str, cursor: usize) -> Option<PathCandidates> {
    path_candidates_with(text, cursor, crate::path::home_dir().as_deref())
}

fn path_candidates_with(text: &str, cursor: usize, home: Option<&Path>) -> Option<PathCandidates> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());

    let first_start = chars.iter().position(|c| !c.is_whitespace())?;
    let first_end = word_end(&chars, first_start);
    if cursor <= first_end {
        return None;
    }
    let name: String = chars[first_start..first_end].iter().collect();
    if !is_path_command(&name) {
        return None;
    }

    // The path is the whole rest of the line: these commands take no other
    // arguments and file names may contain spaces.
    let start = match chars[first_end..].iter().position(|c| !c.is_whitespace()) {
        Some(offset) => first_end + offset,
        None => chars.len(),
    };
    if cursor < start {
        return None;
    }
    let end = chars.len();
    let fragment: String = chars[start..cursor].iter().collect();
    let values = list_path_with(&fragment, home)?;
    Some(PathCandidates {
        start,
        end,
        fragment,
        values,
    })
}

/// Whether the command name takes a file name.
fn is_path_command(name: &str) -> bool {
    matches!(
        name.trim_start_matches(':').to_ascii_lowercase().as_str(),
        "r" | "read" | "w" | "write"
    )
}

/// The index of the first whitespace at or after `start`.
fn word_end(chars: &[char], start: usize) -> usize {
    chars[start.min(chars.len())..]
        .iter()
        .position(|c| c.is_whitespace())
        .map_or(chars.len(), |offset| start + offset)
}

/// The sorted completions for `fragment`.
fn list_path_with(fragment: &str, home: Option<&Path>) -> Option<Vec<String>> {
    if fragment == "~" {
        // Offer the home directory itself as `~/`; its contents complete on
        // the next Tab.
        return home.map(|_| vec!["~/".to_string()]);
    }
    let (dir, prefix) = match fragment.rfind('/') {
        Some(index) => fragment.split_at(index + 1),
        None => ("", fragment),
    };
    // Read through the expanded path but keep the typed `~/` in the results.
    let read_dir = if dir.is_empty() {
        std::path::PathBuf::from(".")
    } else {
        crate::path::expand_tilde_with(dir, home)
    };
    let show_hidden = prefix.starts_with('.');

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&read_dir).ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') && !show_hidden {
            continue;
        }
        if !name.starts_with(prefix) {
            continue;
        }
        let mut candidate = format!("{dir}{name}");
        // `file_type` uses the directory entry's type when the filesystem
        // reports it, avoiding one stat per entry in huge directories. Only
        // symlinks need to be followed.
        let is_dir = match entry.file_type() {
            Ok(file_type) if file_type.is_symlink() => entry.path().is_dir(),
            Ok(file_type) => file_type.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            candidate.push('/');
        }
        candidates.push(candidate);
    }
    candidates.sort();
    candidates.dedup();
    Some(candidates)
}

/// The longest common prefix of `values`.
pub fn shared_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for value in &values[1..] {
        let chars: Vec<char> = value.chars().collect();
        let shared = prefix
            .iter()
            .zip(&chars)
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(shared);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh fixture directory with a known set of entries.
    fn fixture() -> String {
        let dir = std::env::temp_dir().join(format!(
            "mtrak-complete-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["notes.mid", "noise.mid", "sonic.mid", "song.mid"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::write(dir.join(".hidden.mid"), b"").unwrap();
        std::fs::write(dir.join("with space.mid"), b"").unwrap();
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        std::fs::write(dir.join("samples").join("kick.wav"), b"").unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn complete(text: &str) -> Option<String> {
        complete_command_line(text, text.chars().count()).map(|done| done.text)
    }

    fn candidates(text: &str) -> Option<PathCandidates> {
        path_candidates(text, text.chars().count())
    }

    #[test]
    fn a_unique_match_is_filled_in() {
        let dir = fixture();
        assert_eq!(
            complete(&format!("r {dir}/not")),
            Some(format!("r {dir}/notes.mid"))
        );
        assert_eq!(
            complete(&format!(":w {dir}/noi")),
            Some(format!(":w {dir}/noise.mid")),
            "a leading colon is accepted"
        );
    }

    #[test]
    fn directories_get_a_trailing_slash() {
        let dir = fixture();
        assert_eq!(
            complete(&format!("r {dir}/samp")),
            Some(format!("r {dir}/samples/"))
        );
        // Completing inside that directory works as well.
        assert_eq!(
            complete(&format!("r {dir}/samples/ki")),
            Some(format!("r {dir}/samples/kick.wav"))
        );
    }

    #[test]
    fn a_trailing_slash_lists_the_directory_contents() {
        let dir = fixture();
        let found = candidates(&format!("r {dir}/")).unwrap();
        assert_eq!(found.fragment, format!("{dir}/"));
        let leaves: Vec<&str> = found
            .values
            .iter()
            .map(|value| {
                value
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(value)
            })
            .collect();
        assert_eq!(
            leaves,
            vec![
                "noise.mid",
                "notes.mid",
                "samples",
                "song.mid",
                "sonic.mid",
                "with space.mid"
            ],
            "hidden entries stay hidden without a dot prefix"
        );
        assert!(found.values.iter().any(|value| value.ends_with("samples/")));
        // The common prefix is the directory itself, so nothing is filled in.
        assert_eq!(complete(&format!("r {dir}/")), None);
    }

    #[test]
    fn ambiguous_prefixes_extend_to_the_common_part_only() {
        let dir = fixture();
        assert_eq!(
            complete(&format!("w {dir}/so")),
            Some(format!("w {dir}/son")),
            "sonic/song share 'son'"
        );
        let found = candidates(&format!("w {dir}/so")).unwrap();
        assert_eq!(found.values.len(), 2);
    }

    #[test]
    fn missing_and_unchanged_matches_are_not_completions() {
        let dir = fixture();
        assert_eq!(complete(&format!("r {dir}/zzz")), None);
        // Already complete, so there is nothing to add.
        assert_eq!(complete(&format!("r {dir}/notes.mid")), None);
        assert!(candidates(&format!("r {dir}/zzz"))
            .unwrap()
            .values
            .is_empty());
    }

    #[test]
    fn hidden_entries_need_a_dot_prefix() {
        let dir = fixture();
        assert_eq!(
            complete(&format!("r {dir}/.hid")),
            Some(format!("r {dir}/.hidden.mid"))
        );
        assert_eq!(complete(&format!("r {dir}/hi")), None);
    }

    #[test]
    fn names_with_spaces_complete() {
        let dir = fixture();
        assert_eq!(
            complete(&format!("r {dir}/with")),
            Some(format!("r {dir}/with space.mid")),
        );
    }

    #[test]
    fn tilde_paths_complete_against_the_home_directory() {
        let home = fixture();
        let home = Path::new(&home);
        let candidates =
            |text: &str| path_candidates_with(text, text.chars().count(), Some(home)).unwrap();
        assert_eq!(
            candidates("r ~/not").values,
            vec!["~/notes.mid".to_string()]
        );
        assert_eq!(
            candidates("r ~/samp").values,
            vec!["~/samples/".to_string()]
        );
        assert_eq!(
            candidates("r ~/samples/ki").values,
            vec!["~/samples/kick.wav".to_string()]
        );
        assert_eq!(
            candidates("r ~").values,
            vec!["~/".to_string()],
            "a bare tilde offers the home directory"
        );
        assert!(candidates("r ~/").values.len() > 1);
        assert!(
            path_candidates_with("r ~", 3, None).is_none(),
            "without a home directory a tilde is an ordinary name"
        );
    }

    #[test]
    fn only_path_commands_complete() {
        let dir = fixture();
        assert_eq!(complete(&format!("bpm {dir}/not")), None);
        assert_eq!(complete(&format!("event {dir}/not")), None);
        assert_eq!(complete("read"), None);
        assert_eq!(complete("r"), None);
        assert_eq!(candidates(&format!("bpm {dir}/not")), None);
    }

    #[test]
    fn the_cursor_position_is_preserved_after_the_path() {
        let dir = fixture();
        let text = format!("r {dir}/not");
        let done = complete_command_line(&text, text.chars().count()).unwrap();
        assert_eq!(done.text, format!("r {dir}/notes.mid"));
        assert_eq!(done.cursor, done.text.chars().count());

        // Completing with the cursor inside the path replaces the whole path
        // argument, because file names may contain spaces.
        let text = format!("r {dir}/not");
        let cursor = "r ".chars().count() + dir.chars().count() + 4;
        let done = complete_command_line(&text, cursor).unwrap();
        assert_eq!(done.text, format!("r {dir}/notes.mid"));
        assert_eq!(done.cursor, done.text.chars().count());
    }
}
