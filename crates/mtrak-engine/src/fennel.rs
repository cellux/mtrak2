//! Fennel-to-Lua compiler integration.
//!
//! Macro sources are Fennel, but the execution VM is Lua 5.4. This module owns a
//! separate, restricted Lua state that loads the embedded standalone Fennel
//! compiler and turns Fennel source strings into correlated Lua source.
//!
//! The compiler state is deliberately isolated from the macro execution state:
//! it never sees the injected mtrak API, project state, or running coroutines.
//! Filesystem, network, process, and external-module access are blocked before
//! any user source is compiled, so `include` and `import-macros` can only
//! resolve names that are already loaded inside the compiler (decision D-102).
//!
//! Fennel's strict global validation is used with an explicit per-module
//! allowlist, and global-name mangling is reproduced by [`fennel_global_name`]
//! so the execution environment can bind the mangled names the generated Lua
//! refers to (decision D-103).

use mlua::{Function, Lua, MultiValue, Table, Value};

/// The Fennel release checked into `assets/fennel.lua`.
///
/// Bump this together with the asset when running `make update-fennel`.
pub const FENNEL_VERSION: &str = "1.6.1";

/// The standalone Fennel compiler embedded in the executable.
pub const FENNEL_SOURCE: &str = include_str!("../assets/fennel.lua");

/// A failure to initialize the compiler or compile a Fennel source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CompileError {
    /// The Fennel diagnostic, including the synthetic source name and line.
    pub message: String,
}

/// The curated standard-library globals available to compiled macros.
const SAFE_GLOBALS: &[&str] = &[
    "string", "table", "math", "pairs", "ipairs", "next", "type", "tostring", "tonumber", "error",
    "assert", "select", "pcall", "xpcall",
];

/// The mtrak API names available to every module.
const API_GLOBALS: &[&str] = &[
    "emit",
    "emit-sysex",
    "emit-meta",
    "trigger",
    "stop-track",
    "get-setting",
    "set-setting",
    "wait",
    "require",
    "random-generator",
];

/// The extra API name available only to the setup module (`00`).
const SETUP_GLOBALS: &[&str] = &["register-command"];

/// A restricted Fennel compiler.
///
/// Cloning shares the same underlying compiler state, so a runtime can hand a
/// copy to each module environment without recompiling the embedded compiler.
#[derive(Clone)]
pub struct FennelCompiler {
    lua: Lua,
    fennel: Table,
    replace_form_fn: Function,
}

/// Finds exactly one occurrence of `prefix` in `source` and replaces its source
/// span with `replacement`, preserving all source outside the span.
const REPLACE_FORM_SCRIPT: &str = r#"
return function(fennel, source, prefix, replacement)
  local function parse_one(str)
    local iter = fennel.parser(str, "prefix")
    local ok, form = iter()
    if not ok then return nil end
    local ok2 = iter()
    if ok2 then return nil end
    return form
  end

  local prefix_form = parse_one(prefix)
  if not prefix_form then return nil, "invalid prefix form" end
  -- Only tables carry the source metadata needed to locate a span.
  if type(prefix_form) ~= "table" then
    return nil, "prefix literal has no source position metadata"
  end

  local meta_keys = {
    line = true, col = true, filename = true, byteend = true, bytestart = true,
    endline = true, endcol = true, closer = true,
  }

  -- Fennel gives lists a shared metatable and tables a unique metatable that
  -- carries the literal's source position. Compare sequence-ness rather than
  -- metatable identity so plain tables and maps still match structurally.
  local function sequence_metatable(form)
    local mt = getmetatable(form)
    if type(mt) == "table" and mt.sequence then return mt.sequence end
    return nil
  end

  local function ast_equal(a, b)
    if type(a) ~= type(b) then return false end
    if type(a) ~= "table" then return a == b end
    if sequence_metatable(a) ~= sequence_metatable(b) then return false end
    local keys = {}
    for k in pairs(a) do
      if not (type(k) == "string" and meta_keys[k]) then
        keys[k] = true
      end
    end
    for k in pairs(b) do
      if not (type(k) == "string" and meta_keys[k]) then
        if not keys[k] then return false end
        keys[k] = nil
      end
    end
    if next(keys) then return false end
    for k in pairs(a) do
      if not (type(k) == "string" and meta_keys[k]) then
        if not ast_equal(a[k], b[k]) then return false end
      end
    end
    return true
  end

  local matches = {}
  local function walk(form)
    if type(form) ~= "table" then return end
    if ast_equal(form, prefix_form) then
      table.insert(matches, form)
    end
    for k, v in pairs(form) do
      if not (type(k) == "string" and meta_keys[k]) then
        walk(v)
      end
    end
  end

  local iter = fennel.parser(source, "module")
  while true do
    local ok, form = iter()
    if not ok then break end
    walk(form)
  end

  if #matches == 0 then return nil, "no matching form" end
  if #matches > 1 then return nil, "multiple matching forms" end

  local match = matches[1]
  -- Lists carry positions on the form itself; table literals keep them in the
  -- unique metatable. Sequence literals lose their position because Fennel
  -- moves it into the shared sequence metatable, so they cannot be replaced.
  local start, finish = match.bytestart, match.byteend
  if not start then
    local mt = getmetatable(match)
    if type(mt) == "table" and not mt.sequence then
      start, finish = mt.bytestart, mt.byteend
    end
  end
  if not start or not finish then
    return nil, "matched form has no source position metadata"
  end
  return source:sub(1, start - 1) .. replacement .. source:sub(finish + 1)
