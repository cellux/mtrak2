//! The `mtrak` REPL API installed into the per-evaluation environment.

use std::cell::RefCell;

use mlua::{Lua, Scope, Table, Value};

use mtrak_core::composition::PatternId;
use mtrak_core::event::{ChannelKind, ChannelMessage, Event};
use mtrak_core::settings::Scale;
use mtrak_engine::fennel::FennelCompiler;
use mtrak_tui::App;

use crate::events::event_from_lua;

/// The curated standard-library globals shared with macro code.
pub(crate) const SAFE_GLOBALS: &[&str] = &[
    "string", "table", "math", "pairs", "ipairs", "next", "type", "tostring", "tonumber", "error",
    "assert", "select", "pcall", "xpcall",
];

/// Macro-only names that must be allowed as quoted-form symbols but rejected
/// when called, because the REPL does not run as a macro coroutine.
pub(crate) const REJECTED_GLOBALS: &[&str] = &["emit", "wait", "trigger", "stop-track"];

/// Installs the REPL API into `api`, whose `__index` is the persistent globals
/// table.
pub fn install_api<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    api: &Table,
    globals: &'scope Table,
    app: &'scope RefCell<&'env mut App>,
    compiler: &'env FennelCompiler,
) -> mlua::Result<()> {
    for name in SAFE_GLOBALS {
        let value: Value = lua.globals().get(*name)?;
        api.set(*name, value)?;
    }
    for name in REJECTED_GLOBALS {
        let name = *name;
        let reject = scope.create_function(move |_, _: mlua::MultiValue| {
            Err::<(), _>(mlua::Error::RuntimeError(format!(
                "{name} is not available in the REPL"
            )))
        })?;
        // Fennel rewrites globals that are not valid Lua identifiers, so install
        // the handler under both the raw and the mangled name.
        api.set(name, reject.clone())?;
        api.set(fennel_global_name(name), reject)?;
    }
    let mtrak = lua.create_table()?;
    install_patterns(scope, lua, &mtrak, app)?;
    install_macros(scope, lua, &mtrak, app, compiler)?;
    install_settings(scope, lua, &mtrak, app)?;
    install_project(scope, lua, &mtrak, app)?;
    install_repl(scope, lua, &mtrak, globals)?;
    crate::events::install_events(lua, &mtrak)?;
    api.set("mtrak", mtrak)?;
    Ok(())
}

