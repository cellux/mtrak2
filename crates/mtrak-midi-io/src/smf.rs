//! Standard MIDI File reading and writing.
//!
//! Reading uses `midly` (which correctly resolves running status); writing is a
//! small container writer built on the core event codec. Project save embeds
//! tracker metadata plus a marked rendered fallback; standard export omits both.

use midly::num::u4;
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use mtrak_core::composition::{Composition, Pattern, PatternId, MAX_ROWS};
use mtrak_core::event::{
    ChannelKind, ChannelMessage, Event, KeySignature, MetaEvent, SmpteOffset, TextKind,
    TimeSignature,
};
use mtrak_core::timing::{Bpm, Ticks};
use mtrak_engine::macros::MacroFactory;
use mtrak_engine::scheduler::{Engine, TimedEvent};

use crate::metadata::{
    assemble_chunks, decode_project, encode_chunks, encode_project, fallback_marker,
    is_fallback_marker, parse_metadata_chunk, MetadataChunk, MetadataError, TrackerProject,
};

/// Errors from MIDI file operations.
#[derive(Debug, thiserror::Error)]
pub enum SmfError {
    /// The file could not be parsed.
    #[error("could not parse MIDI file: {0}")]
    Parse(String),
    /// The file is structurally invalid.
    #[error("invalid MIDI file: {0}")]
    Invalid(String),
    /// Metadata decoding failed.
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    /// The composition could not be assembled.
    #[error("composition error: {0}")]
    Composition(String),
    /// An event could not be encoded.
    #[error("event could not be encoded: {0}")]
    Encode(String),
}

/// A file loaded from disk: either a tracker project or an imported standard
/// MIDI file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedFile {
    /// The reconstructed project.
    pub project: TrackerProject,
    /// Whether authoritative tracker metadata was present.
    pub was_project: bool,
}

/// Renders a composition into timestamped events using the scheduler.
pub fn render_events(
    composition: &Composition,
    factory: Option<Box<dyn MacroFactory>>,
) -> Vec<TimedEvent> {
    let mut engine = Engine::new(composition.clone());
    if let Some(factory) = factory {
        engine.set_macro_factory(factory);
    }
    if !engine.start(composition.selected_main()) {
        return Vec::new();
    }
    engine.run();
    engine.into_output()
}

fn encodable(event: &Event) -> bool {
    matches!(
        event,
        Event::Channel(_) | Event::SysEx(_) | Event::Meta(_) | Event::Raw(_)
    )
}

fn collect_track(events: Vec<TimedEvent>, prefix: Option<Event>) -> Vec<(Ticks, Event)> {
    let mut track = Vec::new();
    if let Some(event) = prefix {
        track.push((0, event));
    }
    for timed in events {
        if encodable(&timed.event) {
            track.push((timed.tick, timed.event));
        }
    }
    track
}

/// Writes an SMF from absolute-timed tracks.
///
/// Each track's events must be in non-decreasing tick order; an `EndOfTrack`
/// meta-event is appended automatically.
pub fn write_smf(ppq: u16, tracks: &[Vec<(Ticks, Event)>]) -> Result<Vec<u8>, SmfError> {
    let mut out = Vec::new();
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes());
    let format: u16 = if tracks.len() > 1 { 1 } else { 0 };
    out.extend_from_slice(&format.to_be_bytes());
    out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    out.extend_from_slice(&ppq.to_be_bytes());

    for track in tracks {
        let mut body = Vec::new();
        let mut last = 0u64;
        for (tick, event) in track {
            let delta = tick.saturating_sub(last);
            let delta = u32::try_from(delta)
                .map_err(|_| SmfError::Invalid("delta-time overflow".to_string()))?;
            mtrak_core::codec::write_vlq(delta, &mut body);
            let bytes = event
                .to_smf_bytes()
                .map_err(|e| SmfError::Encode(e.to_string()))?;
            body.extend_from_slice(&bytes);
            last = *tick;
        }
        body.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        out.extend_from_slice(b"MTrk");
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }
    Ok(out)
}

