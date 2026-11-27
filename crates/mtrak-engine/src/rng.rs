//! Deterministic pseudo-random numbers for macros.
//!
//! Macros get two ways to draw randomness, and both are deterministic:
//!
//! - `math.random`, seeded once per render from the composition's seed, so a
//!   render is a pure function of the saved project.
//! - `RandomGenerator(seed)`, an independent generator a macro owns, so its
//!   numbers do not depend on what any other macro drew.
//!
//! The algorithm is SplitMix64: a counter-based mix with no hidden state beyond
//! a 64-bit counter, stable across platforms and releases. It is not
//! cryptographic and is not meant to be.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, MetaMethod, MultiValue, UserData, UserDataMethods, Value};

/// SplitMix64 generator state.
///
/// Each `next_u64` advances a counter and mixes it, so the series is a pure
/// function of the seed and the number of draws taken so far.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Creates a generator starting at `seed`.
    pub fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    /// Resets the generator to `seed`.
    pub fn reseed(&mut self, seed: u64) {
        self.state = seed;
    }

    /// Returns the next value in the series.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl Default for SplitMix64 {
    fn default() -> Self {
        SplitMix64::new(0)
    }
}

/// A macro-owned independent generator, exposed to Lua as a userdata.
///
/// Draw with `rng:next(...)` or by calling the generator directly, `rng(...)`.
/// The argument conventions match `math.random`: no arguments gives a float in
/// `[0, 1)`, one argument gives an integer in `1..=m`, and two arguments give an
/// integer in `m..=n`.
pub struct RandomGenerator {
    rng: Rc<RefCell<SplitMix64>>,
}

impl RandomGenerator {
    /// Creates a generator seeded with `seed`.
    pub fn new(seed: u64) -> Self {
        RandomGenerator {
            rng: Rc::new(RefCell::new(SplitMix64::new(seed))),
        }
    }
}

impl UserData for RandomGenerator {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // `rng:next(...)` is the idiomatic method form.
        methods.add_method("next", |_, this, args: MultiValue| {
            draw(&mut this.rng.borrow_mut(), &args)
        });
        // `rng(...)` draws through the same series as `rng:next(...)`.
        methods.add_meta_method(MetaMethod::Call, |_, this, args: MultiValue| {
            draw(&mut this.rng.borrow_mut(), &args)
        });
    }
}

/// Draws one value following Lua's `math.random` conventions.
///
/// The argument conventions are deliberately identical to the standard library
/// so a macro author can use either without learning two sets of rules.
pub fn draw(rng: &mut SplitMix64, args: &MultiValue) -> mlua::Result<Value> {
    let values: Vec<&Value> = args.iter().collect();
    match values.as_slice() {
        [] => Ok(Value::Number(unit(rng))),
        [low] => {
            let low = integer(low)?;
            if low == 0 {
                // Lua's `math.random(0)` yields an integer with all bits set
                // pseudo-randomly.
                return Ok(Value::Integer(rng.next_u64() as i64));
            }
            bounded(rng, 1, low).map(Value::Integer)
        }
        [low, high] => {
            let low = integer(low)?;
            let high = integer(high)?;
            bounded(rng, low, high).map(Value::Integer)
        }
        _ => Err(mlua::Error::RuntimeError(
            "random takes at most two arguments".to_string(),
        )),
    }
}

/// A uniformly distributed float in `[0, 1)`, using the top 53 bits so the
/// result is exactly representable as an `f64`.
fn unit(rng: &mut SplitMix64) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// A uniformly distributed integer in `low..=high`, rejecting the biased tail
/// so every value is equally likely.
fn bounded(rng: &mut SplitMix64, low: i64, high: i64) -> mlua::Result<i64> {
    if low > high {
        return Err(mlua::Error::RuntimeError(format!(
            "random interval is empty ({low} > {high})"
        )));
    }
    let span = (high as i128 - low as i128 + 1) as u128;
    let value = if span >= 1u128 << 64 {
        u128::from(rng.next_u64())
    } else {
        let span = span as u64;
        // Reject candidates below this threshold so the modulo is unbiased.
        let threshold = span.wrapping_neg() % span;
        loop {
            let candidate = rng.next_u64();
            if candidate >= threshold {
                break u128::from(candidate % span);
            }
        }
    };
    Ok((low as i128 + value as i128) as i64)
}