fn install_patterns<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    mtrak: &Table,
    app: &'scope RefCell<&'env mut App>,
) -> mlua::Result<()> {
    let patterns = lua.create_table()?;

    patterns.set(
        "list",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app
                .project()
                .composition
                .pattern_ids()
                .into_iter()
                .map(|id| id.value())
                .collect::<Vec<u8>>())
        })?,
    )?;

    patterns.set(
        "get",
        scope.create_function(|lua, id: Value| {
            let id = value_to_byte(id, "pattern id")?;
            let app = app.borrow();
            let pattern = app
                .project()
                .composition
                .pattern(PatternId(id))
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("pattern {id:02X} does not exist"))
                })?;
            let table = lua.create_table()?;
            table.set("id", pattern.id().value())?;
            table.set("name", pattern.name().map(str::to_string))?;
            table.set("lpb", pattern.lpb())?;
            table.set("rows", pattern.row_count())?;
            table.set("tracks", pattern.track_count())?;
            Ok(table)
        })?,
    )?;

    patterns.set(
        "create",
        scope.create_function(|_, (id, opts): (Value, Option<Table>)| {
            let id = value_to_byte(id, "pattern id")?;
            let mut app = app.borrow_mut();
            // Reject malformed geometry options instead of silently falling
            // back to defaults.
            let (lpb, rows, tracks) = match opts {
                Some(table) => (
                    table_u32(&table, "lpb")?.unwrap_or(4),
                    table_usize(&table, "rows")?.unwrap_or(64),
                    table_usize(&table, "tracks")?.unwrap_or(16),
                ),
                None => (4, 64, 16),
            };
            app.repl_project_mut()
                .composition
                .create_pattern_at(PatternId(id), lpb)
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            // Validate the requested geometry on a clone so a rejected size
            // cannot leave a half-created pattern behind.
            let geometry = (|| -> Result<(), String> {
                let pattern = app
                    .repl_project_mut()
                    .composition
                    .pattern_mut(PatternId(id))
                    .ok_or_else(|| format!("pattern {id:02X} was not created"))?;
                let mut candidate = pattern.clone();
                candidate
                    .resize_rows(rows)
                    .map_err(|error| error.to_string())?;
                candidate
                    .resize_tracks(tracks)
                    .map_err(|error| error.to_string())?;
                *pattern = candidate;
                Ok(())
            })();
            if let Err(error) = geometry {
                let _ = app
                    .repl_project_mut()
                    .composition
                    .delete_pattern(PatternId(id));
                return Err(mlua::Error::RuntimeError(error));
            }
            app.repl_after_pattern_change();
            Ok(())
        })?,
    )?;

    patterns.set(
        "delete",
        scope.create_function(|_, id: Value| {
            let id = value_to_byte(id, "pattern id")?;
            let mut app = app.borrow_mut();
            if app.project().composition.selected_main() == PatternId(id) {
                return Err(mlua::Error::RuntimeError(format!(
                    "pattern {id:02X} is the selected main and cannot be deleted"
                )));
            }
            app.repl_project_mut()
                .composition
                .delete_pattern(PatternId(id))
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            app.repl_after_pattern_change();
            Ok(())
        })?,
    )?;

    patterns.set(
        "set-name",
        scope.create_function(|_, (id, name): (Value, Option<String>)| {
            let id = value_to_byte(id, "pattern id")?;
            let mut app = app.borrow_mut();
            let pattern = app
                .repl_project_mut()
                .composition
                .pattern_mut(PatternId(id))
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("pattern {id:02X} does not exist"))
                })?;
            pattern.set_name(name);
            app.repl_after_pattern_change();
            Ok(())
        })?,
    )?;

    patterns.set(
        "set-lpb",
        scope.create_function(|_, (id, lpb): (Value, Value)| {
            let id = value_to_byte(id, "pattern id")?;
            let lpb = value_to_u32(lpb, "lpb")?;
            let mut app = app.borrow_mut();
            app.repl_project_mut()
                .composition
                .set_pattern_lpb(PatternId(id), lpb)
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            app.repl_after_pattern_change();
            Ok(())
        })?,
    )?;

    patterns.set(
        "resize",
        scope.create_function(|_, (id, opts): (Value, Table)| {
            let id = value_to_byte(id, "pattern id")?;
            let mut app = app.borrow_mut();
            let rows = table_usize(&opts, "rows")?;
            let tracks = table_usize(&opts, "tracks")?;
            // Apply both axes to a clone so a rejected size leaves the original
            // pattern untouched.
            let pattern = app
                .repl_project_mut()
                .composition
                .pattern_mut(PatternId(id))
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("pattern {id:02X} does not exist"))
                })?;
            let mut candidate = pattern.clone();
            if let Some(rows) = rows {
                candidate
                    .resize_rows(rows)
                    .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            }
            if let Some(tracks) = tracks {
                candidate
                    .resize_tracks(tracks)
                    .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            }
            *pattern = candidate;
            app.repl_after_pattern_change();
            Ok(())
        })?,
    )?;

    patterns.set(
        "get-slot",
        scope.create_function(|lua, (id, row, track): (Value, Value, Value)| {
            let id = value_to_byte(id, "pattern id")?;
            let row = value_to_usize(row, "row")?;
            let track = value_to_usize(track, "track")?;
            let app = app.borrow();
            let pattern = app
                .project()
                .composition
                .pattern(PatternId(id))
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(format!("pattern {id:02X} does not exist"))
                })?;
            let slot = pattern.slot(row, track).ok_or_else(|| {
                mlua::Error::RuntimeError(format!("slot {row}:{track} is outside pattern {id:02X}"))
            })?;
            let table = lua.create_table()?;
            table.set("event", crate::events::event_to_lua(lua, &slot.event)?)?;
            table.set("offset", slot.offset)?;
            Ok(table)
        })?,
    )?;

    patterns.set(
        "set-slot",
        scope.create_function(
            |_, (id, row, track, event, offset): (Value, Value, Value, Value, Option<Value>)| {
                let id = value_to_byte(id, "pattern id")?;
                let row = value_to_usize(row, "row")?;
                let track = value_to_usize(track, "track")?;
                let offset = match offset {
                    Some(value) => Some(value_to_u32(value, "offset")?),
                    None => None,
                };
                let event = event_from_lua(event)?;
                let mut app = app.borrow_mut();
                let pattern = app
                    .repl_project_mut()
                    .composition
                    .pattern_mut(PatternId(id))
                    .ok_or_else(|| {
                        mlua::Error::RuntimeError(format!("pattern {id:02X} does not exist"))
                    })?;
                let slot = pattern.slot_mut(row, track).ok_or_else(|| {
                    mlua::Error::RuntimeError(format!(
                        "slot {row}:{track} is outside pattern {id:02X}"
                    ))
                })?;
                slot.event = event;
                slot.offset = offset.unwrap_or(0);
                app.repl_after_pattern_change();
                Ok(())
            },
        )?,
    )?;

    mtrak.set("patterns", patterns)?;
    Ok(())
}