end
"#;

impl FennelCompiler {
    /// Loads the embedded compiler and locks down its state.
    pub fn new() -> Result<Self, CompileError> {
        let lua = Lua::new();
        // Remove `os` before the compiler loads so its captured `getenv`
        // fallback is the null function and no `FENNEL_PATH` can leak into the
        // search paths. The remaining restrictions are applied afterwards, once
        // the compiler's own modules have resolved.
        lua.globals().set("os", Value::Nil).map_err(compile_error)?;
        let fennel: Table = lua
            .load(FENNEL_SOURCE)
            .set_name("fennel.lua")
            .eval()
            .map_err(compile_error)?;
        restrict(&lua).map_err(compile_error)?;
        let replace_form_fn: Function = lua
            .load(REPLACE_FORM_SCRIPT)
            .set_name("replace-form")
            .eval()
            .map_err(compile_error)?;
        Ok(FennelCompiler {
            lua,
            fennel,
            replace_form_fn,
        })
    }

    /// The compiler's reported Fennel version.
    pub fn version(&self) -> Result<String, CompileError> {
        self.fennel.get::<String>("version").map_err(compile_error)
    }

    /// Compiles `source` for `module_id` into correlated Lua source.
    ///
    /// `allow_register` adds the setup-only API names to the allowlist.
    pub fn compile(
        &self,
        source: &str,
        module_id: u8,
        allow_register: bool,
    ) -> Result<String, CompileError> {
        self.compile_with(
            source,
            &source_name(module_id),
            allowed_globals(allow_register).into_iter(),
        )
    }

    /// Compiles a REPL form into correlated Lua source.
    ///
    /// The synthetic source name is `repl`, so diagnostics refer to the
    /// submitted form without pretending to be a module file.
    pub fn compile_repl(
        &self,
        source: &str,
        allowed_globals: &[String],
    ) -> Result<String, CompileError> {
        self.compile_with(
            source,
            "repl",
            allowed_globals.iter().map(|name| name.as_str()),
        )
    }

    /// Replaces exactly one occurrence of `prefix` in `source` with
    /// `replacement`, preserving all source outside the matched span.
    pub fn replace_form(
        &self,
        source: &str,
        prefix: &str,
        replacement: &str,
    ) -> Result<String, CompileError> {
        let result: mlua::Result<(Option<String>, Option<String>)> =
            self.replace_form_fn
                .call((self.fennel.clone(), source, prefix, replacement));
        match result {
            Ok((Some(new_source), _)) => Ok(new_source),
            Ok((None, Some(message))) => Err(CompileError { message }),
            Ok((None, None)) => Err(CompileError {
                message: "replace-form failed".to_string(),
            }),
            Err(error) => Err(compile_error(error)),
        }
    }

