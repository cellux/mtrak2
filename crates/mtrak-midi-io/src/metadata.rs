//! Versioned tracker project metadata.
//!
//! Authoritative tracker state is serialized into a sectioned blob and carried
//! in sequencer-specific meta-events (`FF 7F`). Large blobs are split into
//! numbered chunks so no single meta-event grows without bound (decision
//! D-024). A distinct marker identifies the rendered ordinary-MIDI fallback so
//! the loader can discard it when metadata is present.

use std::collections::BTreeMap;

use mtrak_core::composition::{Composition, Pattern, PatternId};
use mtrak_core::event::{ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};
use mtrak_core::modules::{default_modules, MODULE_TEMPLATE};
use mtrak_core::settings::{Scale, ScaleRegistry, Settings};
use mtrak_core::timing::Bpm;

/// Magic prefix shared by metadata chunks and the fallback marker.
pub const MAGIC: &[u8] = b"MTRAK";
/// Sequencer-specific kind byte identifying a metadata chunk.
pub const KIND_METADATA: u8 = 1;
/// Sequencer-specific kind byte identifying the rendered fallback marker.
pub const KIND_FALLBACK: u8 = 2;
/// Current metadata format version.
pub const METADATA_VERSION: u8 = 1;
/// Largest payload carried by a single metadata chunk.
pub const MAX_CHUNK_PAYLOAD: usize = 60_000;

const TAG_COMPOSITION: u8 = 0x01;
const TAG_GLOBAL_SETTINGS: u8 = 0x02;
const TAG_SCALES: u8 = 0x03;
const TAG_PATTERNS: u8 = 0x04;
const TAG_LUA: u8 = 0x05;

/// A tracker project: the composition plus its Lua module sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackerProject {
    /// The composition itself.
    pub composition: Composition,
    /// Authored Lua modules keyed by ID: module `00` plus user modules
    /// (`40`–`7F`). Built-in modules (`01`–`3F`) are not stored here.
    pub modules: BTreeMap<u8, String>,
}

impl TrackerProject {
    /// A new project around `composition`, containing only the default module
    /// `00` template.
    pub fn new(composition: Composition) -> Self {
        TrackerProject {
            composition,
            modules: default_modules(),
        }
    }

    /// Ensures module `00` exists, initializing it from the minimal template.
    pub fn ensure_setup_module(&mut self) {
        self.modules
            .entry(0)
            .or_insert_with(|| MODULE_TEMPLATE.to_string());
    }
}

/// Errors from metadata encoding and decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    /// The input ended before a value was complete.
    #[error("unexpected end of metadata input")]
    UnexpectedEof,
    /// A tag or code was not recognized.
    #[error("invalid metadata value: {0}")]
    Invalid(String),
    /// The metadata version is newer than this build understands.
    #[error("unsupported metadata version {0}")]
    UnsupportedVersion(u8),
    /// Chunks were missing, duplicated, or inconsistent.
    #[error("inconsistent metadata chunks: {0}")]
    Chunking(String),
    /// The composition could not be reconstructed.
    #[error("invalid composition: {0}")]
    Composition(String),
}

/// One metadata chunk extracted from a `FF 7F` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataChunk {
    /// Format version.
    pub version: u8,
    /// Zero-based chunk index.
    pub index: u16,
    /// Total number of chunks.
    pub count: u16,
    /// The chunk payload.
    pub payload: Vec<u8>,
}

/// Parses a sequencer-specific payload as a metadata chunk, if it is one.
pub fn parse_metadata_chunk(data: &[u8]) -> Option<MetadataChunk> {
    let rest = data.strip_prefix(MAGIC)?;
    if rest.first() != Some(&KIND_METADATA) {
        return None;
    }
    if rest.len() < 6 {
        return None;
    }
    Some(MetadataChunk {
        version: rest[1],
        index: u16::from_be_bytes([rest[2], rest[3]]),
        count: u16::from_be_bytes([rest[4], rest[5]]),
        payload: rest[6..].to_vec(),
    })
}

/// The sequencer-specific payload marking the rendered fallback.
pub fn fallback_marker() -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(KIND_FALLBACK);
    out
}

