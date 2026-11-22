//! Colon-command parsing.
//!
//! Commands accept decimal or `0x`/`0X` hexadecimal integers; invalid values
//! produce an error and never mutate project state (see `PLAN.md` §2.10). The
//! retained command names and aliases keep the Go prototype's semantics.

use mtrak_core::composition::PatternId;
use mtrak_core::settings::parse_note;

/// Arguments to `:scale`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ScaleArgs {
    /// A root note name.
    pub root: Option<String>,
    /// A scale name.
    pub scale: Option<String>,
    /// A modal degree (from `+MODE`).
    pub mode: Option<u32>,
    /// `add NAME MASK`.
    pub add: Option<(String, u16)>,
    /// `use NAME`.
    pub use_name: Option<String>,
    /// `list`.
    pub list: bool,
    /// `delete NAME`.
    pub delete: Option<String>,
}

/// Argument to `:seed`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeedArg {
    /// Show the current seed without changing it.
    Show,
    /// Set an explicit seed.
    Set(u64),
    /// Draw a fresh seed from system entropy.
    Random,
}

/// A parsed colon command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// `:read [filename]` / `:r`.
    Read(Option<String>),
    /// `:write [filename]` / `:w`.
    Write(Option<String>),
    /// `:export [filename]`.
    Export(Option<String>),
    /// `:bpm INTEGER`.
    Bpm(u32),
    /// `:tpb INTEGER`.
    Tpb(u32),
    /// `:lpb INTEGER`.
    Lpb(u32),
    /// `:root NOTE`.
    Root(String),
    /// `:scale` / `:sca` / `:s`.
    Scale(ScaleArgs),
    /// `:mode INTEGER` / `:m`.
    Mode(u32),
    /// `:rows INTEGER`.
    Rows(u32),
    /// `:tracks INTEGER`.
    Tracks(u32),
    /// `:pattern new [lpb]`.
    PatternNew(Option<u32>),
    /// `:pattern select ID`.
    PatternSelect(PatternId),
    /// `:pattern delete ID`.
    PatternDelete(PatternId),
    /// `:pattern main ID`.
    PatternMain(PatternId),
    /// `:port list`.
    PortList,
    /// `:port select ID`.
    PortSelect(usize),
    /// `:event BYTES` — a hex byte sequence for the current cell.
    Event(Vec<u8>),
    /// `:follow` — toggle scrolling with the playhead.
    Follow,
    /// `:seed [VALUE|random]` — inspect or change the random seed.
    Seed(SeedArg),
    /// `:help`.
    Help,
}

/// Parses an integer in decimal or `0x`/`0X` hexadecimal.
pub fn parse_int(input: &str) -> Option<u32> {
    let input = input.trim();
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).ok()
    } else {
        input.parse::<u32>().ok()
    }
}

/// Parses a 64-bit integer in decimal or `0x` hexadecimal.
pub fn parse_u64(input: &str) -> Option<u64> {
    let input = input.trim();
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        input.parse::<u64>().ok()
    }
}

