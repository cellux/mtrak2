//! Composition, patterns, tracks, and slots.
//!
//! Pattern IDs are stable slots (`00`–`FF`): deleting a pattern leaves its ID
//! unused rather than renumbering the others (decision D-006).

use crate::event::Event;
use crate::settings::{ScaleRegistry, Settings};
use crate::timing::{self, Bpm, Ticks, TimingError};

/// Number of stable pattern ID slots.
pub const MAX_PATTERNS: usize = 256;
/// Default number of rows in a new pattern.
pub const DEFAULT_ROWS: usize = 64;
/// Default number of tracks in a new pattern.
pub const DEFAULT_TRACKS: usize = 16;
/// Largest permitted row count.
pub const MAX_ROWS: usize = 4096;
/// Largest permitted track count.
pub const MAX_TRACKS: usize = 256;
/// Smallest permitted row count.
pub const MIN_ROWS: usize = 1;
/// Smallest permitted track count.
pub const MIN_TRACKS: usize = 1;

/// A stable pattern identifier (`00`–`FF`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PatternId(pub u8);

impl PatternId {
    /// The numeric value of the ID.
    pub fn value(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for PatternId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02X}", self.0)
    }
}

impl From<u8> for PatternId {
    fn from(value: u8) -> Self {
        PatternId(value)
    }
}

/// One event cell: a typed event plus an optional intra-row tick offset.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Slot {
    /// The event stored in this cell.
    pub event: Event,
    /// Global-tick offset within the owning row (0 = exactly on the row).
    pub offset: u32,
}

impl Slot {
    /// An empty slot with no offset.
    pub fn empty() -> Self {
        Slot::default()
    }

    /// Whether the slot holds no event (its offset is then meaningless).
    pub fn is_empty(&self) -> bool {
        self.event.is_empty()
    }
}

/// A pattern: a rows × tracks grid sharing one LPB and one timing grid.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pattern {
    id: PatternId,
    name: Option<String>,
    lpb: u32,
    rows: Vec<Vec<Slot>>,
}

impl Pattern {
    /// Creates an empty pattern with the given geometry.
    pub fn new(id: PatternId, lpb: u32, rows: usize, tracks: usize) -> Self {
        let tracks = tracks.max(MIN_TRACKS);
        let rows = rows.max(MIN_ROWS);
        Pattern {
            id,
            name: None,
            lpb,
            rows: vec![vec![Slot::empty(); tracks]; rows],
        }
    }

    /// The stable pattern ID.
    pub fn id(&self) -> PatternId {
        self.id
    }

    /// Returns a copy of this pattern under a different stable ID.
    ///
    /// Used by chooser paste, which preserves the destination ID.
    pub fn with_id(&self, id: PatternId) -> Pattern {
        let mut clone = self.clone();
        clone.id = id;
        clone
    }

    /// The optional pattern name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Sets the pattern name.
    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    /// Lines per beat.
    pub fn lpb(&self) -> u32 {
        self.lpb
    }

    /// Mutable access to lines per beat. The caller must keep TPB divisible.
    pub(crate) fn set_lpb_unchecked(&mut self, lpb: u32) {
        self.lpb = lpb;
    }

    /// The number of rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The number of tracks.
    pub fn track_count(&self) -> usize {
        self.rows.first().map(|r| r.len()).unwrap_or(0)
    }

    /// Immutable access to a slot, if in range.
    pub fn slot(&self, row: usize, track: usize) -> Option<&Slot> {
        self.rows.get(row).and_then(|r| r.get(track))
    }

    /// Mutable access to a slot, if in range.
    pub fn slot_mut(&mut self, row: usize, track: usize) -> Option<&mut Slot> {
        self.rows.get_mut(row).and_then(|r| r.get_mut(track))
    }

    /// Sets the event of a slot, returning whether the coordinates were valid.
    pub fn set_event(&mut self, row: usize, track: usize, event: Event) -> bool {
        match self.slot_mut(row, track) {
            Some(slot) => {
                slot.event = event;
                if slot.event.is_empty() {
                    slot.offset = 0;
                }
                true
            }
            None => false,
        }
    }

