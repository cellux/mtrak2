//! The persistent REPL session and its Lua environment.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, Table, Value, VmState};
use mtrak_engine::fennel::FennelCompiler;
use mtrak_tui::App;

use crate::api;
use crate::error::ReplError;

/// Hard cap on the REPL Lua heap.
pub const MEMORY_LIMIT: usize = 1024 * 1024 * 1024;
/// Instructions between hook checks.
const HOOK_INTERVAL: u32 = 1000;
/// Total instructions allowed per evaluation.
pub const INSTRUCTION_BUDGET: u64 = 50_000_000;
/// Wall-clock limit per evaluation.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A persistent REPL session.
pub struct ReplSession {
    lua: Lua,
    compiler: FennelCompiler,
    globals: Table,
    deadline: Rc<Cell<Option<Instant>>>,
    fuel: Rc<Cell<u64>>,
    limit_error: Rc<RefCell<Option<ReplError>>>,
}

impl ReplSession {
    /// Creates a fresh sandboxed session.
    pub fn new() -> Result<Self, ReplError> {
        let compiler =
            FennelCompiler::new().map_err(|error| ReplError::Compile(error.to_string()))?;
        let lua = Lua::new();
        lua.set_memory_limit(MEMORY_LIMIT)
            .map_err(|error| ReplError::Runtime(error.to_string()))?;
        let deadline = Rc::new(Cell::new(None));
        let fuel = Rc::new(Cell::new(0u64));
        let hook_deadline = deadline.clone();
        let hook_fuel = fuel.clone();
        let limit_error = Rc::new(RefCell::new(None));
        let hook_limit_error = limit_error.clone();
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
            move |_lua, _debug| {
                if let Some(deadline) = hook_deadline.get() {
                    if Instant::now() >= deadline {
                        *hook_limit_error.borrow_mut() = Some(ReplError::Timeout);
                        return Err(mlua::Error::external(ReplError::Timeout));
                    }
                }
                let remaining = hook_fuel.get();
                if remaining == 0 {
                    *hook_limit_error.borrow_mut() = Some(ReplError::InstructionLimit);
                    return Err(mlua::Error::external(ReplError::InstructionLimit));
                }
                hook_fuel.set(remaining - 1);
                Ok(VmState::Continue)
            },
        );
        let globals = lua
            .create_table()
            .map_err(|error| ReplError::Runtime(error.to_string()))?;
        Ok(ReplSession {
            lua,
            compiler,
            globals,
            deadline,
            fuel,
            limit_error,
        })
    }

    /// Evaluates one Fennel form against the current application.
    pub fn eval(&mut self, app: &mut App, source: &str) -> Result<Value, ReplError> {
        let allowed = self.allowed_globals();
        let lua_source = self
            .compiler
            .compile_repl(source, &allowed)
            .map_err(|error| ReplError::Compile(error.to_string()))?;
        self.deadline.set(Some(Instant::now() + TIMEOUT));
        self.fuel.set(INSTRUCTION_BUDGET / HOOK_INTERVAL as u64);
        *self.limit_error.borrow_mut() = None;
        let app = RefCell::new(app);
        let result = self.lua.scope(|scope| {
            let api = self.lua.create_table()?;
            api::install_api(scope, &self.lua, &api, &self.globals, &app, &self.compiler)?;
            let metatable = self.lua.create_table()?;
            metatable.set("__index", api)?;
            self.globals.set_metatable(Some(metatable));
            let value: Value = self
                .lua
                .load(&lua_source)
                .set_name("repl")
                .set_environment(self.globals.clone())
                .eval()?;
            Ok(value)
        });
        self.deadline.set(None);
        self.globals.set_metatable(None);
        // The hook's external error loses its type when it crosses the Lua
        // boundary, so prefer the error recorded beside the hook.
        if let Some(error) = self.limit_error.borrow_mut().take() {
            return Err(error);
        }
        result.map_err(map_lua_error)
    }

    /// The Lua handle backing this session.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Clears session-defined globals without touching the project.
    pub fn reset(&mut self) -> Result<(), ReplError> {
        self.globals
            .clear()
            .map_err(|error| ReplError::Runtime(error.to_string()))?;
        Ok(())
    }

    fn allowed_globals(&self) -> Vec<String> {
        let mut allowed: Vec<String> = crate::api::SAFE_GLOBALS
            .iter()
            .map(|name| name.to_string())
            .collect();
        allowed.extend(
            crate::api::REJECTED_GLOBALS
                .iter()
                .map(|name| name.to_string()),
        );
        allowed.push("mtrak".to_string());
        for (key, _) in self
            .globals
            .clone()
            .pairs::<String, mlua::Value>()
            .flatten()
        {
            // Fennel stores hyphenated globals under their mangled name, but
            // the compiler checks the name the user wrote.
            allowed.push(crate::api::fennel_global_unmangle(&key));
        }
        allowed
    }
}

fn map_lua_error(error: mlua::Error) -> ReplError {
    // Errors raised by a Rust callback arrive wrapped in a `CallbackError`
    // whose `Display` includes the callback traceback. Unwrap it so the
    // diagnostic is the original message rather than a doubled, buried one.
    let error = match error {
        mlua::Error::CallbackError { cause, .. } => cause.as_ref().clone(),
        other => other,
    };
    if let Some(external) = error.downcast_ref::<ReplError>() {
        match external {
            ReplError::Timeout => return ReplError::Timeout,
            ReplError::InstructionLimit => return ReplError::InstructionLimit,
            _ => {}
        }
    }
    match error {
        // `mlua` renders this variant as "runtime error: {message}", and
        // `ReplError::Runtime` adds the same prefix, so keep the bare message.
        mlua::Error::RuntimeError(message) => ReplError::Runtime(message),
        mlua::Error::MemoryError(_) => ReplError::MemoryLimit,
        other => ReplError::Runtime(other.to_string()),
    }
}