/// Whether a sequencer-specific payload is the fallback marker.
pub fn is_fallback_marker(data: &[u8]) -> bool {
    data == fallback_marker().as_slice()
}

/// Reassembles chunk payloads into the metadata blob.
pub fn assemble_chunks(chunks: &[MetadataChunk]) -> Result<Vec<u8>, MetadataError> {
    let Some(first) = chunks.first() else {
        return Err(MetadataError::Chunking("no chunks".to_string()));
    };
    let count = first.count;
    if count == 0 || chunks.len() != count as usize {
        return Err(MetadataError::Chunking(format!(
            "expected {count} chunks, found {}",
            chunks.len()
        )));
    }
    if first.version != METADATA_VERSION {
        return Err(MetadataError::UnsupportedVersion(first.version));
    }
    let mut ordered: Vec<Option<&MetadataChunk>> = vec![None; count as usize];
    for chunk in chunks {
        if chunk.version != first.version {
            return Err(MetadataError::Chunking("mixed versions".to_string()));
        }
        let slot = ordered
            .get_mut(chunk.index as usize)
            .ok_or_else(|| MetadataError::Chunking(format!("bad index {}", chunk.index)))?;
        if slot.is_some() {
            return Err(MetadataError::Chunking(format!(
                "duplicate index {}",
                chunk.index
            )));
        }
        *slot = Some(chunk);
    }
    let mut blob = Vec::new();
    for (i, slot) in ordered.iter().enumerate() {
        let chunk = slot.ok_or_else(|| MetadataError::Chunking(format!("missing index {i}")))?;
        blob.extend_from_slice(&chunk.payload);
    }
    Ok(blob)
}

/// Splits a metadata blob into numbered chunks.
pub fn encode_chunks(blob: &[u8]) -> Vec<Vec<u8>> {
    let count = blob.len().div_ceil(MAX_CHUNK_PAYLOAD).max(1);
    (0..count)
        .map(|i| {
            let start = i * MAX_CHUNK_PAYLOAD;
            let end = (start + MAX_CHUNK_PAYLOAD).min(blob.len());
            let mut w = Writer::new();
            w.bytes(MAGIC);
            w.u8(KIND_METADATA);
            w.u8(METADATA_VERSION);
            w.u16(i as u16);
            w.u16(count as u16);
            w.bytes(&blob[start..end]);
            w.into_vec()
        })
        .collect()
}

/// Encodes a whole project into a metadata blob.
pub fn encode_project(project: &TrackerProject) -> Vec<u8> {
    let mut w = Writer::new();
    section(&mut w, TAG_COMPOSITION, |w| {
        w.u32(project.composition.bpm().q8_8());
        w.u32(project.composition.tpb());
        w.u8(project.composition.selected_main().value());
        // Trailing and optional: blobs written before the seed existed end
        // after the u8, and a reader that ignores the remainder still loads
        // them.
        w.u64(project.composition.seed());
    });
    section(&mut w, TAG_GLOBAL_SETTINGS, |w| {
        write_settings(w, project.composition.global_settings());
    });
    section(&mut w, TAG_SCALES, |w| {
        let user: Vec<(&str, Scale)> = project.composition.scales().iter().collect();
        w.u16(user.len() as u16);
        for (name, scale) in user {
            w.blob(name.as_bytes());
            w.u16(scale.mask());
        }
    });
    section(&mut w, TAG_PATTERNS, |w| {
        let ids = project.composition.pattern_ids();
        w.u16(ids.len() as u16);
        for id in ids {
            let pattern = project
                .composition
                .pattern(id)
                .expect("listed pattern exists");
            write_pattern(w, pattern);
        }
    });
    section(&mut w, TAG_LUA, |w| {
        w.u16(project.modules.len() as u16);
        for (id, source) in &project.modules {
            w.u8(*id);
            w.blob(source.as_bytes());
        }
    });
    w.into_vec()
}