    /// Iterates over all slots row by row.
    pub fn iter_slots(&self) -> impl Iterator<Item = (usize, usize, &Slot)> {
        self.rows
            .iter()
            .enumerate()
            .flat_map(|(r, row)| row.iter().enumerate().map(move |(t, s)| (r, t, s)))
    }

    /// Resizes the row count, preserving existing content.
    ///
    /// New rows are appended empty; extra rows are dropped from the end.
    pub fn resize_rows(&mut self, rows: usize) -> Result<(), CompositionError> {
        if !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
            return Err(CompositionError::RowCountOutOfRange(rows));
        }
        let tracks = self.track_count();
        self.rows.resize_with(rows, || vec![Slot::empty(); tracks]);
        Ok(())
    }

    /// Resizes the track count, preserving existing content.
    ///
    /// New tracks are appended empty in every row; extra tracks are dropped
    /// from the end of every row.
    pub fn resize_tracks(&mut self, tracks: usize) -> Result<(), CompositionError> {
        if !(MIN_TRACKS..=MAX_TRACKS).contains(&tracks) {
            return Err(CompositionError::TrackCountOutOfRange(tracks));
        }
        for row in &mut self.rows {
            row.resize_with(tracks, Slot::empty);
        }
        Ok(())
    }

    /// Multiplies every non-zero slot offset by `factor`.
    pub(crate) fn rescale_offsets(&mut self, factor: u32) {
        for slot in self.rows.iter_mut().flatten() {
            if slot.offset != 0 {
                slot.offset *= factor;
            }
        }
    }

    /// The tick at which `row` begins, given TPB.
    pub fn row_start_tick(&self, row: usize, tpb: u32) -> Option<Ticks> {
        let row_ticks = timing::row_ticks(tpb, self.lpb).ok()?;
        Some(row as u64 * u64::from(row_ticks))
    }

    /// The absolute tick of a slot, given TPB.
    pub fn slot_tick(&self, row: usize, track: usize, tpb: u32) -> Option<Ticks> {
        let start = self.row_start_tick(row, tpb)?;
        let slot = self.slot(row, track)?;
        Some(start + u64::from(slot.offset))
    }
}

/// Errors from composition mutation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositionError {
    /// No free pattern ID remains.
    #[error("no free pattern ID remains")]
    NoFreePatternId,
    /// The requested pattern does not exist.
    #[error("pattern {0} does not exist")]
    NoSuchPattern(PatternId),
    /// Row count outside the permitted range.
    #[error("row count {0} is out of range")]
    RowCountOutOfRange(usize),
    /// Track count outside the permitted range.
    #[error("track count {0} is out of range")]
    TrackCountOutOfRange(usize),
    /// A timing value was invalid.
    #[error(transparent)]
    Timing(#[from] TimingError),
    /// A TPB change would not preserve stored offsets exactly.
    #[error("ticks per beat {0} cannot shrink existing offsets exactly")]
    TpbShrink(u32),
    /// The pattern ID slot is already occupied.
    #[error("pattern slot {0} is already occupied")]
    PatternSlotOccupied(PatternId),
}

/// A whole tracker project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    bpm: Bpm,
    tpb: u32,
    /// Seed for the deterministic random series macros draw from.
    ///
    /// Part of the composition so a render is a pure function of the saved
    /// project: the same seed and the same events always produce the same
    /// audio. Settable with `:seed`.
    seed: u64,
    global_settings: Settings,
    scales: ScaleRegistry,
    patterns: Vec<Option<Pattern>>,
    selected_main: PatternId,
}

impl Composition {
    /// Creates a new project with the `PLAN.md` §2.10 defaults.
    pub fn new() -> Self {
        let mut patterns = Vec::with_capacity(MAX_PATTERNS);
        patterns.resize_with(MAX_PATTERNS, || None);
        let initial = Pattern::new(PatternId(0), DEFAULT_LPB, DEFAULT_ROWS, DEFAULT_TRACKS);
        patterns[0] = Some(initial);
        Composition {
            bpm: Bpm::DEFAULT,
            tpb: DEFAULT_TPB,
            seed: 0,
            global_settings: Settings::new(),
            scales: ScaleRegistry::new(),
            patterns,
            selected_main: PatternId(0),
        }
    }