/// Saves a tracker project: metadata in a conductor track plus a marked,
/// rendered fallback that non-tracker software can play.
pub fn save_project(
    project: &TrackerProject,
    factory: Option<Box<dyn MacroFactory>>,
) -> Result<Vec<u8>, SmfError> {
    let ppq = u16::try_from(project.composition.tpb())
        .map_err(|_| SmfError::Invalid("tpb does not fit a MIDI division".to_string()))?;
    let blob = encode_project(project);
    let conductor: Vec<(Ticks, Event)> = encode_chunks(&blob)
        .into_iter()
        .map(|chunk| (0, Event::Meta(MetaEvent::SequencerSpecific(chunk))))
        .collect();
    let rendered = render_events(&project.composition, factory);
    let fallback = collect_track(
        rendered,
        Some(Event::Meta(MetaEvent::SequencerSpecific(fallback_marker()))),
    );
    write_smf(ppq, &[conductor, fallback])
}

/// Exports standard MIDI: custom behaviour rendered, tracker metadata stripped.
pub fn export_standard(
    composition: &Composition,
    factory: Option<Box<dyn MacroFactory>>,
) -> Result<Vec<u8>, SmfError> {
    let ppq = u16::try_from(composition.tpb())
        .map_err(|_| SmfError::Invalid("tpb does not fit a MIDI division".to_string()))?;
    let rendered = render_events(composition, factory);
    let track = collect_track(rendered, None);
    write_smf(ppq, &[track])
}

/// Loads a `.mid` file, detecting tracker metadata automatically.
pub fn load(bytes: &[u8]) -> Result<LoadedFile, SmfError> {
    let smf = Smf::parse(bytes).map_err(|e| SmfError::Parse(e.to_string()))?;
    let mut chunks: Vec<MetadataChunk> = Vec::new();
    for track in &smf.tracks {
        for event in track {
            if let TrackEventKind::Meta(MetaMessage::SequencerSpecific(data)) = event.kind {
                if let Some(chunk) = parse_metadata_chunk(data) {
                    chunks.push(chunk);
                }
            }
        }
    }
    if !chunks.is_empty() {
        let blob = assemble_chunks(&chunks)?;
        let project = decode_project(&blob)?;
        return Ok(LoadedFile {
            project,
            was_project: true,
        });
    }
    Ok(LoadedFile {
        project: import_standard(&smf)?,
        was_project: false,
    })
}

/// Imports a standard MIDI file into a fresh tracker project.
pub fn import_standard(smf: &Smf<'_>) -> Result<TrackerProject, SmfError> {
    let tpb = match smf.header.timing {
        Timing::Metrical(ppq) => u32::from(ppq.as_int()),
        // SMPTE timing has no exact tick-per-beat equivalent; use a sane default.
        Timing::Timecode(..) => 24,
    };

    let mut events: Vec<(Ticks, Event)> = Vec::new();
    let mut initial_tempo: Option<u32> = None;
    for track in &smf.tracks {
        let mut tick: Ticks = 0;
        for event in track {
            tick += u64::from(event.delta.as_int());
            match &event.kind {
                TrackEventKind::Midi { channel, message } => {
                    events.push((tick, channel_from_midi(*channel, *message)));
                }
                TrackEventKind::SysEx(data) => events.push((tick, Event::SysEx(data.to_vec()))),
                TrackEventKind::Escape(data) => events.push((tick, Event::Raw(data.to_vec()))),
                TrackEventKind::Meta(meta) => {
                    if let Some(event) = meta_from_midly(meta) {
                        if initial_tempo.is_none() {
                            if let Event::Meta(MetaEvent::Tempo(micros)) = &event {
                                initial_tempo = Some(*micros);
                            }
                        }
                        if !matches!(event, Event::Meta(MetaEvent::EndOfTrack)) {
                            events.push((tick, event));
                        }
                    }
                }
            }
        }
    }
    // Stable sort puts events in tick order while preserving source order.
    events.sort_by_key(|(tick, _)| *tick);

    let max_tick = events.last().map(|(tick, _)| *tick).unwrap_or(0);
    // Lossless import: one tick per row when it fits, so only truly
    // simultaneous events ever collide with each other.
    let tick_len = choose_tick_len(tpb, max_tick);
    let lpb = tpb / tick_len;
    let rows = ((max_tick / u64::from(tick_len)) as usize + 1).clamp(1, MAX_ROWS);

    // Greedy per-row track assignment: events sharing a row need distinct
    // tracks; events in different rows never collide.
    let mut max_tracks = 1usize;
    {
        let mut row_cursor = vec![0usize; rows];
        for (tick, _) in &events {
            let row = (*tick / u64::from(tick_len)) as usize;
            if row < rows {
                row_cursor[row] += 1;
                max_tracks = max_tracks.max(row_cursor[row]);
            }
        }
    }
    let mut pattern = Pattern::new(PatternId(0), lpb, rows, max_tracks);
    {
        let mut row_cursor = vec![0usize; rows];
        for (tick, event) in events {
            let row = (tick / u64::from(tick_len)) as usize;
            if row >= rows {
                continue;
            }
            let track = row_cursor[row];
            row_cursor[row] += 1;
            let offset = (tick % u64::from(tick_len)) as u32;
            if let Some(slot) = pattern.slot_mut(row, track) {
                slot.event = event;
                slot.offset = offset;
            }
        }
    }

    let mut composition = Composition::empty();
    if let Some(micros) = initial_tempo {
        if let Some(bpm) = bpm_from_micros(micros) {
            composition.set_bpm(bpm);
        }
    }
    composition
        .set_tpb_raw(tpb)
        .map_err(|e| SmfError::Composition(e.to_string()))?;
    composition
        .place_pattern(pattern)
        .map_err(|e| SmfError::Composition(e.to_string()))?;
    composition.set_selected_main_raw(PatternId(0));

    Ok(TrackerProject {
        composition,
        lua_modules: Default::default(),
        setup_source: None,
    })
}