/// Parses a command line (with or without a leading colon).
pub fn parse(input: &str) -> Result<Command, String> {
    let line = input.trim().trim_start_matches(':').trim();
    if line.is_empty() {
        return Err("empty command".to_string());
    }
    let mut parts = line.split_whitespace();
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();

    let int_arg = |rest: &[&str], which: &str| -> Result<u32, String> {
        let raw = rest
            .first()
            .ok_or_else(|| format!("{which} needs a value"))?;
        parse_int(raw).ok_or_else(|| format!("invalid value for {which}: {raw}"))
    };

    let command = match name.as_str() {
        "read" | "r" => Command::Read(join_path(&rest)),
        "write" | "w" => Command::Write(join_path(&rest)),
        "export" => Command::Export(join_path(&rest)),
        "bpm" => Command::Bpm(int_arg(&rest, "bpm")?),
        "tpb" => Command::Tpb(int_arg(&rest, "tpb")?),
        "lpb" => Command::Lpb(int_arg(&rest, "lpb")?),
        "root" => Command::Root(
            rest.first()
                .ok_or_else(|| "root needs a note".to_string())?
                .to_string(),
        ),
        "scale" | "sca" | "s" => Command::Scale(parse_scale_args(&rest)?),
        "mode" | "m" => Command::Mode(int_arg(&rest, "mode")?),
        "rows" => Command::Rows(int_arg(&rest, "rows")?),
        "tracks" => Command::Tracks(int_arg(&rest, "tracks")?),
        "pattern" | "pat" => parse_pattern(&rest)?,
        "port" | "midi" => parse_port(&rest)?,
        "event" | "ev" => Command::Event(parse_event_bytes(&rest)?),
        "follow" | "fol" => Command::Follow,
        "seed" => {
            let raw = rest.first().map(|value| value.to_ascii_lowercase());
            match raw.as_deref() {
                None => Command::Seed(SeedArg::Show),
                Some("random") => Command::Seed(SeedArg::Random),
                Some(other) => {
                    let value = parse_u64(other)
                        .ok_or_else(|| format!("invalid value for seed: {other}"))?;
                    Command::Seed(SeedArg::Set(value))
                }
            }
        }
        "help" | "h" | "?" => Command::Help,
        other => return Err(format!("unknown command: {other}")),
    };
    Ok(command)
}

/// Joins the remaining words of a file command, which take one path and may
/// contain spaces.
fn join_path(rest: &[&str]) -> Option<String> {
    if rest.is_empty() {
        None
    } else {
        Some(rest.join(" "))
    }
}

fn parse_scale_args(rest: &[&str]) -> Result<ScaleArgs, String> {
    let mut args = ScaleArgs::default();
    let mut i = 0;
    while i < rest.len() {
        let token = rest[i];
        match token {
            "add" => {
                let name = rest
                    .get(i + 1)
                    .ok_or_else(|| "scale add needs a name".to_string())?;
                let mask = rest
                    .get(i + 2)
                    .ok_or_else(|| "scale add needs a mask".to_string())?;
                let parsed = mtrak_core::settings::Scale::parse_mask(mask)
                    .ok_or_else(|| format!("invalid scale mask: {mask}"))?;
                args.add = Some(((*name).to_string(), parsed.mask()));
                i += 3;
            }
            "use" => {
                let name = rest
                    .get(i + 1)
                    .ok_or_else(|| "scale use needs a name".to_string())?;
                args.use_name = Some((*name).to_string());
                i += 2;
            }
            "delete" | "del" => {
                let name = rest
                    .get(i + 1)
                    .ok_or_else(|| "scale delete needs a name".to_string())?;
                args.delete = Some((*name).to_string());
                i += 2;
            }
            "list" => {
                args.list = true;
                i += 1;
            }
            other => {
                if let Some(mode) = other.strip_prefix('+') {
                    args.mode = Some(
                        parse_int(mode).ok_or_else(|| format!("invalid modal degree: {mode}"))?,
                    );
                } else if parse_note(other).is_some() {
                    args.root = Some(other.to_string());
                } else {
                    args.scale = Some(other.to_string());
                }
                i += 1;
            }
        }
    }
    Ok(args)
}

