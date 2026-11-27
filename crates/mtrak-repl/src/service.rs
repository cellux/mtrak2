//! Application-thread request handling and JSON responses.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;

use mlua::Value;
use mtrak_tui::App;

use crate::error::ReplError;
use crate::json::{print_value, type_name, value_to_json};
use crate::request::{ReplRequest, ReplRequestKind, ReplResponse};
use crate::session::ReplSession;

/// Services one request on the application thread.
pub fn handle_request(
    session: &mut ReplSession,
    app: &mut App,
    kind: ReplRequestKind,
) -> ReplResponse {
    match kind {
        ReplRequestKind::Eval(form) => {
            app.stop_playback_and_release();
            match session.eval(app, &form) {
                Ok(value) => success_response(&value),
                Err(error) => error_response(&error),
            }
        }
        ReplRequestKind::Play { pattern, row } => match app.repl_play(pattern, row) {
            Ok(()) => success_response(&Value::Nil),
            Err(error) => error_response(&ReplError::Protocol(error)),
        },
        ReplRequestKind::Stop => {
            app.stop_playback_and_release();
            success_response(&Value::Nil)
        }
        ReplRequestKind::Reset => match session.reset() {
            Ok(()) => success_response(&Value::Nil),
            Err(error) => error_response(&error),
        },
        ReplRequestKind::Status => match status_table(session.lua(), app) {
            Ok(value) => success_response(&Value::Table(value)),
            Err(error) => error_response(&ReplError::Runtime(error.to_string())),
        },
        ReplRequestKind::Help => match crate::api::help_table(session.lua()) {
            Ok(value) => success_response(&Value::Table(value)),
            Err(error) => error_response(&ReplError::Runtime(error.to_string())),
        },
    }
}

/// Services every request waiting in `receiver`, skipping cancelled ones.
///
/// A request is cancelled when its HTTP worker gave up waiting: the client has
/// already seen a timeout, so running the request would mutate state behind its
/// back.
pub fn drain_requests(session: &mut ReplSession, app: &mut App, receiver: &Receiver<ReplRequest>) {
    while let Ok(request) = receiver.try_recv() {
        if request.cancel.load(Ordering::SeqCst) {
            continue;
        }
        let response = handle_request(session, app, request.kind);
        let _ = request.response.send(response);
    }
}

/// Reports playback and project state without disturbing playback.
fn status_table(lua: &mlua::Lua, app: &App) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    table.set("playing", app.is_playing())?;
    table.set("row", app.play_row())?;
    table.set("pattern", app.current_pattern().value())?;
    table.set("mode", format!("{:?}", app.mode()).to_lowercase())?;
    table.set("dirty", app.is_dirty())?;
    table.set("revision", app.project_revision())?;
    Ok(table)
}

/// Builds a successful JSON response.
pub fn success_response(value: &Value) -> ReplResponse {
    let mut body = serde_json::Map::new();
    body.insert("ok".to_string(), serde_json::Value::Bool(true));
    match value_to_json(value) {
        Ok(json) => {
            body.insert("value".to_string(), json);
        }
        Err(error) => {
            body.insert(
                "value".to_string(),
                serde_json::Value::String(print_value(value)),
            );
            body.insert(
                "diagnostic".to_string(),
                serde_json::Value::String(error.to_string()),
            );
        }
    }
    body.insert(
        "printed".to_string(),
        serde_json::Value::String(print_value(value)),
    );
    body.insert(
        "type".to_string(),
        serde_json::Value::String(type_name(value).to_string()),
    );
    ReplResponse {
        status: 200,
        body: serde_json::Value::Object(body),
    }
}

/// Builds an error JSON response.
pub fn error_response(error: &ReplError) -> ReplResponse {
    let mut body = serde_json::Map::new();
    body.insert("ok".to_string(), serde_json::Value::Bool(false));
    let mut error_object = serde_json::Map::new();
    error_object.insert(
        "kind".to_string(),
        serde_json::Value::String(error.kind().to_string()),
    );
    error_object.insert(
        "message".to_string(),
        serde_json::Value::String(error.to_string()),
    );
    let (line, column) = parse_location(&error.to_string());
    if let Some(line) = line {
        error_object.insert("line".to_string(), serde_json::Value::Number(line.into()));
    }
    if let Some(column) = column {
        error_object.insert(
            "column".to_string(),
            serde_json::Value::Number(column.into()),
        );
    }
    body.insert("error".to_string(), serde_json::Value::Object(error_object));
    ReplResponse {
        status: 400,
        body: serde_json::Value::Object(body),
    }
}

/// Extracts a `source:line:column` location from a diagnostic.
pub fn parse_location(message: &str) -> (Option<u32>, Option<u32>) {
    for part in message.split_whitespace() {
        let mut pieces = part.split(':');
        let _source = pieces.next();
        if let (Some(line), Some(column)) = (pieces.next(), pieces.next()) {
            if let (Ok(line), Ok(column)) = (line.parse(), column.parse()) {
                return (Some(line), Some(column));
            }
        }
    }
    (None, None)
}