fn choose_tick_len(tpb: u32, max_tick: Ticks) -> u32 {
    if (max_tick as usize + 1) <= MAX_ROWS {
        return 1;
    }
    let mut divisors: Vec<u32> = (1..=tpb).filter(|d| tpb % d == 0).collect();
    divisors.sort_unstable();
    for d in divisors {
        let rows = (max_tick / u64::from(d)) as usize + 1;
        if rows <= MAX_ROWS {
            return d;
        }
    }
    tpb.max(1)
}

fn bpm_from_micros(micros: u32) -> Option<Bpm> {
    if micros == 0 {
        return None;
    }
    let q8_8 = (60_000_000u64 * 256 / u64::from(micros)) as u32;
    Bpm::from_q8_8(q8_8)
}

fn channel_from_midi(channel: u4, message: MidiMessage) -> Event {
    let channel = channel.as_int();
    let message = match message {
        MidiMessage::NoteOff { key, vel } => ChannelMessage {
            kind: ChannelKind::NoteOff,
            channel: Some(channel),
            data1: Some(key.as_int()),
            data2: Some(vel.as_int()),
        },
        MidiMessage::NoteOn { key, vel } => ChannelMessage {
            kind: ChannelKind::NoteOn,
            channel: Some(channel),
            data1: Some(key.as_int()),
            data2: Some(vel.as_int()),
        },
        MidiMessage::Aftertouch { key, vel } => ChannelMessage {
            kind: ChannelKind::PolyAftertouch,
            channel: Some(channel),
            data1: Some(key.as_int()),
            data2: Some(vel.as_int()),
        },
        MidiMessage::Controller { controller, value } => ChannelMessage {
            kind: ChannelKind::ControlChange,
            channel: Some(channel),
            data1: Some(controller.as_int()),
            data2: Some(value.as_int()),
        },
        MidiMessage::ProgramChange { program } => ChannelMessage {
            kind: ChannelKind::ProgramChange,
            channel: Some(channel),
            data1: Some(program.as_int()),
            data2: None,
        },
        MidiMessage::ChannelAftertouch { vel } => ChannelMessage {
            kind: ChannelKind::ChannelAftertouch,
            channel: Some(channel),
            data1: Some(vel.as_int()),
            data2: None,
        },
        MidiMessage::PitchBend { bend } => {
            let value = bend.0.as_int();
            ChannelMessage {
                kind: ChannelKind::PitchBend,
                channel: Some(channel),
                data1: Some((value & 0x7F) as u8),
                data2: Some(((value >> 7) & 0x7F) as u8),
            }
        }
    };
    Event::Channel(message)
}

