//! Lua-to-JSON conversion and printed-value fallback.

use std::collections::HashSet;

use mlua::{Table, Value};
use serde_json::{Map, Number, Value as JsonValue};

use crate::error::JsonError;

/// Maximum nesting depth accepted by JSON conversion.
pub const MAX_JSON_DEPTH: usize = 32;
/// Maximum number of values accepted by JSON conversion.
pub const MAX_JSON_NODES: usize = 10_000;

/// Converts a Lua value to JSON, rejecting cycles, functions, and userdata.
pub fn value_to_json(value: &Value) -> Result<JsonValue, JsonError> {
    let mut seen = HashSet::new();
    let mut nodes = 0usize;
    convert(value, 0, &mut nodes, &mut seen)
}

fn convert(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
    seen: &mut HashSet<usize>,
) -> Result<JsonValue, JsonError> {
    if depth > MAX_JSON_DEPTH {
        return Err(JsonError::Depth);
    }
    *nodes += 1;
    if *nodes > MAX_JSON_NODES {
        return Err(JsonError::NodeCount);
    }
    match value {
        Value::Nil => Ok(JsonValue::Null),
        Value::Boolean(boolean) => Ok(JsonValue::Bool(*boolean)),
        Value::Integer(integer) => Ok(JsonValue::Number(Number::from(*integer))),
        Value::Number(number) => Number::from_f64(*number)
            .map(JsonValue::Number)
            .ok_or_else(|| JsonError::Opaque("non-finite number".to_string())),
        Value::String(string) => {
            let text = string
                .to_str()
                .map_err(|_| JsonError::Opaque("non-UTF-8 string".to_string()))?;
            Ok(JsonValue::String(text.to_string()))
        }
        Value::Table(table) => {
            let pointer = table.to_pointer() as usize;
            if !seen.insert(pointer) {
                return Err(JsonError::Opaque("cyclic table".to_string()));
            }
            let result = table_to_json(table, depth, nodes, seen);
            seen.remove(&pointer);
            result
        }
        Value::Function(_) => Err(JsonError::Opaque("function".to_string())),
        Value::UserData(_) => Err(JsonError::Opaque("userdata".to_string())),
        Value::Thread(_) => Err(JsonError::Opaque("thread".to_string())),
        Value::LightUserData(_) => Err(JsonError::Opaque("light userdata".to_string())),
        Value::Error(_) => Err(JsonError::Opaque("error".to_string())),
        _ => Err(JsonError::Opaque("unknown value".to_string())),
    }
}

fn table_to_json(
    table: &Table,
    depth: usize,
    nodes: &mut usize,
    seen: &mut HashSet<usize>,
) -> Result<JsonValue, JsonError> {
    let len = table.raw_len();
    let mut is_array = true;
    let mut count = 0usize;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair.map_err(|_| JsonError::Opaque("table iteration".to_string()))?;
        count += 1;
        match key {
            Value::Integer(index) if index >= 1 && (index as usize) <= len => {}
            _ => {
                is_array = false;
                break;
            }
        }
    }
    if is_array && count == len {
        let mut array = Vec::with_capacity(len);
        for index in 1..=len {
            let value: Value = table
                .raw_get(index)
                .map_err(|_| JsonError::Opaque("table index".to_string()))?;
            array.push(convert(&value, depth + 1, nodes, seen)?);
        }
        Ok(JsonValue::Array(array))
    } else {
        let mut map = Map::new();
        for pair in table.clone().pairs::<Value, Value>() {
            let (key, value) =
                pair.map_err(|_| JsonError::Opaque("table iteration".to_string()))?;
            let key = match key {
                Value::String(string) => string
                    .to_str()
                    .map_err(|_| JsonError::Opaque("non-UTF-8 key".to_string()))?
                    .to_string(),
                Value::Integer(integer) => integer.to_string(),
                Value::Number(number) => number.to_string(),
                _ => return Err(JsonError::Opaque("table key".to_string())),
            };
            map.insert(key, convert(&value, depth + 1, nodes, seen)?);
        }
        Ok(JsonValue::Object(map))
    }
}

/// Renders a Lua value as a readable Fennel/Lua-like string.
pub fn print_value(value: &Value) -> String {
    let mut output = String::new();
    print_into(value, &mut output, 0);
    output
}

