//! Timing primitives: BPM, ticks per beat, per-pattern line rates, and tempo maps.
//!
//! TPB is the canonical resolution and the single scheduler clock (decision
//! D-009). Every pattern's LPB must divide TPB exactly, so a row is exactly
//! `TPB / LPB` global ticks.

/// A tick on the global TPB timeline.
pub type Ticks = u64;

/// The largest TPB representable in a Standard MIDI File division (15 bits).
pub const MAX_TPB: u32 = 0x7FFF;

/// Tempo expressed as unsigned Q8.8 beats per minute.
///
/// The integer part occupies the high byte and the fraction the low byte, which
/// matches the built-in tempo macro's two argument bytes (decision D-011).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bpm(u32);

impl Bpm {
    /// The default project tempo (120 BPM).
    pub const DEFAULT: Bpm = Bpm(120 << 8);

    /// The largest permitted tempo (1000 BPM).
    pub const MAX: Bpm = Bpm(1000 << 8);

    /// Creates a tempo from an integer BPM value, if non-zero and in range.
    pub fn from_int(bpm: u32) -> Option<Bpm> {
        let value = bpm.checked_mul(256)?;
        Bpm::from_q8_8(value)
    }

    /// Creates a tempo from a raw Q8.8 value.
    pub fn from_q8_8(value: u32) -> Option<Bpm> {
        if value == 0 || value > Bpm::MAX.0 {
            return None;
        }
        Some(Bpm(value))
    }

    /// Creates a tempo from the two macro argument bytes (integer, fraction).
    pub fn from_macro_args(integer: u8, fraction: u8) -> Option<Bpm> {
        Bpm::from_q8_8((u32::from(integer) << 8) | u32::from(fraction))
    }

    /// The raw Q8.8 value.
    pub fn q8_8(self) -> u32 {
        self.0
    }

    /// The tempo as a floating-point BPM.
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 256.0
    }

    /// The integer and fractional macro argument bytes.
    pub fn macro_args(self) -> (u8, u8) {
        ((self.0 >> 8) as u8, (self.0 & 0xFF) as u8)
    }

    /// Microseconds per quarter note at this tempo.
    pub fn micros_per_quarter(self) -> u32 {
        ((60_000_000u64 * 256) / u64::from(self.0)) as u32
    }

    /// Microseconds per tick at this tempo and resolution.
    pub fn micros_per_tick(self, tpb: u32) -> f64 {
        f64::from(self.micros_per_quarter()) / f64::from(tpb)
    }
}

impl std::fmt::Debug for Bpm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} BPM", self.as_f64())
    }
}

impl std::fmt::Display for Bpm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 % 256 == 0 {
            write!(f, "{}", self.0 / 256)
        } else {
            write!(f, "{:.2}", self.as_f64())
        }
    }
}

/// Errors from timing validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimingError {
    /// LPB must be non-zero.
    #[error("lines per beat must be non-zero")]
    ZeroLpb,
    /// TPB must be non-zero.
    #[error("ticks per beat must be non-zero")]
    ZeroTpb,
    /// LPB must divide TPB exactly.
    #[error("lines per beat ({lpb}) must divide ticks per beat ({tpb}) exactly")]
    Indivisible {
        /// The offending LPB.
        lpb: u32,
        /// The TPB it must divide.
        tpb: u32,
    },
    /// The resulting TPB would exceed the MIDI division limit.
    #[error("ticks per beat {0} exceeds the MIDI division limit of {MAX_TPB}")]
    TpbTooLarge(u32),
}

/// Greatest common divisor of two non-zero values.
pub fn gcd(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Least common multiple of two non-zero values, saturating on overflow.
pub fn lcm(a: u32, b: u32) -> u32 {
    if a == 0 || b == 0 {
        return 0;
    }
    a / gcd(a, b) * b
}

/// Whether `lpb` divides `tpb` exactly.
pub fn is_divisible(tpb: u32, lpb: u32) -> bool {
    lpb != 0 && tpb != 0 && tpb % lpb == 0
}

/// The number of global ticks in one pattern row.
pub fn row_ticks(tpb: u32, lpb: u32) -> Result<u32, TimingError> {
    if tpb == 0 {
        return Err(TimingError::ZeroTpb);
    }
    if lpb == 0 {
        return Err(TimingError::ZeroLpb);
    }
    if !is_divisible(tpb, lpb) {
        return Err(TimingError::Indivisible { lpb, tpb });
    }
    Ok(tpb / lpb)
}

/// The TPB required to represent `lpb` on top of `current`, checked against the
/// division limit.
pub fn resolve_tpb(current: u32, lpb: u32) -> Result<u32, TimingError> {
    if lpb == 0 {
        return Err(TimingError::ZeroLpb);
    }
    if current == 0 {
        return Err(TimingError::ZeroTpb);
    }
    let next = if current % lpb == 0 {
        current
    } else {
        lcm(current, lpb)
    };
    if next > MAX_TPB {
        return Err(TimingError::TpbTooLarge(next));
    }
    Ok(next)
}

/// Converts one tick offset measured at `old_tpb` to the same absolute time at
/// `new_tpb`, which must be an integer multiple of `old_tpb`.
pub fn rescale_offset(offset: u32, old_tpb: u32, new_tpb: u32) -> u32 {
    if old_tpb == 0 {
        return offset;
    }
    offset * (new_tpb / old_tpb)
}

/// A tempo segment: the tempo in effect from `start_tick` onward.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TempoSegment {
    /// First tick at which this tempo applies.
    pub start_tick: Ticks,
    /// Tempo in microseconds per quarter note.
    pub micros_per_quarter: u32,
}