fn meta_from_midly(meta: &MetaMessage<'_>) -> Option<Event> {
    let meta = match meta {
        MetaMessage::TrackNumber(value) => match value {
            Some(number) => MetaEvent::SequenceNumber(*number),
            None => MetaEvent::Unknown {
                kind: 0x00,
                data: Vec::new(),
            },
        },
        MetaMessage::Text(bytes) => MetaEvent::text(TextKind::Text, bytes.to_vec()),
        MetaMessage::Copyright(bytes) => MetaEvent::text(TextKind::Copyright, bytes.to_vec()),
        MetaMessage::TrackName(bytes) => MetaEvent::text(TextKind::TrackName, bytes.to_vec()),
        MetaMessage::InstrumentName(bytes) => {
            MetaEvent::text(TextKind::InstrumentName, bytes.to_vec())
        }
        MetaMessage::Lyric(bytes) => MetaEvent::text(TextKind::Lyric, bytes.to_vec()),
        MetaMessage::Marker(bytes) => MetaEvent::text(TextKind::Marker, bytes.to_vec()),
        MetaMessage::CuePoint(bytes) => MetaEvent::text(TextKind::CuePoint, bytes.to_vec()),
        MetaMessage::ProgramName(bytes) => MetaEvent::text(TextKind::ProgramName, bytes.to_vec()),
        MetaMessage::DeviceName(bytes) => MetaEvent::text(TextKind::DeviceName, bytes.to_vec()),
        MetaMessage::MidiChannel(channel) => MetaEvent::ChannelPrefix(channel.as_int()),
        MetaMessage::MidiPort(port) => MetaEvent::Unknown {
            kind: 0x21,
            data: vec![port.as_int()],
        },
        MetaMessage::EndOfTrack => MetaEvent::EndOfTrack,
        MetaMessage::Tempo(tempo) => MetaEvent::Tempo(tempo.as_int()),
        MetaMessage::SmpteOffset(time) => {
            let code = match time.fps().as_int() {
                25 => 1u8,
                29 => 2,
                30 => 3,
                _ => 0,
            };
            MetaEvent::SmpteOffset(SmpteOffset([
                (code << 5) | (time.hour() & 0x1F),
                time.minute(),
                time.second(),
                time.frame(),
                time.subframe(),
            ]))
        }
        MetaMessage::TimeSignature(numerator, denominator, clocks, thirty_seconds) => {
            MetaEvent::TimeSignature(TimeSignature {
                numerator: *numerator,
                denominator_pow2: *denominator,
                clocks_per_click: *clocks,
                notated_32nds_per_quarter: *thirty_seconds,
            })
        }
        MetaMessage::KeySignature(sharps_flats, is_minor) => {
            MetaEvent::KeySignature(KeySignature {
                sharps_flats: *sharps_flats,
                is_minor: *is_minor,
            })
        }
        MetaMessage::SequencerSpecific(data) => MetaEvent::SequencerSpecific(data.to_vec()),
        MetaMessage::Unknown(kind, data) => MetaEvent::Unknown {
            kind: *kind,
            data: data.to_vec(),
        },
    };
    Some(Event::Meta(meta))
}