/// Reads an integer argument, accepting integral floats the way Lua's own
/// argument checking does.
fn integer(value: &Value) -> mlua::Result<i64> {
    match value {
        Value::Integer(value) => Ok(*value),
        Value::Number(value) if value.fract() == 0.0 => Ok(*value as i64),
        _ => Err(mlua::Error::RuntimeError(
            "random arguments must be integers".to_string(),
        )),
    }
}

/// Installs `math.random` and `random-generator` into a module environment.
///
/// The stock `math` table is copied so the sandbox never mutates the privileged
/// state, and its random entries are replaced: `math.random` draws from the
/// composition-seeded global series and `math.randomseed` refuses to run,
/// because reseeding would break the "a render is a pure function of the
/// project" guarantee.
pub fn install(lua: &Lua, env: &mlua::Table, global: Rc<RefCell<SplitMix64>>) -> mlua::Result<()> {
    let globals = lua.globals();
    let math: mlua::Table = globals.get("math")?;
    let curated = lua.create_table()?;
    for pair in math.pairs::<Value, Value>() {
        let (key, value) = pair?;
        curated.set(key, value)?;
    }
    curated.set(
        "random",
        lua.create_function(move |_, args: MultiValue| draw(&mut global.borrow_mut(), &args))?,
    )?;
    curated.set(
        "randomseed",
        lua.create_function(|_, _: MultiValue| {
            Err::<(), _>(mlua::Error::RuntimeError(
                "math.randomseed is not available: the seed comes from the \
                 composition (:seed) or from RandomGenerator(seed)"
                    .to_string(),
            ))
        })?,
    )?;
    env.set("math", curated)?;

    let constructor = lua.create_function(|lua, seed: Value| {
        let seed = integer(&seed)? as u64;
        lua.create_userdata(RandomGenerator::new(seed))
    })?;
    env.set("random-generator", constructor.clone())?;
    env.set(
        crate::fennel::fennel_global_name("random-generator"),
        constructor,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_the_series() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..32 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn reseeding_restarts_the_series() {
        let mut rng = SplitMix64::new(9);
        let first: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        rng.reseed(9);
        let again: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        assert_eq!(first, again);
    }

    #[test]
    fn unit_stays_in_the_half_open_range() {
        let mut rng = SplitMix64::new(3);
        for _ in 0..1_000 {
            let value = unit(&mut rng);
            assert!((0.0..1.0).contains(&value), "{value}");
        }
    }

    #[test]
    fn bounded_covers_the_interval_inclusively() {
        let mut rng = SplitMix64::new(5);
        let mut seen = [false; 8];
        for _ in 0..1_000 {
            let value = bounded(&mut rng, 0, 7).unwrap();
            assert!((0..=7).contains(&value), "{value}");
            seen[value as usize] = true;
        }
        assert!(seen.iter().all(|hit| *hit), "every value should appear");
    }

    #[test]
    fn a_single_value_interval_is_constant() {
        let mut rng = SplitMix64::new(11);
        for _ in 0..10 {
            assert_eq!(bounded(&mut rng, 5, 5).unwrap(), 5);
        }
    }

    #[test]
    fn an_empty_interval_is_an_error() {
        let mut rng = SplitMix64::new(1);
        assert!(bounded(&mut rng, 9, 3).is_err());
    }

    #[test]
    fn the_full_i64_interval_is_reachable() {
        let mut rng = SplitMix64::new(13);
        // Exercises the 2^64 span branch rather than the modulo branch.
        let value = bounded(&mut rng, i64::MIN, i64::MAX).unwrap();
        assert!((i64::MIN..=i64::MAX).contains(&value));
    }
}
