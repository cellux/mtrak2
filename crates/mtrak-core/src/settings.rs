//! Scales, note entry, and composition-global settings.
//!
//! Settings are a single concrete layer; the pattern-local inheritance layer
//! was removed by the remaining-gaps milestone, so there is nothing to resolve
//! and no `Setting` wrapper.

use std::fmt;

/// Number of pitch classes in an octave.
pub const PITCH_CLASSES: usize = 12;

/// A 12-bit pitch-class set.
///
/// Bit `n` is set when pitch class `n` (0 = C) belongs to the scale. The text
/// form lists 12 characters from pitch class 11 down to 0, so `"100000000000"`
/// is a single-note "scale" on C.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Scale {
    mask: u16,
}

impl Scale {
    /// Creates a scale from a 12-bit mask. The mask must be non-zero.
    pub const fn from_mask(mask: u16) -> Scale {
        Scale {
            mask: mask & 0x0FFF,
        }
    }

    /// The raw 12-bit mask.
    pub const fn mask(self) -> u16 {
        self.mask & 0x0FFF
    }

    /// Whether pitch class `pc` (`0..=11`) belongs to the scale.
    pub fn contains(self, pc: u8) -> bool {
        pc < PITCH_CLASSES as u8 && self.mask & (1 << pc) != 0
    }

    /// Number of pitch classes in the scale.
    pub fn len(self) -> usize {
        (self.mask & 0x0FFF).count_ones() as usize
    }

    /// Whether the scale contains no pitch classes. Scales are constructed
    /// from non-zero masks, so this is always `false`; it exists to satisfy the
    /// convention that a `len` method is paired with an `is_empty` method.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The pitch classes of the scale, ascending from 0.
    pub fn pitch_classes(self) -> Vec<u8> {
        (0..PITCH_CLASSES as u8)
            .filter(|pc| self.contains(*pc))
            .collect()
    }

    /// Parses the 12-character text form (pitch class 11 first).
    pub fn parse_mask(text: &str) -> Option<Scale> {
        let trimmed = text.trim();
        if trimmed.len() != PITCH_CLASSES {
            return None;
        }
        let mut mask = 0u16;
        for (i, ch) in trimmed.chars().enumerate() {
            let pc = (PITCH_CLASSES - 1 - i) as u16;
            match ch {
                '1' => mask |= 1 << pc,
                '0' => {}
                _ => return None,
            }
        }
        if mask == 0 {
            return None;
        }
        Some(Scale::from_mask(mask))
    }

    /// Formats the scale as the 12-character text form.
    pub fn format_mask(self) -> String {
        (0..PITCH_CLASSES)
            .rev()
            .map(|pc| if self.contains(pc as u8) { '1' } else { '0' })
            .collect()
    }

    /// The scale intervals relative to pitch class 0, ascending.
    pub fn intervals(self) -> Vec<u8> {
        self.pitch_classes()
    }

    /// Rotates the scale to start on its `degree`-th tone.
    ///
    /// This models the "modal degree": degree 1 of the major scale becomes the
    /// Dorian mode, and so on. Returns intervals relative to the mode's tonic.
    pub fn mode_intervals(self, degree: u8) -> Vec<u8> {
        let pcs = self.pitch_classes();
        let k = pcs.len();
        if k == 0 {
            return Vec::new();
        }
        let d = (degree as usize) % k;
        (0..k)
            .map(|j| {
                let idx = d + j;
                (pcs[idx % k] as i32 + 12 * (idx / k) as i32 - pcs[d] as i32) as u8
            })
            .collect()
    }
}

impl fmt::Debug for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scale({})", self.format_mask())
    }
}

/// A named scale definition (built-in or user).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScaleDef {
    /// Canonical name.
    pub name: &'static str,
    /// Accepted aliases (including the canonical name's short forms).
    pub aliases: &'static [&'static str],
    /// The pitch-class mask.
    pub mask: u16,
}

/// The built-in scales and their aliases (see `PLAN.md` §2.10).
pub const BUILTIN_SCALES: &[ScaleDef] = &[
    ScaleDef {
        name: "major",
        aliases: &["M", "major"],
        mask: 0x0AB5,
    },
    ScaleDef {
        name: "natural minor",
        aliases: &["m", "minor", "min", "natural minor"],
        mask: 0x05AD,
    },
    ScaleDef {
        name: "harmonic minor",
        aliases: &["hm", "hmin", "harmonic minor"],
        mask: 0x09AD,
    },
    ScaleDef {
        name: "melodic minor",
        aliases: &["mm", "mmin", "melodic minor"],
        mask: 0x0AAD,
    },
    ScaleDef {
        name: "pentatonic",
        aliases: &["p", "penta", "pentatonic"],
        mask: 0x0295,
    },
    ScaleDef {
        name: "whole tone",
        aliases: &["w", "whole", "whole tone"],
        mask: 0x0555,
    },
    ScaleDef {
        name: "octatonic",
        aliases: &["o", "octa", "octatonic"],
        mask: 0x0B6D,
    },
];