fn print_into(value: &Value, output: &mut String, depth: usize) {
    if depth > 8 {
        output.push_str("...");
        return;
    }
    match value {
        Value::Nil => output.push_str("nil"),
        Value::Boolean(boolean) => output.push_str(if *boolean { "true" } else { "false" }),
        Value::Integer(integer) => output.push_str(&integer.to_string()),
        Value::Number(number) => output.push_str(&number.to_string()),
        Value::String(string) => {
            if let Ok(text) = string.to_str() {
                output.push('"');
                for character in text.chars() {
                    match character {
                        '"' => output.push_str("\\\""),
                        '\\' => output.push_str("\\\\"),
                        '\n' => output.push_str("\\n"),
                        '\r' => output.push_str("\\r"),
                        '\t' => output.push_str("\\t"),
                        _ => output.push(character),
                    }
                }
                output.push('"');
            } else {
                output.push_str("<string>");
            }
        }
        Value::Table(table) => {
            let len = table.raw_len();
            let mut is_array = true;
            for (key, _) in table.clone().pairs::<Value, Value>().flatten() {
                match key {
                    Value::Integer(index) if index >= 1 && (index as usize) <= len => {}
                    _ => {
                        is_array = false;
                        break;
                    }
                }
            }
            if is_array {
                output.push('[');
                for index in 1..=len {
                    if index > 1 {
                        output.push(' ');
                    }
                    if let Ok(value) = table.raw_get::<Value>(index) {
                        print_into(&value, output, depth + 1);
                    }
                }
                output.push(']');
            } else {
                output.push('{');
                let mut first = true;
                for (key, value) in table.clone().pairs::<Value, Value>().flatten() {
                    if !first {
                        output.push(' ');
                    }
                    first = false;
                    print_into(&key, output, depth + 1);
                    output.push_str(": ");
                    print_into(&value, output, depth + 1);
                }
                output.push('}');
            }
        }
        Value::Function(_) => output.push_str("<function>"),
        Value::UserData(_) => output.push_str("<userdata>"),
        Value::Thread(_) => output.push_str("<thread>"),
        Value::LightUserData(_) => output.push_str("<light userdata>"),
        Value::Error(_) => output.push_str("<error>"),
        _ => output.push_str("<value>"),
    }
}

/// Returns a short type name for a Lua value.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) | Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::UserData(_) => "userdata",
        Value::Thread(_) => "thread",
        Value::LightUserData(_) => "light userdata",
        Value::Error(_) => "error",
        _ => "value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn converts_scalars() {
        let lua = Lua::new();
        assert_eq!(value_to_json(&Value::Nil).unwrap(), JsonValue::Null);
        assert_eq!(
            value_to_json(&Value::Boolean(true)).unwrap(),
            JsonValue::Bool(true)
        );
        assert_eq!(
            value_to_json(&Value::Integer(42)).unwrap(),
            serde_json::json!(42)
        );
        let string = lua.create_string("hello").unwrap();
        assert_eq!(
            value_to_json(&Value::String(string)).unwrap(),
            serde_json::json!("hello")
        );
    }

    #[test]
    fn converts_arrays() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set(1, 1).unwrap();
        table.set(2, 2).unwrap();
        table.set(3, 3).unwrap();
        assert_eq!(
            value_to_json(&Value::Table(table)).unwrap(),
            serde_json::json!([1, 2, 3])
        );
    }

    #[test]
    fn converts_objects() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("name", "bass").unwrap();
        table.set("id", 3).unwrap();
        let json = value_to_json(&Value::Table(table)).unwrap();
        assert_eq!(json["name"], "bass");
        assert_eq!(json["id"], 3);
    }

    #[test]
    fn rejects_cyclic_tables() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("self", table.clone()).unwrap();
        let error = value_to_json(&Value::Table(table)).unwrap_err();
        assert!(error.to_string().contains("cyclic"), "{error}");
    }

    #[test]
    fn rejects_functions() {
        let lua = Lua::new();
        let function = lua.create_function(|_, ()| Ok(())).unwrap();
        let error = value_to_json(&Value::Function(function)).unwrap_err();
        assert!(error.to_string().contains("function"), "{error}");
    }

    #[test]
    fn rejects_excessive_depth() {
        let lua = Lua::new();
        let mut table = lua.create_table().unwrap();
        let outer = table.clone();
        for _ in 0..MAX_JSON_DEPTH + 2 {
            let next = lua.create_table().unwrap();
            table.set(1, next.clone()).unwrap();
            table = next;
        }
        let error = value_to_json(&Value::Table(outer)).unwrap_err();
        assert!(error.to_string().contains("depth"), "{error}");
    }

    #[test]
    fn prints_values() {
        let lua = Lua::new();
        assert_eq!(print_value(&Value::Nil), "nil");
        assert_eq!(print_value(&Value::Boolean(true)), "true");
        assert_eq!(print_value(&Value::Integer(42)), "42");
        let string = lua.create_string("hi").unwrap();
        assert_eq!(print_value(&Value::String(string)), "\"hi\"");
    }

    #[test]
    fn type_names_are_stable() {
        let lua = Lua::new();
        assert_eq!(type_name(&Value::Nil), "nil");
        assert_eq!(type_name(&Value::Boolean(true)), "boolean");
        assert_eq!(type_name(&Value::Integer(1)), "number");
        let string = lua.create_string("x").unwrap();
        assert_eq!(type_name(&Value::String(string)), "string");
        let table = lua.create_table().unwrap();
        assert_eq!(type_name(&Value::Table(table)), "table");
    }
}