    /// Creates an empty composition with no patterns, for project loading.
    pub fn empty() -> Self {
        let mut patterns = Vec::with_capacity(MAX_PATTERNS);
        patterns.resize_with(MAX_PATTERNS, || None);
        Composition {
            bpm: Bpm::DEFAULT,
            tpb: DEFAULT_TPB,
            seed: 0,
            global_settings: Settings::new(),
            scales: ScaleRegistry::new(),
            patterns,
            selected_main: PatternId(0),
        }
    }

    /// Places a fully-built pattern at its own stable ID without touching TPB.
    ///
    /// This is a loading-time primitive: the caller is responsible for setting a
    /// TPB that every pattern's LPB divides.
    pub fn place_pattern(&mut self, pattern: Pattern) -> Result<(), CompositionError> {
        let id = pattern.id();
        let slot = self
            .patterns
            .get_mut(id.0 as usize)
            .ok_or(CompositionError::NoSuchPattern(id))?;
        if slot.is_some() {
            return Err(CompositionError::PatternSlotOccupied(id));
        }
        *slot = Some(pattern);
        Ok(())
    }

    /// Sets TPB directly without rescaling, validating every pattern's LPB.
    pub fn set_tpb_raw(&mut self, tpb: u32) -> Result<(), CompositionError> {
        if tpb == 0 {
            return Err(CompositionError::Timing(TimingError::ZeroTpb));
        }
        if tpb > timing::MAX_TPB {
            return Err(CompositionError::Timing(TimingError::TpbTooLarge(tpb)));
        }
        for pattern in self.patterns.iter().flatten() {
            if !timing::is_divisible(tpb, pattern.lpb()) {
                return Err(CompositionError::Timing(TimingError::Indivisible {
                    lpb: pattern.lpb(),
                    tpb,
                }));
            }
        }
        self.tpb = tpb;
        Ok(())
    }

    /// Selects the main pattern without checking that it exists (loading only).
    pub fn set_selected_main_raw(&mut self, id: PatternId) {
        self.selected_main = id;
    }

    /// Composition tempo.
    pub fn bpm(&self) -> Bpm {
        self.bpm
    }

    /// The random seed macros draw from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Sets the random seed macros draw from.
    pub fn set_seed(&mut self, seed: u64) {
        self.seed = seed;
    }

    /// Sets the composition tempo.
    pub fn set_bpm(&mut self, bpm: Bpm) {
        self.bpm = bpm;
    }

    /// Global ticks per beat.
    pub fn tpb(&self) -> u32 {
        self.tpb
    }

    /// The global settings layer.
    pub fn global_settings(&self) -> &Settings {
        &self.global_settings
    }

    /// Mutable access to the global settings layer.
    pub fn global_settings_mut(&mut self) -> &mut Settings {
        &mut self.global_settings
    }

    /// The user scale registry.
    pub fn scales(&self) -> &ScaleRegistry {
        &self.scales
    }

    /// Mutable access to the scale registry.
    pub fn scales_mut(&mut self) -> &mut ScaleRegistry {
        &mut self.scales
    }

    /// The pattern selected as the playback/export root.
    pub fn selected_main(&self) -> PatternId {
        self.selected_main
    }

    /// Selects a new main pattern.
    pub fn select_main(&mut self, id: PatternId) -> Result<(), CompositionError> {
        self.pattern(id)
            .ok_or(CompositionError::NoSuchPattern(id))?;
        self.selected_main = id;
        Ok(())
    }