/// Decodes a metadata blob into a project.
pub fn decode_project(blob: &[u8]) -> Result<TrackerProject, MetadataError> {
    let mut r = Reader::new(blob);
    let mut bpm = Bpm::DEFAULT;
    let mut tpb = 24u32;
    let mut seed = 0u64;
    let mut selected = PatternId(0);
    let mut global_settings = Settings::new();
    let mut registry = ScaleRegistry::new();
    let mut patterns: Vec<Pattern> = Vec::new();
    let mut modules: BTreeMap<u8, String> = default_modules();

    while !r.is_done() {
        let tag = r.u8()?;
        let body = r.blob()?;
        let mut s = Reader::new(body);
        match tag {
            TAG_COMPOSITION => {
                bpm = Bpm::from_q8_8(s.u32()?)
                    .ok_or_else(|| MetadataError::Invalid("tempo".to_string()))?;
                tpb = s.u32()?;
                selected = PatternId(s.u8()?);
                // Optional trailing seed.
                if !s.is_done() {
                    seed = s.u64()?;
                }
            }
            TAG_GLOBAL_SETTINGS => {
                global_settings = read_settings(&mut s)?;
            }
            TAG_SCALES => {
                let count = s.u16()? as usize;
                for _ in 0..count {
                    let name = String::from_utf8(s.blob()?.to_vec())
                        .map_err(|_| MetadataError::Invalid("scale name".to_string()))?;
                    let mask = s.u16()?;
                    registry.add(&name, Scale::from_mask(mask));
                }
            }
            TAG_PATTERNS => {
                let count = s.u16()? as usize;
                for _ in 0..count {
                    patterns.push(read_pattern(&mut s)?);
                }
            }
            TAG_LUA => {
                let count = s.u16()? as usize;
                for _ in 0..count {
                    let id = s.u8()?;
                    let source = String::from_utf8(s.blob()?.to_vec())
                        .map_err(|_| MetadataError::Invalid("lua source".to_string()))?;
                    modules.insert(id, source);
                }
            }
            _ => {
                return Err(MetadataError::Invalid(format!(
                    "unknown section {tag:#04X}"
                )))
            }
        }
    }

    let mut composition = Composition::empty();
    composition.set_bpm(bpm);
    composition.set_seed(seed);
    composition.set_tpb_raw(tpb).map_err(comp_err)?;
    *composition.global_settings_mut() = global_settings;
    *composition.scales_mut() = registry;
    for pattern in patterns {
        composition.place_pattern(pattern).map_err(comp_err)?;
    }
    composition.set_selected_main_raw(selected);
    if composition.pattern(selected).is_none() {
        return Err(MetadataError::Composition(format!(
            "selected pattern {selected} is missing"
        )));
    }

    Ok(TrackerProject {
        composition,
        modules,
    })
}

fn comp_err(error: mtrak_core::composition::CompositionError) -> MetadataError {
    MetadataError::Composition(error.to_string())
}

fn section(w: &mut Writer, tag: u8, body: impl FnOnce(&mut Writer)) {
    let mut inner = Writer::new();
    body(&mut inner);
    w.u8(tag);
    w.blob(&inner.into_vec());
}

fn write_settings(w: &mut Writer, settings: &Settings) {
    w.u8(settings.root);
    w.u16(settings.scale.mask());
    w.u8(settings.modal_degree);
}

fn read_settings(r: &mut Reader<'_>) -> Result<Settings, MetadataError> {
    Ok(Settings {
        root: r.u8()?,
        scale: Scale::from_mask(r.u16()?),
        modal_degree: r.u8()?,
    })
}

fn write_pattern(w: &mut Writer, pattern: &Pattern) {
    w.u8(pattern.id().value());
    w.u32(pattern.lpb());
    w.u32(pattern.row_count() as u32);
    w.u32(pattern.track_count() as u32);
    match pattern.name() {
        Some(name) => {
            w.u8(1);
            w.blob(name.as_bytes());
        }
        None => w.u8(0),
    }
    for row in 0..pattern.row_count() {
        for track in 0..pattern.track_count() {
            let slot = pattern.slot(row, track).expect("in-range slot");
            write_event(w, &slot.event);
            w.u32(slot.offset);
        }
    }
}