/// A sorted map of tempo changes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TempoMap {
    segments: Vec<TempoSegment>,
}

impl TempoMap {
    /// Creates a map with a single tempo in effect from tick 0.
    pub fn new(micros_per_quarter: u32) -> Self {
        TempoMap {
            segments: vec![TempoSegment {
                start_tick: 0,
                micros_per_quarter,
            }],
        }
    }

    /// Creates a map whose initial tempo is `bpm`.
    pub fn from_bpm(bpm: Bpm) -> Self {
        TempoMap::new(bpm.micros_per_quarter())
    }

    /// The tempo in effect at `tick`.
    pub fn tempo_at(&self, tick: Ticks) -> u32 {
        self.segments
            .iter()
            .rev()
            .find(|s| s.start_tick <= tick)
            .map(|s| s.micros_per_quarter)
            .unwrap_or(self.segments[0].micros_per_quarter)
    }

    /// The initial tempo.
    pub fn initial_tempo(&self) -> u32 {
        self.segments[0].micros_per_quarter
    }

    /// Inserts or replaces a tempo change at `tick`.
    pub fn insert(&mut self, tick: Ticks, micros_per_quarter: u32) {
        match self.segments.binary_search_by_key(&tick, |s| s.start_tick) {
            Ok(i) => self.segments[i].micros_per_quarter = micros_per_quarter,
            Err(i) => self.segments.insert(
                i,
                TempoSegment {
                    start_tick: tick,
                    micros_per_quarter,
                },
            ),
        }
    }

    /// The elapsed microseconds between two ticks, honouring tempo changes.
    pub fn micros_between(&self, from: Ticks, to: Ticks, tpb: u32) -> u64 {
        if to <= from || tpb == 0 {
            return 0;
        }
        let mut total: u64 = 0;
        let mut cursor = from;
        for (index, segment) in self.segments.iter().enumerate() {
            let segment_end = self
                .segments
                .get(index + 1)
                .map(|s| s.start_tick)
                .unwrap_or(u64::MAX);
            if segment_end <= cursor {
                continue;
            }
            if segment.start_tick > cursor {
                cursor = segment.start_tick.min(to);
            }
            let end = segment_end.min(to);
            if end > cursor {
                let ticks = end - cursor;
                total += ticks * u64::from(segment.micros_per_quarter) / u64::from(tpb);
                cursor = end;
            }
            if cursor >= to {
                break;
            }
        }
        total
    }

    /// The elapsed microseconds from tick 0 to `tick`.
    pub fn time_at(&self, tick: Ticks, tpb: u32) -> u64 {
        self.micros_between(0, tick, tpb)
    }

    /// The segments, in tick order.
    pub fn segments(&self) -> &[TempoSegment] {
        &self.segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_conversions() {
        let bpm = Bpm::from_int(120).unwrap();
        assert_eq!(bpm.q8_8(), 30720);
        assert_eq!(bpm.as_f64(), 120.0);
        assert_eq!(bpm.micros_per_quarter(), 500_000);
        assert_eq!(bpm.macro_args(), (120, 0));
        assert_eq!(Bpm::from_macro_args(120, 0), Some(bpm));
        assert_eq!(Bpm::from_macro_args(0, 0), None);
        assert_eq!(Bpm::from_int(0), None);
    }

    #[test]
    fn fractional_bpm_is_exact_in_q8_8() {
        let bpm = Bpm::from_q8_8((120 << 8) | 128).unwrap();
        assert_eq!(bpm.as_f64(), 120.5);
        assert_eq!(bpm.macro_args(), (120, 128));
    }

    #[test]
    fn lcm_and_divisibility() {
        assert_eq!(lcm(24, 4), 24);
        assert_eq!(lcm(24, 5), 120);
        assert_eq!(lcm(24, 9), 72);
        assert!(is_divisible(24, 4));
        assert!(!is_divisible(24, 5));
        assert_eq!(row_ticks(24, 4).unwrap(), 6);
        assert_eq!(
            row_ticks(24, 5),
            Err(TimingError::Indivisible { lpb: 5, tpb: 24 })
        );
    }

    #[test]
    fn resolve_tpb_finds_common_multiple_and_respects_limit() {
        assert_eq!(resolve_tpb(24, 4).unwrap(), 24);
        assert_eq!(resolve_tpb(24, 5).unwrap(), 120);
        assert_eq!(resolve_tpb(24, 9).unwrap(), 72);
        assert!(resolve_tpb(MAX_TPB, 2).is_err());
    }

    #[test]
    fn offsets_rescale_exactly() {
        assert_eq!(rescale_offset(3, 24, 120), 15);
        assert_eq!(rescale_offset(0, 24, 120), 0);
        assert_eq!(rescale_offset(3, 24, 24), 3);
    }

    #[test]
    fn tempo_map_piecewise_accumulates() {
        let mut map = TempoMap::new(500_000); // 120 BPM
        map.insert(24, 250_000); // 240 BPM from tick 24
        assert_eq!(map.tempo_at(0), 500_000);
        assert_eq!(map.tempo_at(23), 500_000);
        assert_eq!(map.tempo_at(24), 250_000);
        // tpb = 24: one quarter note is 24 ticks.
        assert_eq!(map.micros_between(0, 24, 24), 500_000);
        assert_eq!(map.micros_between(0, 48, 24), 750_000);
        assert_eq!(map.time_at(24, 24), 500_000);
    }

    #[test]
    fn tempo_map_replace_is_idempotent() {
        let mut map = TempoMap::new(500_000);
        map.insert(0, 400_000);
        assert_eq!(map.segments().len(), 1);
        assert_eq!(map.initial_tempo(), 400_000);
    }
}