    fn compile_with<'a>(
        &self,
        source: &str,
        filename: &str,
        allowed_globals: impl Iterator<Item = &'a str>,
    ) -> Result<String, CompileError> {
        let compile_string: Function = self.fennel.get("compile-string").map_err(compile_error)?;
        let opts = self.lua.create_table().map_err(compile_error)?;
        opts.set("filename", filename).map_err(compile_error)?;
        // Source correlation pads the generated Lua so its line numbers match
        // the Fennel lines shown in the Code screen or REPL form.
        opts.set("correlate", true).map_err(compile_error)?;
        // `unfriendly` and `error-pinpoint` keep diagnostics free of terminal
        // escape codes and suggestion chatter.
        opts.set("unfriendly", true).map_err(compile_error)?;
        opts.set("error-pinpoint", false).map_err(compile_error)?;
        opts.set("source", source).map_err(compile_error)?;
        let allowed = self.lua.create_table().map_err(compile_error)?;
        for (index, name) in allowed_globals.enumerate() {
            allowed.set(index + 1, name).map_err(compile_error)?;
        }
        opts.set("allowedGlobals", allowed).map_err(compile_error)?;
        let (lua_source, _sourcemap): (String, Table) =
            compile_string.call((source, opts)).map_err(compile_error)?;
        Ok(lua_source)
    }
}

/// The synthetic Fennel source name used for `module_id`.
pub fn source_name(module_id: u8) -> String {
    format!("module {module_id:02X}.fnl")
}

/// The per-module allowlist of global names.
fn allowed_globals(allow_register: bool) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    names.extend_from_slice(SAFE_GLOBALS);
    names.extend_from_slice(API_GLOBALS);
    if allow_register {
        names.extend_from_slice(SETUP_GLOBALS);
    }
    names
}

/// Removes filesystem, process, and external-module access from the compiler.
///
/// The embedded compiler itself is already loaded, so its internal `require`
/// calls have resolved into `package.loaded`; clearing the searchers means user
/// source cannot pull in anything new. `io.open` becomes a function that always
/// reports failure, which makes Fennel's `include` and macro searchers report
/// "module not found" instead of reading a file.
fn restrict(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let io = lua.create_table()?;
    io.set(
        "open",
        lua.create_function(|_, _: MultiValue| Ok((Value::Nil, Value::Nil)))?,
    )?;
    globals.set("io", io)?;
    globals.set("os", Value::Nil)?;
    globals.set("debug", Value::Nil)?;
    if let Ok(package) = globals.get::<Table>("package") {
        package.set("path", "")?;
        package.set("cpath", "")?;
        package.set("searchers", lua.create_table()?)?;
        package.set("loaders", lua.create_table()?)?;
    }
    Ok(())
}

/// Reproduces Fennel's `global-mangling` for a global name.
///
/// Fennel leaves valid Lua identifiers alone and rewrites every other character
/// of a non-identifier as `_XX` behind a `__fnl_global__` prefix, so
/// `get-setting` becomes `__fnl_global__get_2dsetting`. The execution
/// environment must expose the mangled name for generated Lua to resolve it.
pub fn fennel_global_name(name: &str) -> String {
    if is_valid_lua_identifier(name) {
        return name.to_string();
    }
    let mut mangled = String::from("__fnl_global__");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() {
            mangled.push(char::from(byte));
        } else {
            mangled.push_str(&format!("_{byte:02x}"));
        }
    }
    mangled
}

/// Whether `name` is a valid Lua identifier (and not a Lua keyword).
fn is_valid_lua_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
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

fn compile_error(error: mlua::Error) -> CompileError {
    CompileError {
        message: error_message(error),
    }
}

/// Extracts the Fennel diagnostic from an `mlua` error.
///
/// `mlua` renders `RuntimeError` as "runtime error: {message}" and wraps errors
/// raised by Rust or Lua callbacks in `CallbackError`, whose `Display` appends a
/// Lua stack traceback. The diagnostic is the innermost cause, so unwrap it and
/// drop the redundant prefix.
fn error_message(error: mlua::Error) -> String {
    let message = match error {
        mlua::Error::RuntimeError(message) => message,
        mlua::Error::CallbackError { cause, .. } => match std::sync::Arc::try_unwrap(cause) {
            Ok(cause) => error_message(cause),
            Err(cause) => cause.to_string(),
        },
        other => other.to_string(),
    };
    strip_traceback(message)
}