/// Looks up a built-in scale by name or alias.
///
/// Single-letter aliases are case-sensitive (`M` is major, `m` is minor); all
/// other names fall back to a case-insensitive comparison.
pub fn lookup_builtin(name: &str) -> Option<Scale> {
    for def in BUILTIN_SCALES {
        if def.aliases.contains(&name) {
            return Some(Scale::from_mask(def.mask));
        }
    }
    let lower = name.to_ascii_lowercase();
    for def in BUILTIN_SCALES {
        if def.name == lower || def.aliases.iter().any(|a| a.to_ascii_lowercase() == lower) {
            return Some(Scale::from_mask(def.mask));
        }
    }
    None
}

/// A registry of user-defined scales layered over the built-ins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScaleRegistry {
    user: Vec<(String, Scale)>,
}

impl ScaleRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        ScaleRegistry::default()
    }

    /// Adds or replaces a user scale. Name comparison is case-insensitive.
    pub fn add(&mut self, name: &str, scale: Scale) {
        if let Some(entry) = self
            .user
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            entry.1 = scale;
        } else {
            self.user.push((name.to_string(), scale));
        }
    }

    /// Removes a user scale, returning whether one was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.user.len();
        self.user.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.user.len() != before
    }

    /// Resolves a scale by name, preferring user scales.
    pub fn get(&self, name: &str) -> Option<Scale> {
        self.user
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, s)| *s)
            .or_else(|| lookup_builtin(name))
    }

    /// Iterates over user scales as `(name, scale)`.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Scale)> {
        self.user.iter().map(|(n, s)| (n.as_str(), *s))
    }
}

/// Composition-global settings used by note entry, display, and macros.
///
/// The remaining gaps removed the pattern-local settings layer, so a single
/// concrete value now describes the whole composition (no inheritance).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// Root note as a MIDI note number.
    pub root: u8,
    /// Selected scale.
    pub scale: Scale,
    /// Modal degree within the scale.
    pub modal_degree: u8,
    /// Whether note entry is scale-aware.
    pub scale_aware: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::new()
    }
}

impl Settings {
    /// The defaults for a new project (see `PLAN.md` §2.10).
    pub fn new() -> Self {
        Settings {
            root: 60,
            scale: Scale::from_mask(0x0AB5),
            modal_degree: 0,
            scale_aware: true,
        }
    }

    /// Converts a note-entry degree to a MIDI note, clamped to `0..=127`.
    ///
    /// In scale-aware mode the degree indexes the (modal) scale; in chromatic
    /// mode it is added directly to the root.
    pub fn degree_to_note(&self, degree: u8) -> u8 {
        let offset = if self.scale_aware {
            let intervals = self.scale.mode_intervals(self.modal_degree);
            if intervals.is_empty() {
                return clamp_note(self.root as i32 + degree as i32);
            }
            let k = intervals.len();
            let octave = (degree as usize) / k;
            let idx = (degree as usize) % k;
            (octave * 12 + intervals[idx] as usize) as i32
        } else {
            degree as i32
        };
        clamp_note(self.root as i32 + offset)
    }
}

/// A fully resolved settings view. Retained as an alias because settings are
/// global-only now; there is no separate local layer to resolve.
pub type EffectiveSettings = Settings;

fn clamp_note(note: i32) -> u8 {
    note.clamp(0, 127) as u8
}

/// Parses a note name such as `C-4` or `C#4` into a MIDI note number.
///
/// The middle character is `-` for a natural, `#`/`s` for a sharp, or `b` for
/// a flat. The octave is a single digit. The result is clamped to `0..=127`.
pub fn parse_note(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    if bytes.len() != 3 {
        return None;
    }
    let base: i32 = match bytes[0].to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    let accidental = match bytes[1] {
        b'-' | b'n' | b'N' => 0,
        b'#' | b's' | b'S' => 1,
        b'b' | b'f' | b'F' => -1,
        _ => return None,
    };
    let octave = (bytes[2] as char).to_digit(10)? as i32;
    let note = (octave + 1) * 12 + base + accidental;
    Some(clamp_note(note))
}