fn read_pattern(r: &mut Reader<'_>) -> Result<Pattern, MetadataError> {
    let id = PatternId(r.u8()?);
    let lpb = r.u32()?;
    let rows = r.u32()? as usize;
    let tracks = r.u32()? as usize;
    let name = if r.u8()? == 1 {
        Some(
            String::from_utf8(r.blob()?.to_vec())
                .map_err(|_| MetadataError::Invalid("pattern name".to_string()))?,
        )
    } else {
        None
    };
    let mut pattern = Pattern::new(id, lpb, rows, tracks);
    pattern.set_name(name);
    for row in 0..rows {
        for track in 0..tracks {
            let event = read_event(r)?;
            let offset = r.u32()?;
            let slot = pattern.slot_mut(row, track).expect("in-range slot");
            slot.event = event;
            slot.offset = offset;
        }
    }
    Ok(pattern)
}

fn write_event(w: &mut Writer, event: &Event) {
    match event {
        Event::Empty => w.u8(0),
        Event::Channel(message) => {
            w.u8(1);
            w.u8(message.kind.status_nibble() - 0x8);
            let mut flags = 0u8;
            if message.channel.is_some() {
                flags |= 1;
            }
            if message.data1.is_some() {
                flags |= 2;
            }
            if message.data2.is_some() {
                flags |= 4;
            }
            w.u8(flags);
            if let Some(channel) = message.channel {
                w.u8(channel);
            }
            if let Some(data1) = message.data1 {
                w.u8(data1);
            }
            if let Some(data2) = message.data2 {
                w.u8(data2);
            }
        }
        Event::SysEx(data) => {
            w.u8(2);
            w.blob(data);
        }
        Event::Meta(meta) => {
            w.u8(3);
            w.u8(meta.type_byte());
            w.blob(&meta.data_bytes());
        }
        Event::Macro(call) => {
            w.u8(4);
            w.u8(call.id);
            w.u8(call.arg1);
            w.u8(call.arg2);
        }
        Event::SmfEscape(data) => {
            w.u8(5);
            w.blob(data);
        }
        Event::Wire(data) => {
            w.u8(6);
            w.blob(data);
        }
    }
}