fn parse_pattern(rest: &[&str]) -> Result<Command, String> {
    let sub = rest
        .first()
        .ok_or_else(|| "pattern needs a subcommand".to_string())?
        .to_ascii_lowercase();
    match sub.as_str() {
        "new" => match rest.get(1) {
            Some(raw) => Ok(Command::PatternNew(Some(
                parse_int(raw).ok_or_else(|| format!("invalid lpb: {raw}"))?,
            ))),
            None => Ok(Command::PatternNew(None)),
        },
        "select" | "use" => {
            let raw = rest
                .get(1)
                .ok_or_else(|| "pattern select needs an ID".to_string())?;
            let id = parse_int(raw).ok_or_else(|| format!("invalid pattern ID: {raw}"))?;
            Ok(Command::PatternSelect(PatternId(id as u8)))
        }
        "delete" | "del" => {
            let raw = rest
                .get(1)
                .ok_or_else(|| "pattern delete needs an ID".to_string())?;
            let id = parse_int(raw).ok_or_else(|| format!("invalid pattern ID: {raw}"))?;
            Ok(Command::PatternDelete(PatternId(id as u8)))
        }
        "main" => {
            let raw = rest
                .get(1)
                .ok_or_else(|| "pattern main needs an ID".to_string())?;
            let id = parse_int(raw).ok_or_else(|| format!("invalid pattern ID: {raw}"))?;
            Ok(Command::PatternMain(PatternId(id as u8)))
        }
        other => Err(format!("unknown pattern subcommand: {other}")),
    }
}

