//! Structured Lua tables for mtrak events.

use mlua::{Lua, MultiValue, Table, Value};

use mtrak_core::event::{ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};

/// Converts an event to a structured Lua table.
pub fn event_to_lua(lua: &Lua, event: &Event) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    match event {
        Event::Empty => {
            table.set("type", "empty")?;
        }
        Event::Channel(message) => {
            table.set("type", "channel")?;
            table.set("kind", channel_kind_name(message.kind))?;
            set_optional(table.clone(), "channel", message.channel)?;
            set_optional(table.clone(), "data1", message.data1)?;
            set_optional(table.clone(), "data2", message.data2)?;
        }
        Event::Macro(call) => {
            table.set("type", "macro")?;
            table.set("id", call.id)?;
            table.set("arg1", call.arg1)?;
            table.set("arg2", call.arg2)?;
        }
        Event::SysEx(data) => {
            table.set("type", "sysex")?;
            table.set("data", bytes_to_table(lua, data)?)?;
        }
        Event::Meta(meta) => {
            table.set("type", "meta")?;
            table.set("type-byte", meta.type_byte())?;
            table.set("data", bytes_to_table(lua, &meta.data_bytes())?)?;
        }
        Event::SmfEscape(data) => {
            table.set("type", "escape")?;
            table.set("data", bytes_to_table(lua, data)?)?;
        }
        Event::Wire(data) => {
            table.set("type", "wire")?;
            table.set("data", bytes_to_table(lua, data)?)?;
        }
    }
    Ok(table)
}

/// Converts a structured Lua table back to an event.
pub fn event_from_lua(value: Value) -> mlua::Result<Event> {
    let table = match value {
        Value::Table(table) => table,
        Value::Nil => return Ok(Event::Empty),
        _ => {
            return Err(mlua::Error::RuntimeError(
                "event must be a table or nil".to_string(),
            ))
        }
    };
    let kind: String = table.get("type").unwrap_or_default();
    match kind.as_str() {
        "empty" => Ok(Event::Empty),
        "channel" => {
            let kind_name: String = table.get("kind")?;
            let kind = channel_kind_from_name(&kind_name).ok_or_else(|| {
                mlua::Error::RuntimeError(format!("unknown channel kind {kind_name}"))
            })?;
            let channel = table.get::<Option<u8>>("channel")?;
            let data1 = table.get::<Option<u8>>("data1")?;
            let data2 = table.get::<Option<u8>>("data2")?;
            let message = ChannelMessage {
                kind,
                channel,
                data1,
                data2,
            };
            validate_channel_message(&message)?;
            Ok(Event::Channel(message))
        }
        "macro" => {
            let id = table.get::<u8>("id")?;
            let arg1 = table.get::<u8>("arg1")?;
            let arg2 = table.get::<u8>("arg2")?;
            Ok(Event::Macro(MacroCall { id, arg1, arg2 }))
        }
        "sysex" => Ok(Event::SysEx(table_to_bytes(table.get("data")?)?)),
        "meta" => {
            let type_byte = table.get::<u8>("type-byte")?;
            let data = table_to_bytes(table.get("data")?)?;
            Ok(Event::Meta(MetaEvent::from_parts(type_byte, data)))
        }
        "escape" => Ok(Event::SmfEscape(table_to_bytes(table.get("data")?)?)),
        "wire" => Ok(Event::Wire(table_to_bytes(table.get("data")?)?)),
        other => Err(mlua::Error::RuntimeError(format!(
            "unknown event type {other}"
        ))),
    }
}

fn set_optional(table: Table, key: &str, value: Option<u8>) -> mlua::Result<()> {
    match value {
        Some(value) => table.set(key, value),
        None => table.set(key, Value::Nil),
    }
}

fn bytes_to_table(lua: &Lua, data: &[u8]) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for (index, byte) in data.iter().enumerate() {
        table.set(index + 1, *byte)?;
    }
    Ok(table)
}

fn table_to_bytes(value: Value) -> mlua::Result<Vec<u8>> {
    match value {
        Value::Table(table) => {
            let mut bytes = Vec::new();
            for pair in table.sequence_values::<u8>() {
                bytes.push(pair?);
            }
            Ok(bytes)
        }
        Value::Nil => Ok(Vec::new()),
        _ => Err(mlua::Error::RuntimeError(
            "data must be a sequence of bytes".to_string(),
        )),
    }
}