/// Returns the name Fennel uses for a global in generated Lua code.
///
/// Valid Lua identifiers are left alone; anything else is prefixed with
/// `__fnl_global__` and has each non-alphanumeric byte escaped as `_XX`.
pub(crate) fn fennel_global_name(name: &str) -> String {
    if is_valid_lua_identifier(name) {
        return name.to_string();
    }
    let mut mangled = String::from("__fnl_global__");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() {
            mangled.push(byte as char);
        } else {
            mangled.push_str(&format!("_{byte:02x}"));
        }
    }
    mangled
}

/// Reverses [`fennel_global_name`] for names collected from session globals.
pub(crate) fn fennel_global_unmangle(name: &str) -> String {
    let Some(rest) = name.strip_prefix("__fnl_global__") else {
        return name.to_string();
    };
    let bytes = rest.as_bytes();
    let mut unmangled = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let is_escape = bytes[index] == b'_'
            && index + 2 < bytes.len()
            && is_lower_hex(bytes[index + 1])
            && is_lower_hex(bytes[index + 2]);
        if is_escape {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("00");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                unmangled.push(value as char);
                index += 3;
                continue;
            }
        }
        unmangled.push(bytes[index] as char);
        index += 1;
    }
    unmangled
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn is_valid_lua_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    if !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return false;
    }
    !matches!(
        name,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "goto"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    )
}

fn value_to_source_string(value: Value) -> mlua::Result<String> {
    match value {
        Value::String(text) => Ok(text.to_str()?.to_string()),
        _ => Err(mlua::Error::RuntimeError(
            "prefix and replacement must be Fennel source strings".to_string(),
        )),
    }
}

/// Rejects IDs that are read-only. The setup module `00` and user modules
/// `40`–`7F` are writable; built-ins `01`–`3F` are not.
fn ensure_writable_module(id: u8) -> mlua::Result<()> {
    if (0x01..=0x3F).contains(&id) {
        return Err(mlua::Error::RuntimeError(format!(
            "module {id:02X} is a built-in and cannot be modified"
        )));
    }
    Ok(())
}