    /// All pattern IDs that currently exist, ascending.
    pub fn pattern_ids(&self) -> Vec<PatternId> {
        self.patterns
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.as_ref().map(|_| PatternId(i as u8)))
            .collect()
    }

    /// Looks up a pattern.
    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.get(id.0 as usize).and_then(|p| p.as_ref())
    }

    /// Mutable lookup of a pattern.
    pub fn pattern_mut(&mut self, id: PatternId) -> Option<&mut Pattern> {
        self.patterns
            .get_mut(id.0 as usize)
            .and_then(|p| p.as_mut())
    }

    /// Creates a pattern with the given LPB in the lowest free slot.
    ///
    /// If `lpb` does not divide the current TPB, TPB is raised to a common
    /// multiple and all stored offsets are rescaled (decision D-021).
    pub fn create_pattern(&mut self, lpb: u32) -> Result<PatternId, CompositionError> {
        if lpb == 0 {
            return Err(CompositionError::Timing(TimingError::ZeroLpb));
        }
        let index = self
            .patterns
            .iter()
            .position(|p| p.is_none())
            .ok_or(CompositionError::NoFreePatternId)?;
        self.ensure_tpb_for_lpb(lpb)?;
        let id = PatternId(index as u8);
        self.patterns[index] = Some(Pattern::new(id, lpb, DEFAULT_ROWS, DEFAULT_TRACKS));
        Ok(id)
    }

    /// Creates a pattern with the given LPB at a specific stable ID.
    ///
    /// This backs the pattern chooser: selecting an empty cell creates it with
    /// the existing defaults (LPB 4, 64 rows, 16 tracks).
    pub fn create_pattern_at(&mut self, id: PatternId, lpb: u32) -> Result<(), CompositionError> {
        if lpb == 0 {
            return Err(CompositionError::Timing(TimingError::ZeroLpb));
        }
        if self.pattern(id).is_some() {
            return Err(CompositionError::PatternSlotOccupied(id));
        }
        self.ensure_tpb_for_lpb(lpb)?;
        self.patterns[id.0 as usize] = Some(Pattern::new(id, lpb, DEFAULT_ROWS, DEFAULT_TRACKS));
        Ok(())
    }

    /// Places or replaces a fully-built pattern at its own ID.
    ///
    /// Used by chooser paste, which preserves the destination ID.
    pub fn replace_pattern(&mut self, pattern: Pattern) -> Result<(), CompositionError> {
        let id = pattern.id();
        self.ensure_tpb_for_lpb(pattern.lpb())?;
        self.patterns[id.0 as usize] = Some(pattern);
        Ok(())
    }

    /// Deletes a pattern, leaving its ID unused. The main pattern cannot be
    /// deleted.
    pub fn delete_pattern(&mut self, id: PatternId) -> Result<(), CompositionError> {
        if id == self.selected_main {
            return Err(CompositionError::NoSuchPattern(id));
        }
        let slot = self
            .patterns
            .get_mut(id.0 as usize)
            .ok_or(CompositionError::NoSuchPattern(id))?;
        if slot.take().is_none() {
            return Err(CompositionError::NoSuchPattern(id));
        }
        Ok(())
    }

    /// Sets a pattern's LPB, raising TPB if required.
    pub fn set_pattern_lpb(&mut self, id: PatternId, lpb: u32) -> Result<(), CompositionError> {
        if lpb == 0 {
            return Err(CompositionError::Timing(TimingError::ZeroLpb));
        }
        if self.pattern(id).is_none() {
            return Err(CompositionError::NoSuchPattern(id));
        }
        self.ensure_tpb_for_lpb(lpb)?;
        self.pattern_mut(id).unwrap().set_lpb_unchecked(lpb);
        Ok(())
    }

    /// Ensures TPB is a multiple of `lpb`, raising it (and rescaling offsets)
    /// when necessary.
    pub fn ensure_tpb_for_lpb(&mut self, lpb: u32) -> Result<(), CompositionError> {
        let next = timing::resolve_tpb(self.tpb, lpb)?;
        if next != self.tpb {
            self.raise_tpb(next)?;
        }
        Ok(())
    }

    /// Raises TPB to `new_tpb`, which must be a multiple of the current TPB and
    /// of every pattern's LPB.
    ///
    /// Offsets are multiplied by `new_tpb / old_tpb`, which is exact precisely
    /// because the change is an integer increase.
    pub fn raise_tpb(&mut self, new_tpb: u32) -> Result<(), CompositionError> {
        if new_tpb == 0 {
            return Err(CompositionError::Timing(TimingError::ZeroTpb));
        }
        if new_tpb > timing::MAX_TPB {
            return Err(CompositionError::Timing(TimingError::TpbTooLarge(new_tpb)));
        }
        if new_tpb < self.tpb {
            return Err(CompositionError::TpbShrink(new_tpb));
        }
        if new_tpb % self.tpb != 0 {
            return Err(CompositionError::Timing(TimingError::Indivisible {
                lpb: self.tpb,
                tpb: new_tpb,
            }));
        }
        for pattern in self.patterns.iter().flatten() {
            if !timing::is_divisible(new_tpb, pattern.lpb()) {
                return Err(CompositionError::Timing(TimingError::Indivisible {
                    lpb: pattern.lpb(),
                    tpb: new_tpb,
                }));
            }
        }
        let factor = new_tpb / self.tpb;
        for pattern in self.patterns.iter_mut().flatten() {
            pattern.rescale_offsets(factor);
        }
        self.tpb = new_tpb;
        Ok(())
    }

    /// The composition-global settings, for any existing pattern.
    ///
    /// Settings are global-only now, so this ignores `id` except to reject
    /// patterns that do not exist.
    pub fn effective_settings(&self, id: PatternId) -> Option<crate::settings::EffectiveSettings> {
        self.pattern(id).map(|_| self.global_settings)
    }
}