/// Parses whitespace-separated hexadecimal bytes (or one compact hex string).
fn parse_event_bytes(rest: &[&str]) -> Result<Vec<u8>, String> {
    let joined: String = rest.concat();
    if joined.is_empty() {
        return Err("event needs a hex byte sequence".to_string());
    }
    if joined.len() % 2 != 0 {
        return Err(format!("event bytes must be whole bytes: {joined}"));
    }
    let mut bytes = Vec::with_capacity(joined.len() / 2);
    let chars: Vec<char> = joined.chars().collect();
    for pair in chars.chunks(2) {
        let text: String = pair.iter().collect();
        let byte =
            u8::from_str_radix(&text, 16).map_err(|_| format!("invalid event byte: {text}"))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

fn parse_port(rest: &[&str]) -> Result<Command, String> {
    match rest.first() {
        None => Ok(Command::PortList),
        Some(first) => {
            let sub = first.to_ascii_lowercase();
            match sub.as_str() {
                "list" => Ok(Command::PortList),
                "select" | "use" => {
                    let raw = rest
                        .get(1)
                        .ok_or_else(|| "port select needs an index".to_string())?;
                    let id = parse_int(raw).ok_or_else(|| format!("invalid port index: {raw}"))?;
                    Ok(Command::PortSelect(id as usize))
                }
                other => Err(format!("unknown port subcommand: {other}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_accept_decimal_and_hex() {
        assert_eq!(parse_int("64"), Some(64));
        assert_eq!(parse_int("0x40"), Some(64));
        assert_eq!(parse_int("0X40"), Some(64));
        assert_eq!(parse_int("abc"), None);
    }

    #[test]
    fn retained_commands_parse_with_aliases() {
        assert_eq!(
            parse(":r foo.mid"),
            Ok(Command::Read(Some("foo.mid".to_string())))
        );
        assert_eq!(parse(":w"), Ok(Command::Write(None)));
        assert_eq!(parse(":bpm 140"), Ok(Command::Bpm(140)));
        assert_eq!(parse(":bpm 0x40"), Ok(Command::Bpm(64)));
        assert_eq!(parse(":tpb 24"), Ok(Command::Tpb(24)));
        assert_eq!(parse(":lpb 8"), Ok(Command::Lpb(8)));
        assert_eq!(parse(":m 2"), Ok(Command::Mode(2)));
        assert_eq!(parse(":mode 2"), Ok(Command::Mode(2)));
        assert_eq!(parse(":rows 64"), Ok(Command::Rows(64)));
        assert_eq!(parse(":tracks 16"), Ok(Command::Tracks(16)));
        assert_eq!(parse(":root C-4"), Ok(Command::Root("C-4".to_string())));
        assert_eq!(
            parse(":r my songs/live set.mid"),
            Ok(Command::Read(Some("my songs/live set.mid".to_string()))),
            "file commands take the rest of the line as one path"
        );
        assert_eq!(parse(":follow"), Ok(Command::Follow));
        assert_eq!(parse(":fol"), Ok(Command::Follow));
        assert_eq!(parse(":seed"), Ok(Command::Seed(SeedArg::Show)));
        assert_eq!(parse(":seed 42"), Ok(Command::Seed(SeedArg::Set(42))));
        assert_eq!(
            parse(":seed 0xDEADBEEF"),
            Ok(Command::Seed(SeedArg::Set(0xDEADBEEF)))
        );
        assert_eq!(parse(":seed random"), Ok(Command::Seed(SeedArg::Random)));
        assert!(parse(":seed nonsense").is_err());
    }

    #[test]
    fn scale_command_accepts_root_scale_and_mode_in_any_order() {
        let parsed = parse(":scale C-4 major +1").unwrap();
        match parsed {
            Command::Scale(args) => {
                assert_eq!(args.root.as_deref(), Some("C-4"));
                assert_eq!(args.scale.as_deref(), Some("major"));
                assert_eq!(args.mode, Some(1));
            }
            other => panic!("unexpected {other:?}"),
        }
        // Aliases and reordering.
        let parsed = parse(":s +2 minor").unwrap();
        match parsed {
            Command::Scale(args) => {
                assert_eq!(args.mode, Some(2));
                assert_eq!(args.scale.as_deref(), Some("minor"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scale_subcommands_parse() {
        match parse(":scale add weird 101001010100").unwrap() {
            Command::Scale(args) => {
                assert_eq!(args.add, Some(("weird".to_string(), 0x0A54)))
            }
            other => panic!("unexpected {other:?}"),
        }
        match parse(":scale use weird").unwrap() {
            Command::Scale(args) => assert_eq!(args.use_name.as_deref(), Some("weird")),
            other => panic!("unexpected {other:?}"),
        }
        match parse(":scale list").unwrap() {
            Command::Scale(args) => assert!(args.list),
            other => panic!("unexpected {other:?}"),
        }
        match parse(":scale delete weird").unwrap() {
            Command::Scale(args) => assert_eq!(args.delete.as_deref(), Some("weird")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pattern_and_port_commands_parse() {
        assert_eq!(parse(":pattern new 8"), Ok(Command::PatternNew(Some(8))));
        assert_eq!(parse(":pattern new"), Ok(Command::PatternNew(None)));
        assert_eq!(
            parse(":pattern select 5"),
            Ok(Command::PatternSelect(PatternId(5)))
        );
        assert_eq!(
            parse(":pat delete 0x5"),
            Ok(Command::PatternDelete(PatternId(5)))
        );
        assert_eq!(
            parse(":pattern main 2"),
            Ok(Command::PatternMain(PatternId(2)))
        );
        assert_eq!(parse(":port"), Ok(Command::PortList));
        assert_eq!(parse(":port select 1"), Ok(Command::PortSelect(1)));
    }

    #[test]
    fn invalid_inputs_error_without_panicking() {
        assert!(parse(":bpm abc").is_err());
        assert!(parse(":mode").is_err());
        assert!(parse(":nonsense").is_err());
        assert!(parse("").is_err());
        assert!(parse(":scale add bad 1234").is_err());
    }

    #[test]
    fn event_command_parses_spaced_and_compact_hex() {
        assert_eq!(
            parse(":event 90 64 3F"),
            Ok(Command::Event(vec![0x90, 0x64, 0x3F]))
        );
        assert_eq!(
            parse(":event 90643F"),
            Ok(Command::Event(vec![0x90, 0x64, 0x3F]))
        );
        assert_eq!(
            parse(":ev F0 7D 01 02 F7"),
            Ok(Command::Event(vec![0xF0, 0x7D, 0x01, 0x02, 0xF7]))
        );
    }

    #[test]
    fn event_command_rejects_malformed_hex() {
        assert!(parse(":event").is_err());
        assert!(parse(":event 9").is_err());
        assert!(parse(":event 90 6").is_err());
        assert!(parse(":event 90 zz").is_err());
    }
}