fn channel_kind_name(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::NoteOff => "note-off",
        ChannelKind::NoteOn => "note-on",
        ChannelKind::PolyAftertouch => "poly-aftertouch",
        ChannelKind::ControlChange => "control-change",
        ChannelKind::ProgramChange => "program-change",
        ChannelKind::ChannelAftertouch => "channel-aftertouch",
        ChannelKind::PitchBend => "pitch-bend",
    }
}

fn channel_kind_from_name(name: &str) -> Option<ChannelKind> {
    match name {
        "note-off" => Some(ChannelKind::NoteOff),
        "note-on" => Some(ChannelKind::NoteOn),
        "poly-aftertouch" => Some(ChannelKind::PolyAftertouch),
        "control-change" => Some(ChannelKind::ControlChange),
        "program-change" => Some(ChannelKind::ProgramChange),
        "channel-aftertouch" => Some(ChannelKind::ChannelAftertouch),
        "pitch-bend" => Some(ChannelKind::PitchBend),
        _ => None,
    }
}

/// Validates MIDI channel and data ranges for a channel message.
fn validate_channel_message(message: &ChannelMessage) -> mlua::Result<()> {
    if let Some(channel) = message.channel {
        if channel > 15 {
            return Err(mlua::Error::RuntimeError(format!(
                "MIDI channel {channel} is out of range (0-15)"
            )));
        }
    }
    for (name, value) in [("data1", message.data1), ("data2", message.data2)] {
        if let Some(value) = value {
            if value > 127 {
                return Err(mlua::Error::RuntimeError(format!(
                    "{name} {value} is out of range (0-127)"
                )));
            }
        }
    }
    Ok(())
}

fn channel_event(
    lua: &Lua,
    kind: ChannelKind,
    channel: u8,
    data1: u8,
    data2: Option<u8>,
) -> mlua::Result<Table> {
    let message = ChannelMessage {
        kind,
        channel: Some(channel),
        data1: Some(data1),
        data2,
    };
    validate_channel_message(&message)?;
    event_to_lua(lua, &Event::Channel(message))
}

/// Installs the `mtrak.events` constructors.
pub fn install_events(lua: &Lua, mtrak: &Table) -> mlua::Result<()> {
    let events = lua.create_table()?;
    events.set(
        "empty",
        lua.create_function(|lua, ()| event_to_lua(lua, &Event::Empty))?,
    )?;
    events.set(
        "note-on",
        lua.create_function(|lua, (channel, note, velocity): (u8, u8, u8)| {
            channel_event(lua, ChannelKind::NoteOn, channel, note, Some(velocity))
        })?,
    )?;
    events.set(
        "note-off",
        lua.create_function(|lua, (channel, note, velocity): (u8, u8, u8)| {
            channel_event(lua, ChannelKind::NoteOff, channel, note, Some(velocity))
        })?,
    )?;
    events.set(
        "control-change",
        lua.create_function(|lua, (channel, controller, value): (u8, u8, u8)| {
            channel_event(
                lua,
                ChannelKind::ControlChange,
                channel,
                controller,
                Some(value),
            )
        })?,
    )?;
    events.set(
        "program-change",
        lua.create_function(|lua, (channel, program): (u8, u8)| {
            channel_event(lua, ChannelKind::ProgramChange, channel, program, None)
        })?,
    )?;
    events.set(
        "sysex",
        lua.create_function(|lua, data: MultiValue| {
            let mut bytes = Vec::new();
            for value in data {
                let byte = value.as_integer().ok_or_else(|| {
                    mlua::Error::RuntimeError("sysex bytes must be integers".to_string())
                })?;
                bytes.push(u8::try_from(byte).map_err(|_| {
                    mlua::Error::RuntimeError(format!("sysex byte {byte} is out of range (0-255)"))
                })?);
            }
            event_to_lua(lua, &Event::SysEx(bytes))
        })?,
    )?;
    events.set(
        "macro",
        lua.create_function(|lua, (id, arg1, arg2): (u8, u8, u8)| {
            event_to_lua(lua, &Event::Macro(MacroCall { id, arg1, arg2 }))
        })?,
    )?;
    mtrak.set("events", events)?;
    Ok(())
}