fn install_macros<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    mtrak: &Table,
    app: &'scope RefCell<&'env mut App>,
    compiler: &'env FennelCompiler,
) -> mlua::Result<()> {
    let macros = lua.create_table()?;

    macros.set(
        "list",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project().modules.keys().copied().collect::<Vec<u8>>())
        })?,
    )?;

    macros.set(
        "get",
        scope.create_function(|_, id: Value| {
            let id = value_to_byte(id, "module id")?;
            let app = app.borrow();
            Ok(app.project().modules.get(&id).cloned())
        })?,
    )?;

    macros.set(
        "create",
        scope.create_function(|_, (id, source): (Value, String)| {
            let id = value_to_byte(id, "module id")?;
            let mut app = app.borrow_mut();
            if app.project().modules.contains_key(&id) {
                return Err(mlua::Error::RuntimeError(format!(
                    "module {id:02X} already exists"
                )));
            }
            if !(0x40..=0x7F).contains(&id) {
                return Err(mlua::Error::RuntimeError(format!(
                    "module {id:02X} is not a user module ID"
                )));
            }
            app.repl_apply_module_source(id, source.clone())
                .map_err(mlua::Error::RuntimeError)?;
            Ok(())
        })?,
    )?;

    macros.set(
        "update",
        scope.create_function(|_, (id, source): (Value, String)| {
            let id = value_to_byte(id, "module id")?;
            ensure_writable_module(id)?;
            let mut app = app.borrow_mut();
            if !app.project().modules.contains_key(&id) {
                return Err(mlua::Error::RuntimeError(format!(
                    "module {id:02X} does not exist"
                )));
            }
            app.repl_apply_module_source(id, source.clone())
                .map_err(mlua::Error::RuntimeError)?;
            Ok(())
        })?,
    )?;

    macros.set(
        "delete",
        scope.create_function(|_, id: Value| {
            let id = value_to_byte(id, "module id")?;
            if id == 0 {
                return Err(mlua::Error::RuntimeError(
                    "module 00 cannot be deleted".to_string(),
                ));
            }
            ensure_writable_module(id)?;
            let mut app = app.borrow_mut();
            if !app.project().modules.contains_key(&id) {
                return Err(mlua::Error::RuntimeError(format!(
                    "module {id:02X} does not exist"
                )));
            }
            let mut modules = app.project().modules.clone();
            modules.remove(&id);
            app.repl_validate_modules(&modules)
                .map_err(mlua::Error::RuntimeError)?;
            app.repl_project_mut().modules.remove(&id);
            app.repl_mark_mutated();
            Ok(())
        })?,
    )?;

    macros.set(
        "replace-form",
        scope.create_function(|_, (id, prefix, replacement): (Value, Value, Value)| {
            let id = value_to_byte(id, "module id")?;
            ensure_writable_module(id)?;
            // The compiler owns a separate Lua state; handing it a value from the
            // REPL state panics. Structural edits therefore take source strings.
            let prefix = value_to_source_string(prefix)?;
            let replacement = value_to_source_string(replacement)?;
            let mut app = app.borrow_mut();
            let source = app.project().modules.get(&id).cloned().ok_or_else(|| {
                mlua::Error::RuntimeError(format!("module {id:02X} does not exist"))
            })?;
            let replaced = compiler
                .replace_form(&source, &prefix, &replacement)
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            app.repl_apply_module_source(id, replaced)
                .map_err(mlua::Error::RuntimeError)?;
            Ok(())
        })?,
    )?;

    macros.set(
        "reload-all",
        scope.create_function(|_, ()| {
            let mut app = app.borrow_mut();
            app.repl_reload_all().map_err(mlua::Error::RuntimeError)?;
            Ok(())
        })?,
    )?;

    mtrak.set("macros", macros)?;
    Ok(())
}

fn install_settings<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    mtrak: &Table,
    app: &'scope RefCell<&'env mut App>,
) -> mlua::Result<()> {
    let settings = lua.create_table()?;

    settings.set(
        "get",
        scope.create_function(|lua, key: String| {
            let app = app.borrow();
            settings_value(lua, app.project().composition.global_settings(), &key)
        })?,
    )?;

    settings.set(
        "set",
        scope.create_function(|_, (key, value): (String, Value)| {
            let mut app = app.borrow_mut();
            let settings = app.repl_project_mut().composition.global_settings_mut();
            match key.as_str() {
                "root" => {
                    settings.root = value_to_u8(value)?;
                }
                "scale" => {
                    settings.scale = value_to_scale(value)?;
                }
                "modal-degree" | "modal_degree" => {
                    settings.modal_degree = value_to_u8(value)?;
                }
                other => {
                    return Err(mlua::Error::RuntimeError(format!(
                        "unknown setting {other}"
                    )))
                }
            }
            app.repl_mark_mutated();
            Ok(())
        })?,
    )?;

    let settings_metatable = lua.create_table()?;
    settings_metatable.set(
        "__call",
        scope.create_function(|lua, (_self, key): (Table, String)| {
            let app = app.borrow();
            settings_value(lua, app.project().composition.global_settings(), &key)
        })?,
    )?;
    settings.set_metatable(Some(settings_metatable));
    mtrak.set("settings", settings)?;
    Ok(())
}

