//! Path helpers shared by the command layer and Tab completion.

use std::path::{Path, PathBuf};

/// The user's home directory, if the environment provides one.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Expands a leading `~` or `~/` to the user's home directory.
///
/// Other paths, including `~user/...`, are returned unchanged.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with(path, home_dir().as_deref())
}

/// [`expand_tilde`] with an explicit home directory.
pub fn expand_tilde_with(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_tilde_expands_to_the_home_directory() {
        let home = Path::new("/home/user");
        assert_eq!(expand_tilde_with("~", Some(home)), PathBuf::from(home));
        assert_eq!(
            expand_tilde_with("~/song.mid", Some(home)),
            PathBuf::from("/home/user/song.mid")
        );
        assert_eq!(
            expand_tilde_with("~/music/live/", Some(home)),
            PathBuf::from("/home/user/music/live/")
        );
    }

    #[test]
    fn other_paths_are_left_alone() {
        let home = Path::new("/home/user");
        assert_eq!(
            expand_tilde_with("/tmp/song.mid", Some(home)),
            PathBuf::from("/tmp/song.mid")
        );
        assert_eq!(
            expand_tilde_with("songs/song.mid", Some(home)),
            PathBuf::from("songs/song.mid")
        );
        assert_eq!(
            expand_tilde_with("~other/song.mid", Some(home)),
            PathBuf::from("~other/song.mid")
        );
        assert_eq!(
            expand_tilde_with("~/song.mid", None),
            PathBuf::from("~/song.mid"),
            "without a home directory the path is unchanged"
        );
    }
}