fn read_event(r: &mut Reader<'_>) -> Result<Event, MetadataError> {
    match r.u8()? {
        0 => Ok(Event::Empty),
        1 => {
            let kind = ChannelKind::from_status_nibble(r.u8()? + 0x8)
                .ok_or_else(|| MetadataError::Invalid("channel kind".to_string()))?;
            let flags = r.u8()?;
            let channel = if flags & 1 != 0 { Some(r.u8()?) } else { None };
            let data1 = if flags & 2 != 0 { Some(r.u8()?) } else { None };
            let data2 = if flags & 4 != 0 { Some(r.u8()?) } else { None };
            Ok(Event::Channel(ChannelMessage {
                kind,
                channel,
                data1,
                data2,
            }))
        }
        2 => Ok(Event::SysEx(r.blob()?.to_vec())),
        3 => {
            let type_byte = r.u8()?;
            let data = r.blob()?.to_vec();
            Ok(Event::Meta(MetaEvent::from_parts(type_byte, data)))
        }
        4 => {
            let id = r.u8()?;
            let arg1 = r.u8()?;
            let arg2 = r.u8()?;
            Ok(Event::Macro(MacroCall::new(id, arg1, arg2)))
        }
        5 => Ok(Event::SmfEscape(r.blob()?.to_vec())),
        6 => Ok(Event::Wire(r.blob()?.to_vec())),
        other => Err(MetadataError::Invalid(format!("event tag {other}"))),
    }
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.buf.extend_from_slice(value);
    }

    fn blob(&mut self, value: &[u8]) {
        self.u32(value.len() as u32);
        self.bytes(value);
    }

    fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn is_done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], MetadataError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.buf.len())
            .ok_or(MetadataError::UnexpectedEof)?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, MetadataError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, MetadataError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, MetadataError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, MetadataError> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn blob(&mut self) -> Result<&'a [u8], MetadataError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::event::TextKind;

    fn sample_project() -> TrackerProject {
        let mut composition = Composition::new();
        {
            let pattern = composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(4).unwrap();
            pattern.resize_tracks(2).unwrap();
            pattern.set_event(
                0,
                0,
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    3,
                    60,
                    Some(100),
                )),
            );
            pattern.set_event(
                1,
                0,
                Event::Channel(ChannelMessage {
                    kind: ChannelKind::ControlChange,
                    channel: Some(0),
                    data1: Some(7),
                    data2: Some(0),
                }),
            );
            pattern.set_event(2, 1, Event::Meta(MetaEvent::Tempo(400_000)));
            pattern.set_event(
                3,
                1,
                Event::Meta(MetaEvent::text(TextKind::Marker, b"end".to_vec())),
            );
            pattern.slot_mut(0, 1).unwrap().offset = 3;
            pattern.slot_mut(2, 1).unwrap().event = Event::SysEx(vec![0x7D, 1, 2]);
        }
        let extra = composition.create_pattern(5).unwrap();
        composition.pattern_mut(extra).unwrap().set_event(
            0,
            0,
            Event::Macro(MacroCall::new(0x40, 1, 2)),
        );
        composition
            .scales_mut()
            .add("weird", Scale::parse_mask("101001010100").unwrap());
        let mut modules = default_modules();
        modules.insert(0x40, "return { run = function() end }".to_string());
        modules.insert(0, "register_command('x')".to_string());
        TrackerProject {
            composition,
            modules,
        }
    }

    #[test]
    fn project_blob_round_trips() {
        let project = sample_project();
        let blob = encode_project(&project);
        let decoded = decode_project(&blob).unwrap();
        assert_eq!(decoded, project);
    }

    #[test]
    fn chunking_reassembles_large_blobs() {
        let project = sample_project();
        let mut blob = encode_project(&project);
        blob.resize(MAX_CHUNK_PAYLOAD * 2 + 17, 0xAB);
        let chunks: Vec<Vec<u8>> = encode_chunks(&blob);
        assert_eq!(chunks.len(), 3);
        let parsed: Vec<MetadataChunk> = chunks
            .iter()
            .filter_map(|c| parse_metadata_chunk(c))
            .collect();
        assert_eq!(parsed.len(), 3);
        let reassembled = assemble_chunks(&parsed).unwrap();
        assert_eq!(reassembled, blob);
    }

    #[test]
    fn fallback_marker_is_recognizable() {
        let marker = fallback_marker();
        assert!(is_fallback_marker(&marker));
        assert!(parse_metadata_chunk(&marker).is_none());
        assert!(!is_fallback_marker(MAGIC));
    }

    #[test]
    fn corrupt_chunks_are_rejected() {
        let project = sample_project();
        let blob = encode_project(&project);
        let chunks: Vec<MetadataChunk> = encode_chunks(&blob)
            .iter()
            .filter_map(|c| parse_metadata_chunk(c))
            .collect();
        // Drop a chunk.
        let mut fewer = chunks.clone();
        fewer.pop();
        assert!(assemble_chunks(&fewer).is_err());
        // Unknown version.
        let mut bad = chunks.clone();
        bad[0].version = 99;
        assert!(matches!(
            assemble_chunks(&bad),
            Err(MetadataError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn truncated_blob_is_rejected() {
        let project = sample_project();
        let blob = encode_project(&project);
        assert!(decode_project(&blob[..blob.len() / 2]).is_err());
    }

    #[test]
    fn blobs_written_before_the_seed_load_with_seed_zero() {
        let mut project = sample_project();
        project.composition.set_seed(0x1234_5678_9ABC_DEF0);
        let mut blob = encode_project(&project);
        // The composition section comes first: tag, u32 length, then the body.
        // Strip the trailing seed so the blob matches one written before it
        // existed, and shrink the length prefix to match.
        assert_eq!(blob[0], TAG_COMPOSITION);
        let len = u32::from_be_bytes([blob[1], blob[2], blob[3], blob[4]]) as usize;
        let seed_start = 5 + len - 8;
        blob.drain(seed_start..seed_start + 8);
        blob[1..5].copy_from_slice(&((len - 8) as u32).to_be_bytes());

        let loaded = decode_project(&blob).unwrap();
        assert_eq!(loaded.composition.seed(), 0, "old blobs default the seed");
    }
}