fn install_project<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    mtrak: &Table,
    app: &'scope RefCell<&'env mut App>,
) -> mlua::Result<()> {
    let project = lua.create_table()?;

    project.set(
        "bpm",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project().composition.bpm().as_f64())
        })?,
    )?;

    project.set(
        "set-bpm",
        scope.create_function(|_, value: Value| {
            let bpm = value_to_u32(value, "BPM")?;
            let mut app = app.borrow_mut();
            let bpm = mtrak_core::timing::Bpm::from_int(bpm)
                .ok_or_else(|| mlua::Error::RuntimeError("invalid BPM".to_string()))?;
            app.repl_project_mut().composition.set_bpm(bpm);
            app.repl_mark_mutated();
            Ok(())
        })?,
    )?;

    project.set(
        "tpb",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project().composition.tpb())
        })?,
    )?;

    project.set(
        "seed",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(format!("0x{:016X}", app.project().composition.seed()))
        })?,
    )?;

    project.set(
        "set-seed",
        scope.create_function(|_, seed: Value| {
            let mut app = app.borrow_mut();
            let seed = value_to_seed(seed)?;
            app.repl_project_mut().composition.set_seed(seed);
            app.repl_mark_mutated();
            Ok(())
        })?,
    )?;

    project.set(
        "selected-main",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project().composition.selected_main().value())
        })?,
    )?;

    project.set(
        "set-selected-main",
        scope.create_function(|_, id: Value| {
            let id = value_to_byte(id, "pattern id")?;
            let mut app = app.borrow_mut();
            app.repl_project_mut()
                .composition
                .select_main(PatternId(id))
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))?;
            app.repl_mark_mutated();
            Ok(())
        })?,
    )?;

    project.set(
        "dirty",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.is_dirty())
        })?,
    )?;

    project.set(
        "revision",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project_revision())
        })?,
    )?;

    project.set(
        "path",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.path().map(|path| path.display().to_string()))
        })?,
    )?;

    project.set(
        "patterns",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app
                .project()
                .composition
                .pattern_ids()
                .into_iter()
                .map(|id| id.value())
                .collect::<Vec<u8>>())
        })?,
    )?;

    project.set(
        "modules",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.project().modules.keys().copied().collect::<Vec<u8>>())
        })?,
    )?;

    project.set(
        "playing",
        scope.create_function(|_, ()| {
            let app = app.borrow();
            Ok(app.is_playing())
        })?,
    )?;

    mtrak.set("project", project)?;
    Ok(())
}

fn install_repl<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    lua: &'env Lua,
    mtrak: &Table,
    globals: &'scope Table,
) -> mlua::Result<()> {
    let repl = lua.create_table()?;
    repl.set(
        "reset",
        scope.create_function(|_, ()| {
            globals.clear()?;
            Ok(())
        })?,
    )?;
    mtrak.set("repl", repl)?;
    Ok(())
}

fn settings_value(
    lua: &Lua,
    settings: &mtrak_core::settings::Settings,
    key: &str,
) -> mlua::Result<Value> {
    match key {
        "root" => Ok(Value::Integer(settings.root as i64)),
        "scale" => Ok(Value::String(
            lua.create_string(scale_name(settings.scale))?,
        )),
        "modal-degree" | "modal_degree" => Ok(Value::Integer(settings.modal_degree as i64)),
        other => Err(mlua::Error::RuntimeError(format!(
            "unknown setting {other}"
        ))),
    }
}

fn value_to_u8(value: Value) -> mlua::Result<u8> {
    value
        .as_integer()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| mlua::Error::RuntimeError("expected an integer 0-255".to_string()))
}

fn value_to_scale(value: Value) -> mlua::Result<Scale> {
    match value {
        Value::String(name) => {
            let name = name.to_str()?;
            mtrak_core::settings::lookup_builtin(name.as_ref())
                .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown scale {name}")))
        }
        Value::Integer(mask) => {
            let mask = u16::try_from(mask).map_err(|_| {
                mlua::Error::RuntimeError("scale mask must be between 0 and 65535".to_string())
            })?;
            Ok(Scale::from_mask(mask))
        }
        _ => Err(mlua::Error::RuntimeError(
            "scale must be a name or a mask".to_string(),
        )),
    }
}