/// Removes the Lua stack traceback that `mlua` appends to diagnostics.
fn strip_traceback(message: String) -> String {
    match message.find("\nstack traceback:") {
        Some(index) => message[..index].to_string(),
        None => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_compiler_reports_the_checked_in_version() {
        let compiler = FennelCompiler::new().unwrap();
        assert_eq!(compiler.version().unwrap(), FENNEL_VERSION);
    }

    #[test]
    fn compiles_a_simple_module_with_correlated_lines() {
        let compiler = FennelCompiler::new().unwrap();
        let source = "(local M {})\n\n(fn M.run [a1 a2 ctx]\n  (emit 0x90 a1 100))\n\nM\n";
        let lua = compiler.compile(source, 0x40, false).unwrap();
        let lines: Vec<&str> = lua.lines().collect();
        assert!(lines[0].contains("local M = {}"), "{lua}");
        assert!(lines[2].contains("M.run = function"), "{lua}");
        assert!(lines[3].contains("return emit(144, a1, 100)"), "{lua}");
    }

    #[test]
    fn mangles_kebab_case_api_names() {
        assert_eq!(fennel_global_name("emit"), "emit");
        assert_eq!(
            fennel_global_name("emit-sysex"),
            "__fnl_global__emit_2dsysex"
        );
        assert_eq!(
            fennel_global_name("get-setting"),
            "__fnl_global__get_2dsetting"
        );
        assert_eq!(
            fennel_global_name("random-generator"),
            "__fnl_global__random_2dgenerator"
        );
        assert_eq!(fennel_global_name("and"), "__fnl_global__and");
    }

    #[test]
    fn generated_lua_uses_the_mangled_names() {
        let compiler = FennelCompiler::new().unwrap();
        let lua = compiler
            .compile("(fn run [] (get-setting :root))", 0x40, false)
            .unwrap();
        assert!(lua.contains("__fnl_global__get_2dsetting"), "{lua}");
    }

    #[test]
    fn unknown_globals_are_rejected_at_compile_time() {
        let compiler = FennelCompiler::new().unwrap();
        let error = compiler
            .compile("(fn run [] (os.execute \"ls\"))", 0x40, false)
            .unwrap_err();
        assert!(error.message.contains("module 40.fnl"), "{}", error.message);
        assert!(
            error.message.contains("unknown identifier: os"),
            "{}",
            error.message
        );
    }

    #[test]
    fn setup_only_names_need_the_setup_module() {
        let compiler = FennelCompiler::new().unwrap();
        assert!(compiler
            .compile("(register-command :hello)", 0x40, false)
            .is_err());
        assert!(compiler
            .compile("(register-command :hello)", 0x00, true)
            .is_ok());
    }

    #[test]
    fn diagnostics_carry_the_fennel_line_number() {
        let compiler = FennelCompiler::new().unwrap();
        let source = "(fn run [a1]\n  (emit 0x90 a1)\n  (bogus 1))\n";
        let error = compiler.compile(source, 0x40, false).unwrap_err();
        assert!(
            error.message.contains("module 40.fnl:3:"),
            "{}",
            error.message
        );
    }

    #[test]
    fn compiles_repl_forms_with_the_repl_source_name() {
        let compiler = FennelCompiler::new().unwrap();
        let allowed = vec!["mtrak".to_string(), "print".to_string()];
        let lua = compiler
            .compile_repl("(mtrak.patterns.list)", &allowed)
            .unwrap();
        assert!(lua.contains("mtrak.patterns.list"), "{lua}");
        let error = compiler
            .compile_repl("(unknown-global 1)", &allowed)
            .unwrap_err();
        assert!(error.message.contains("repl:1:"), "{}", error.message);
    }

    #[test]
    fn repl_compilation_honours_the_supplied_allowlist() {
        let compiler = FennelCompiler::new().unwrap();
        let allowed = vec!["mtrak".to_string()];
        assert!(compiler.compile_repl("(mtrak.settings)", &allowed).is_ok());
        let error = compiler.compile_repl("(print 1)", &allowed).unwrap_err();
        assert!(
            error.message.contains("unknown identifier: print"),
            "{}",
            error.message
        );
    }

    #[test]
    fn replace_form_preserves_source_outside_the_match() {
        let compiler = FennelCompiler::new().unwrap();
        let source = "(fn run [a1]\n  (emit 0x90 a1 100))\n\n(fn other [] (emit 1 2 3))\n";
        let prefix = "(fn run [a1]\n  (emit 0x90 a1 100))";
        let replacement = "(fn run [a1]\n  (emit 0x91 a1 101))";
        let updated = compiler.replace_form(source, prefix, replacement).unwrap();
        assert!(updated.contains("(emit 0x91 a1 101)"), "{updated}");
        assert!(updated.contains("(fn other [] (emit 1 2 3))"), "{updated}");
    }

    #[test]
    fn replace_form_matches_table_and_map_literals() {
        let compiler = FennelCompiler::new().unwrap();
        let source = "(local M {:a 1})\n\n(fn run [] M)\n";
        let updated = compiler.replace_form(source, "{:a 1}", "{:a 2}").unwrap();
        assert_eq!(updated, "(local M {:a 2})\n\n(fn run [] M)\n");
        let empty = compiler
            .replace_form("(local M {})", "{}", "{:b 3}")
            .unwrap();
        assert_eq!(empty, "(local M {:b 3})");
    }

    #[test]
    fn replace_form_rejects_literal_prefixes_with_diagnostics() {
        let compiler = FennelCompiler::new().unwrap();
        let error = compiler
            .replace_form("(fn run [] 42)", "42", "43")
            .unwrap_err();
        assert!(
            error.message.contains("no source position"),
            "{}",
            error.message
        );
    }

    #[test]
    fn replace_form_rejects_sequence_literals_with_diagnostics() {
        let compiler = FennelCompiler::new().unwrap();
        let error = compiler
            .replace_form("(local M [1])", "[1]", "[2]")
            .unwrap_err();
        assert!(
            error.message.contains("no source position"),
            "{}",
            error.message
        );
    }

    #[test]
    fn replace_form_rejects_zero_and_multiple_matches() {
        let compiler = FennelCompiler::new().unwrap();
        let source = "(fn run [a1] (emit 0x90 a1 100))\n";
        let zero = compiler
            .replace_form(source, "(fn missing [] 1)", "(fn missing [] 2)")
            .unwrap_err();
        assert!(
            zero.message.contains("no matching form"),
            "{}",
            zero.message
        );
        let multiple = compiler
            .replace_form(
                "(fn run [a1] (emit 0x90 a1 100) (emit 0x90 a1 100))",
                "(emit 0x90 a1 100)",
                "(emit 0x91 a1 101)",
            )
            .unwrap_err();
        assert!(
            multiple.message.contains("multiple matching forms"),
            "{}",
            multiple.message
        );
    }

    #[test]
    fn malformed_source_is_a_parse_error() {
        let compiler = FennelCompiler::new().unwrap();
        let error = compiler.compile("(fn run [a1]", 0x40, false).unwrap_err();
        assert!(error.message.contains("Parse error"), "{}", error.message);
    }

    #[test]
    fn compile_time_macros_are_allowed_but_external_modules_are_not() {
        let compiler = FennelCompiler::new().unwrap();
        let lua = compiler
            .compile(
                "(macro double [x] `(* 2 ,x))\n(fn run [] (emit (double 21) 1))",
                0x40,
                false,
            )
            .unwrap();
        assert!(lua.contains("(2 * 21)"), "{lua}");

        let error = compiler
            .compile("(include \"missing\")", 0x40, false)
            .unwrap_err();
        assert!(
            error.message.contains("module not found"),
            "{}",
            error.message
        );

        let error = compiler
            .compile("(import-macros missing :missing)", 0x40, false)
            .unwrap_err();
        assert!(
            error.message.contains("module not found"),
            "{}",
            error.message
        );
    }
}