/// Formats a MIDI note number as a name such as `C-4` or `C#4`.
pub fn format_note(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C-", "C#", "D-", "D#", "E-", "F-", "F#", "G-", "G#", "A-", "A#", "B-",
    ];
    let note = note as usize;
    format!("{}{}", NAMES[note % 12], note / 12 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_masks_match_expected_pitch_classes() {
        let major = lookup_builtin("major").unwrap();
        assert_eq!(major.pitch_classes(), vec![0, 2, 4, 5, 7, 9, 11]);
        let minor = lookup_builtin("m").unwrap();
        assert_eq!(minor.pitch_classes(), vec![0, 2, 3, 5, 7, 8, 10]);
        assert_eq!(
            lookup_builtin("harmonic minor").unwrap().pitch_classes(),
            vec![0, 2, 3, 5, 7, 8, 11]
        );
        assert_eq!(
            lookup_builtin("melodic minor").unwrap().pitch_classes(),
            vec![0, 2, 3, 5, 7, 9, 11]
        );
        assert_eq!(
            lookup_builtin("penta").unwrap().pitch_classes(),
            vec![0, 2, 4, 7, 9]
        );
        assert_eq!(
            lookup_builtin("w").unwrap().pitch_classes(),
            vec![0, 2, 4, 6, 8, 10]
        );
        assert_eq!(
            lookup_builtin("octa").unwrap().pitch_classes(),
            vec![0, 2, 3, 5, 6, 8, 9, 11]
        );
    }

    #[test]
    fn aliases_are_case_sensitive_for_single_letters_but_not_words() {
        assert_eq!(lookup_builtin("M"), lookup_builtin("major"));
        assert_eq!(lookup_builtin("m"), lookup_builtin("minor"));
        assert_ne!(lookup_builtin("M"), lookup_builtin("m"));
        assert_eq!(lookup_builtin("NATURAL MINOR"), lookup_builtin("minor"));
    }

    #[test]
    fn mask_text_round_trips() {
        let scale = lookup_builtin("major").unwrap();
        assert_eq!(scale.format_mask(), "101010110101");
        assert_eq!(Scale::parse_mask("101010110101"), Some(scale));
        assert_eq!(Scale::parse_mask("000000000000"), None);
        assert_eq!(Scale::parse_mask("1"), None);
        assert_eq!(Scale::parse_mask("2".repeat(12).as_str()), None);
    }

    #[test]
    fn mode_intervals_rotate_the_scale() {
        let major = lookup_builtin("major").unwrap();
        assert_eq!(major.mode_intervals(0), vec![0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(major.mode_intervals(1), vec![0, 2, 3, 5, 7, 9, 10]); // Dorian
        assert_eq!(major.mode_intervals(6), vec![0, 1, 3, 5, 6, 8, 10]); // Locrian
    }

    #[test]
    fn degree_to_note_in_scale_and_chromatic_modes() {
        let settings = Settings::new();
        // C major, scale-aware: degrees 0..7 -> C D E F G A B C
        assert_eq!(settings.degree_to_note(0), 60);
        assert_eq!(settings.degree_to_note(1), 62);
        assert_eq!(settings.degree_to_note(2), 64);
        assert_eq!(settings.degree_to_note(3), 65);
        assert_eq!(settings.degree_to_note(6), 71);
        assert_eq!(settings.degree_to_note(7), 72);

        let chromatic = Settings {
            scale_aware: false,
            ..Settings::new()
        };
        assert_eq!(chromatic.degree_to_note(0), 60);
        assert_eq!(chromatic.degree_to_note(4), 64);
        assert_eq!(chromatic.degree_to_note(127), 127);
    }

    #[test]
    fn degree_to_note_clamps_at_127() {
        let settings = Settings {
            root: 120,
            scale_aware: false,
            ..Settings::new()
        };
        assert_eq!(settings.degree_to_note(100), 127);
    }

    #[test]
    fn settings_are_a_single_concrete_layer() {
        let mut settings = Settings::new();
        assert_eq!(settings.root, 60);
        settings.root = 48;
        assert_eq!(settings.degree_to_note(0), 48);
        settings.modal_degree = 5;
        assert_eq!(settings.modal_degree, 5);
    }

    #[test]
    fn user_scales_take_precedence() {
        let mut registry = ScaleRegistry::new();
        let custom = Scale::parse_mask("101001010100").unwrap();
        registry.add("weird", custom);
        assert_eq!(registry.get("weird"), Some(custom));
        assert_eq!(registry.get("major"), lookup_builtin("major"));
        assert!(registry.remove("weird"));
        assert!(!registry.remove("weird"));
        assert_eq!(registry.get("weird"), None);
    }

    #[test]
    fn note_names_round_trip_and_clamp() {
        assert_eq!(parse_note("C-4"), Some(60));
        assert_eq!(parse_note("C#4"), Some(61));
        assert_eq!(parse_note("A-4"), Some(69));
        assert_eq!(parse_note("G-9"), Some(127));
        assert_eq!(parse_note("bad"), None);
        assert_eq!(parse_note("C-4x"), None);
        assert_eq!(format_note(60), "C-4");
        assert_eq!(format_note(61), "C#4");
    }
}