fn scale_name(scale: Scale) -> &'static str {
    if scale == mtrak_core::settings::CHROMATIC_SCALE {
        "chromatic"
    } else if Some(scale) == mtrak_core::settings::lookup_builtin("major") {
        "major"
    } else if Some(scale) == mtrak_core::settings::lookup_builtin("minor") {
        "minor"
    } else {
        "custom"
    }
}

/// Converts a Lua value to a whole `u64`, rejecting fractional numbers.
fn value_to_u64(value: Value, field: &str) -> mlua::Result<u64> {
    match value {
        Value::Integer(integer) => Ok(integer as u64),
        Value::Number(number) => {
            if !number.is_finite() || number.fract() != 0.0 {
                return Err(mlua::Error::RuntimeError(format!(
                    "{field} must be a whole number"
                )));
            }
            if number < 0.0 {
                return Err(mlua::Error::RuntimeError(format!(
                    "{field} must be non-negative"
                )));
            }
            Ok(number as u64)
        }
        _ => Err(mlua::Error::RuntimeError(format!(
            "{field} must be a number"
        ))),
    }
}

fn value_to_byte(value: Value, field: &str) -> mlua::Result<u8> {
    u8::try_from(value_to_u64(value, field)?)
        .map_err(|_| mlua::Error::RuntimeError(format!("{field} is out of range")))
}

fn value_to_u32(value: Value, field: &str) -> mlua::Result<u32> {
    u32::try_from(value_to_u64(value, field)?)
        .map_err(|_| mlua::Error::RuntimeError(format!("{field} is out of range")))
}

fn value_to_usize(value: Value, field: &str) -> mlua::Result<usize> {
    usize::try_from(value_to_u64(value, field)?)
        .map_err(|_| mlua::Error::RuntimeError(format!("{field} is out of range")))
}

/// Reads an optional whole-number field from a table.
fn table_u32(table: &Table, key: &str) -> mlua::Result<Option<u32>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        value => Ok(Some(value_to_u32(value, key)?)),
    }
}

fn table_usize(table: &Table, key: &str) -> mlua::Result<Option<usize>> {
    match table.get::<Value>(key)? {
        Value::Nil => Ok(None),
        value => Ok(Some(value_to_usize(value, key)?)),
    }
}

fn value_to_seed(value: Value) -> mlua::Result<u64> {
    match value {
        Value::Integer(seed) => Ok(seed as u64),
        Value::Number(seed) => {
            // Lua 5.4 turns literals above `i64::MAX` into floats, so accept
            // whole floats here and point at hex strings for the full range.
            if !seed.is_finite() || seed.fract() != 0.0 {
                return Err(mlua::Error::RuntimeError(
                    "seed must be a whole number or hex string".to_string(),
                ));
            }
            if seed < 0.0 {
                return Err(mlua::Error::RuntimeError(
                    "seed must be non-negative".to_string(),
                ));
            }
            if seed > 9_007_199_254_740_992.0 {
                return Err(mlua::Error::RuntimeError(
                    "seed is too large for a number; use a hex string".to_string(),
                ));
            }
            Ok(seed as u64)
        }
        Value::String(seed) => {
            let text = seed.to_str()?;
            let text = text.trim();
            let parsed =
                if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                    u64::from_str_radix(hex, 16)
                } else {
                    text.parse::<u64>()
                };
            parsed.map_err(|_| mlua::Error::RuntimeError("invalid seed".to_string()))
        }
        _ => Err(mlua::Error::RuntimeError(
            "seed must be an integer or hex string".to_string(),
        )),
    }
}

/// A channel kind name used by `mtrak.events`.
pub fn channel_kind_name(kind: ChannelKind) -> &'static str {
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

/// A channel kind parsed from its name.
pub fn channel_kind_from_name(name: &str) -> Option<ChannelKind> {
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

/// Unused helper to keep the event type in scope for tests.
#[allow(dead_code)]
fn _event_type_marker(_: ChannelMessage, _: Event) {}