impl Default for Composition {
    fn default() -> Self {
        Composition::new()
    }
}

/// Default LPB for a new project.
pub const DEFAULT_LPB: u32 = 4;
/// Default TPB for a new project.
pub const DEFAULT_TPB: u32 = 24;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ChannelKind;

    #[test]
    fn new_project_matches_spec_defaults() {
        let comp = Composition::new();
        assert_eq!(comp.bpm(), Bpm::from_int(120).unwrap());
        assert_eq!(comp.tpb(), 24);
        assert_eq!(comp.pattern_ids(), vec![PatternId(0)]);
        assert_eq!(comp.selected_main(), PatternId(0));
        let pattern = comp.pattern(PatternId(0)).unwrap();
        assert_eq!(pattern.lpb(), 4);
        assert_eq!(pattern.row_count(), 64);
        assert_eq!(pattern.track_count(), 16);
        assert_eq!(comp.effective_settings(PatternId(0)).unwrap().root, 60);
    }

    #[test]
    fn pattern_ids_are_stable_across_deletion() {
        let mut comp = Composition::new();
        let a = comp.create_pattern(4).unwrap();
        let b = comp.create_pattern(4).unwrap();
        let c = comp.create_pattern(4).unwrap();
        assert_eq!((a.value(), b.value(), c.value()), (1, 2, 3));
        comp.delete_pattern(b).unwrap();
        let d = comp.create_pattern(4).unwrap();
        assert_eq!(d.value(), 2, "the freed slot is reused");
        // Deleting the main pattern is refused.
        assert!(comp.delete_pattern(PatternId(0)).is_err());
        assert!(comp.pattern(PatternId(0)).is_some());
    }

    #[test]
    fn resizing_preserves_content_and_appends_at_the_end() {
        let mut pattern = Pattern::new(PatternId(0), 4, 2, 2);
        pattern.set_event(
            0,
            0,
            Event::Channel(crate::event::ChannelMessage::with_data(
                ChannelKind::NoteOn,
                0,
                60,
                Some(100),
            )),
        );
        pattern.resize_rows(4).unwrap();
        assert_eq!(pattern.row_count(), 4);
        assert!(!pattern.slot(0, 0).unwrap().is_empty());
        assert!(pattern.slot(3, 1).unwrap().is_empty());
        pattern.resize_tracks(3).unwrap();
        assert_eq!(pattern.track_count(), 3);
        assert!(!pattern.slot(0, 0).unwrap().is_empty());
        assert!(pattern.slot(0, 2).unwrap().is_empty());
        pattern.resize_tracks(1).unwrap();
        assert_eq!(pattern.track_count(), 1);
        assert!(!pattern.slot(0, 0).unwrap().is_empty());
        pattern.resize_rows(1).unwrap();
        assert_eq!(pattern.row_count(), 1);
    }

    #[test]
    fn introducing_a_new_lpb_raises_tpb_and_rescales_offsets() {
        let mut comp = Composition::new(); // tpb 24, lpb 4
        comp.pattern_mut(PatternId(0))
            .unwrap()
            .slot_mut(1, 0)
            .unwrap()
            .offset = 3;
        // LPB 5 needs tpb 120.
        let id = comp.create_pattern(5).unwrap();
        assert_eq!(comp.tpb(), 120);
        assert_eq!(comp.pattern(id).unwrap().lpb(), 5);
        // Existing offset 3 (in 24-tpb units) becomes 15 (in 120-tpb units).
        assert_eq!(
            comp.pattern(PatternId(0))
                .unwrap()
                .slot(1, 0)
                .unwrap()
                .offset,
            15
        );
        // Absolute timing is preserved: row 1 starts after one beat either way.
        // Old: 6 ticks of 24/beat; new: 30 ticks of 120/beat, both = 1/4 quarter note.
        assert_eq!(
            comp.pattern(PatternId(0)).unwrap().row_start_tick(1, 120),
            Some(30)
        );
        assert_eq!(
            comp.pattern(PatternId(0)).unwrap().slot_tick(1, 0, 120),
            Some(45)
        );
    }

    #[test]
    fn tpb_cannot_shrink() {
        let mut comp = Composition::new();
        assert_eq!(comp.raise_tpb(12), Err(CompositionError::TpbShrink(12)));
        comp.raise_tpb(48).unwrap();
        assert_eq!(comp.tpb(), 48);
        // Row length doubles, so an empty project still round-trips.
        assert_eq!(
            comp.pattern(PatternId(0)).unwrap().row_start_tick(1, 48),
            Some(12)
        );
    }

    #[test]
    fn tpb_raise_rejects_indivisible_and_overflow() {
        let mut comp = Composition::new();
        assert!(matches!(
            comp.raise_tpb(100),
            Err(CompositionError::Timing(_))
        ));
        assert!(matches!(
            comp.raise_tpb(timing::MAX_TPB + 1),
            Err(CompositionError::Timing(_))
        ));
    }

    #[test]
    fn pattern_lpb_update_reraises_tpb() {
        let mut comp = Composition::new();
        comp.set_pattern_lpb(PatternId(0), 7).unwrap();
        assert_eq!(comp.tpb(), 168);
        assert_eq!(comp.pattern(PatternId(0)).unwrap().lpb(), 7);
    }

    #[test]
    fn selected_main_must_exist() {
        let mut comp = Composition::new();
        assert!(comp.select_main(PatternId(9)).is_err());
        let id = comp.create_pattern(4).unwrap();
        comp.select_main(id).unwrap();
        assert_eq!(comp.selected_main(), id);
    }

    #[test]
    fn settings_are_global_only() {
        let mut comp = Composition::new();
        let id = comp.create_pattern(4).unwrap();
        assert_eq!(comp.effective_settings(id).unwrap().root, 60);
        comp.global_settings_mut().root = 67;
        assert_eq!(comp.effective_settings(id).unwrap().root, 67);
        assert_eq!(comp.effective_settings(PatternId(0)).unwrap().root, 67);
    }

    #[test]
    fn mixed_lpbs_resolve_to_a_common_multiple() {
        let mut comp = Composition::new(); // tpb 24, lpb 4
        comp.create_pattern(3).unwrap();
        comp.create_pattern(5).unwrap();
        comp.create_pattern(7).unwrap();
        assert_eq!(comp.tpb(), 840);
        for id in comp.pattern_ids() {
            assert!(timing::is_divisible(
                comp.tpb(),
                comp.pattern(id).unwrap().lpb()
            ));
        }
    }

    #[test]
    fn unusual_resolutions_stay_exact() {
        for (tpb, lpb, expected) in [
            (96u32, 24u32, 4u32),
            (480, 96, 5),
            (384, 128, 3),
            (960, 120, 8),
        ] {
            assert_eq!(timing::row_ticks(tpb, lpb).unwrap(), expected);
            assert_eq!(timing::resolve_tpb(tpb, lpb).unwrap(), tpb);
        }
        // A resolution that does not yet divide the LPB is raised exactly.
        assert_eq!(timing::resolve_tpb(480, 11).unwrap(), timing::lcm(480, 11));
    }
}