/// Whether a loaded file's fallback track should be ignored (always true when
/// metadata is present; provided for callers that inspect raw tracks).
pub fn is_fallback(data: &[u8]) -> bool {
    is_fallback_marker(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::event::MacroCall;

    fn note(key: u8, channel: u8) -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            channel,
            key,
            Some(100),
        ))
    }

    fn sample_project() -> TrackerProject {
        let mut composition = Composition::new();
        {
            let pattern = composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(8).unwrap();
            pattern.resize_tracks(2).unwrap();
            pattern.set_event(0, 0, note(60, 0));
            pattern.set_event(2, 0, note(62, 0));
            pattern.slot_mut(1, 0).unwrap().offset = 3;
            pattern.set_event(0, 1, Event::Meta(MetaEvent::Tempo(400_000)));
            pattern.set_event(
                3,
                1,
                Event::Meta(MetaEvent::TimeSignature(TimeSignature {
                    numerator: 3,
                    denominator_pow2: 2,
                    clocks_per_click: 24,
                    notated_32nds_per_quarter: 8,
                })),
            );
        }
        TrackerProject {
            composition,
            lua_modules: Default::default(),
            setup_source: None,
        }
    }

    #[test]
    fn project_save_and_load_round_trips_without_fallback_duplication() {
        let project = sample_project();
        let bytes = save_project(&project, None).unwrap();
        let loaded = load(&bytes).unwrap();
        assert!(loaded.was_project);
        // The rendered fallback must not appear as composition data.
        assert_eq!(
            loaded.project.composition.pattern_ids(),
            project.composition.pattern_ids()
        );
        let original = project.composition.pattern(PatternId(0)).unwrap();
        let restored = loaded.project.composition.pattern(PatternId(0)).unwrap();
        assert_eq!(original.row_count(), restored.row_count());
        assert_eq!(original.track_count(), restored.track_count());
        assert_eq!(restored.slot(0, 0).unwrap().event, note(60, 0));
        assert_eq!(restored.slot(1, 0).unwrap().offset, 3);
        assert_eq!(
            restored.slot(0, 1).unwrap().event,
            Event::Meta(MetaEvent::Tempo(400_000))
        );
    }

    #[test]
    fn standard_export_has_no_metadata_and_renders_macros() {
        let mut composition = Composition::new();
        let child = composition.create_pattern(4).unwrap();
        {
            let pattern = composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(2).unwrap();
            pattern.set_event(0, 0, note(60, 0));
            pattern.set_event(
                1,
                0,
                Event::Macro(MacroCall::new(
                    mtrak_core::event::macro_id::BUILTIN_TRIGGER,
                    child.value(),
                    0,
                )),
            );
        }
        {
            let pattern = composition.pattern_mut(child).unwrap();
            pattern.resize_rows(1).unwrap();
            pattern.set_event(0, 0, note(72, 0));
        }
        let bytes = export_standard(&composition, None).unwrap();
        let smf = Smf::parse(&bytes).unwrap();
        // No sequencer-specific metadata anywhere.
        let mut has_metadata = false;
        for track in &smf.tracks {
            for event in track {
                if let TrackEventKind::Meta(MetaMessage::SequencerSpecific(data)) = event.kind {
                    if parse_metadata_chunk(data).is_some() {
                        has_metadata = true;
                    }
                }
            }
        }
        assert!(!has_metadata);
        // The trigger macro is rendered into an ordinary note.
        let mut keys = Vec::new();
        for track in &smf.tracks {
            for event in track {
                if let TrackEventKind::Midi {
                    message: MidiMessage::NoteOn { key, .. },
                    ..
                } = event.kind
                {
                    keys.push(key.as_int());
                }
            }
        }
        assert!(keys.contains(&60));
        assert!(keys.contains(&72));
    }

    #[test]
    fn import_preserves_timing_and_splits_colliding_tracks() {
        // Build a standard SMF with two simultaneous notes in separate tracks
        // and an off-grid note using raw tick 5.
        let ppq = 24u16;
        let mut audio: Vec<(Ticks, Event)> = vec![
            (0, note(60, 0)),
            (0, note(64, 1)),
            (5, note(67, 0)),
            (24, Event::Meta(MetaEvent::Tempo(500_000))),
        ];
        audio.sort_by_key(|(tick, _)| *tick);
        let bytes = write_smf(ppq, &[audio]).unwrap();

        let loaded = load(&bytes).unwrap();
        assert!(!loaded.was_project);
        let composition = &loaded.project.composition;
        assert_eq!(composition.tpb(), 24);
        // Import must not silently quantize: re-render and compare ticks.
        let rendered = render_events(composition, None);
        let mut ticks: Vec<(Ticks, Event)> = rendered
            .into_iter()
            .filter(|t| !matches!(t.event, Event::Meta(MetaEvent::EndOfTrack)))
            .map(|t| (t.tick, t.event))
            .collect();
        ticks.sort_by_key(|(tick, _)| *tick);
        let mut expected: Vec<(Ticks, Event)> = vec![
            (0, note(60, 0)),
            (0, note(64, 1)),
            (5, note(67, 0)),
            (24, Event::Meta(MetaEvent::Tempo(500_000))),
        ];
        expected.sort_by_key(|(tick, _)| *tick);
        assert_eq!(ticks, expected);
        assert_eq!(composition.pattern(PatternId(0)).unwrap().lpb(), 24);
        assert_eq!(composition.pattern(PatternId(0)).unwrap().track_count(), 2);
    }

    #[test]
    fn round_trip_playback_equivalence() {
        let bytes = export_standard(&sample_project().composition, None).unwrap();
        let imported = load(&bytes).unwrap();
        let reexported = export_standard(&imported.project.composition, None).unwrap();

        let collect = |bytes: &[u8]| -> Vec<(Ticks, Event)> {
            let smf = Smf::parse(bytes).unwrap();
            let mut out = Vec::new();
            for track in &smf.tracks {
                let mut tick = 0u64;
                for event in track {
                    tick += u64::from(event.delta.as_int());
                    match &event.kind {
                        TrackEventKind::Midi { channel, message } => {
                            out.push((tick, channel_from_midi(*channel, *message)));
                        }
                        TrackEventKind::Meta(MetaMessage::Tempo(t)) => {
                            out.push((tick, Event::Meta(MetaEvent::Tempo(t.as_int()))));
                        }
                        _ => {}
                    }
                }
            }
            out.sort_by_key(|(tick, _)| *tick);
            out
        };
        assert_eq!(collect(&bytes), collect(&reexported));
    }
}
