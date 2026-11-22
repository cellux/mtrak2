//! Sandboxed Lua macro runtime.
//!
//! Modules are compiled with an empty `_ENV` that exposes only the injected API
//! and a curated standard library, so there is no filesystem, network, process,
//! `os`, `io`, `debug`, `package`, or FFI access (decision D-004). Each macro
//! runs as a coroutine that the scheduler resumes once per tick; instruction and
//! memory limits keep a runaway macro from corrupting engine state (D-026).
//!
//! The remaining-gaps milestone migrated the built-in macros to inspectable Lua
//! modules (`01` trigger, `02` Q8.8 tempo shorthand), made module `00` an
//! editable project/library module whose setup side effects run once, and added
//! numeric-hexadecimal `require` with caching and circular-dependency checks.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use mlua::{Function, HookTriggers, Lua, MultiValue, Table, Thread, ThreadStatus, Value, VmState};

use mtrak_core::composition::PatternId;
use mtrak_core::event::{macro_id, ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};
use mtrak_core::modules::BUILTIN_MODULES;
use mtrak_core::settings::{Scale, Settings};
use mtrak_core::timing::Ticks;

use crate::macros::{Macro, MacroAction, MacroCtx, MacroFactory, SettingChange};

/// Interval, in VM instructions, between instruction-budget hook checks.
const HOOK_INTERVAL: u32 = 1_000;

/// Globals copied into a module environment from the privileged state.
const SAFE_GLOBALS: &[&str] = &[
    "string", "table", "math", "pairs", "ipairs", "next", "type", "tostring", "tonumber", "error",
    "assert", "select", "pcall", "xpcall",
];

/// Errors from the Lua runtime.
#[derive(Debug, thiserror::Error)]
pub enum LuaError {
    /// An error propagated from the embedded Lua state.
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    /// The macro ID is not a user module ID (`40`–`7F`).
    #[error("macro id {0:#04X} is not a user macro id")]
    NotUserMacro(u8),
    /// No module was loaded for the macro ID.
    #[error("no module is loaded for macro id {0:#04X}")]
    UnknownModule(u8),
    /// Too many live coroutines are already running.
    #[error("the live coroutine limit has been reached")]
    TooManyCoroutines,
    /// The macro ID is reserved or an undefined built-in.
    #[error("macro id {0:#04X} is reserved or undefined")]
    ReservedMacro(u8),
}

/// A project command registered by the setup module.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegisteredCommand {
    /// The command name (without the leading colon).
    pub name: String,
    /// An optional human-readable description.
    pub description: Option<String>,
}

/// Deterministic resource limits for the Lua runtime.
#[derive(Clone, Copy, Debug)]
pub struct LuaLimits {
    /// Maximum Lua heap in bytes.
    pub memory_bytes: usize,
    /// Maximum VM instructions a single macro may execute per resume.
    pub instructions_per_tick: u32,
    /// Maximum simultaneously live macro coroutines.
    pub max_live_coroutines: usize,
}

impl Default for LuaLimits {
    fn default() -> Self {
        LuaLimits {
            memory_bytes: 16 * 1024 * 1024,
            instructions_per_tick: 1_000_000,
            max_live_coroutines: 64,
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    actions: Vec<MacroAction>,
    settings: Option<Settings>,
    track: usize,
    tick: Ticks,
}

enum ResumeOutcome {
    Wait(Ticks),
    Finished,
}

struct Inner {
    sources: HashMap<u8, String>,
    modules: HashMap<u8, Function>,
    cache: HashMap<u8, Value>,
    loading: HashSet<u8>,
    shared: Rc<RefCell<RuntimeState>>,
    commands: Rc<RefCell<Vec<RegisteredCommand>>>,
    handlers: Rc<RefCell<HashMap<String, Function>>>,
    fuel: Rc<Cell<u64>>,
    limits: LuaLimits,
    live: usize,
}

/// The shared Lua runtime used to create macro coroutines.
#[derive(Clone)]
pub struct LuaRuntime {
    lua: Lua,
    inner: Rc<RefCell<Inner>>,
}

impl LuaRuntime {
    /// Creates a new sandboxed runtime with the given limits and the built-in
    /// module sources pre-registered.
    pub fn new(limits: LuaLimits) -> Result<Self, LuaError> {
        let lua = Lua::new();
        lua.set_memory_limit(limits.memory_bytes)?;
        let mut sources = HashMap::new();
        for (id, source) in BUILTIN_MODULES {
            sources.insert(*id, (*source).to_string());
        }
        let inner = Inner {
            sources,
            modules: HashMap::new(),
            cache: HashMap::new(),
            loading: HashSet::new(),
            shared: Rc::new(RefCell::new(RuntimeState::default())),
            commands: Rc::new(RefCell::new(Vec::new())),
            handlers: Rc::new(RefCell::new(HashMap::new())),
            fuel: Rc::new(Cell::new(0)),
            limits,
            live: 0,
        };
        Ok(LuaRuntime {
            lua,
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    /// Stores a module source without evaluating it. Used for lazy loading.
    pub fn set_module_source(&self, id: u8, source: &str) {
        let mut inner = self.inner.borrow_mut();
        inner.sources.insert(id, source.to_string());
        inner.cache.remove(&id);
        inner.modules.remove(&id);
    }

    /// Loads a user macro module (`40`–`7F`) defining `run(arg1, arg2, ctx)`.
    ///
    /// The module is evaluated immediately so syntax errors surface at load.
    pub fn load_module(&self, id: u8, source: &str) -> Result<(), LuaError> {
        if !macro_id::is_user(id) {
            return Err(LuaError::NotUserMacro(id));
        }
        self.set_module_source(id, source);
        ensure_module(&self.lua, &self.inner, id, false).map(|_| ())
    }

    /// Executes the project setup module (`00`), which may register commands and
    /// perform one-time side effects. Its returned value is cached for
    /// `require(0x00)`.
    pub fn run_setup(&self, source: &str) -> Result<(), LuaError> {
        self.set_module_source(macro_id::SETUP, source);
        ensure_module(&self.lua, &self.inner, macro_id::SETUP, true).map(|_| ())
    }

    /// Clears every cached module value and re-evaluates all stored sources.
    ///
    /// Used by the explicit `F5` reload path in the Code screen.
    pub fn reload(&self) -> Result<(), LuaError> {
        {
            let mut inner = self.inner.borrow_mut();
            inner.cache.clear();
            inner.modules.clear();
            inner.loading.clear();
            inner.commands.borrow_mut().clear();
            inner.handlers.borrow_mut().clear();
        }
        self.validate()
    }

    /// Evaluates every stored module source, surfacing the first error.
    pub fn validate(&self) -> Result<(), LuaError> {
        let ids: Vec<u8> = {
            let inner = self.inner.borrow();
            let mut ids: Vec<u8> = inner.sources.keys().copied().collect();
            ids.sort_unstable();
            ids
        };
        for id in ids {
            let allow_register = id == macro_id::SETUP;
            ensure_module(&self.lua, &self.inner, id, allow_register)?;
        }
        Ok(())
    }

    /// The commands registered by the setup module.
    pub fn commands(&self) -> Vec<RegisteredCommand> {
        self.inner.borrow().commands.borrow().clone()
    }

    /// The cached return value of module `id`, if it has been loaded.
    pub fn module_value(&self, id: u8) -> Option<Value> {
        self.inner.borrow().cache.get(&id).cloned()
    }

    /// Whether a module source is available for `id`.
    pub fn has_module(&self, id: u8) -> bool {
        self.inner.borrow().sources.contains_key(&id)
    }

    /// Creates a macro coroutine for a built-in or user macro call.
    pub fn create_macro(&self, call: MacroCall) -> Result<LuaMacro, LuaError> {
        let id = call.id;
        let defined_builtin = macro_id::is_defined_builtin(id);
        if !defined_builtin && !macro_id::is_user(id) {
            return Err(LuaError::ReservedMacro(id));
        }
        {
            let inner = self.inner.borrow();
            if inner.live >= inner.limits.max_live_coroutines {
                return Err(LuaError::TooManyCoroutines);
            }
            if !inner.sources.contains_key(&id) {
                return Err(LuaError::UnknownModule(id));
            }
        }
        ensure_module(&self.lua, &self.inner, id, false)?;
        let run = self
            .inner
            .borrow()
            .modules
            .get(&id)
            .cloned()
            .ok_or(LuaError::UnknownModule(id))?;
        let body: Function = self
            .lua
            .load("local run = ...; return function(a1, a2, ctx) return run(a1, a2, ctx) end")
            .call(run)?;
        let thread = self.lua.create_thread(body)?;
        if self.inner.borrow().limits.instructions_per_tick > 0 {
            let fuel = self.inner.borrow().fuel.clone();
            let triggers = HookTriggers::new().every_nth_instruction(HOOK_INTERVAL);
            thread.set_hook(triggers, move |_lua, _debug| {
                let remaining = fuel.get();
                if remaining == 0 {
                    return Err(mlua::Error::RuntimeError(
                        "macro instruction budget exceeded".to_string(),
                    ));
                }
                fuel.set(remaining - 1);
                Ok(VmState::Continue)
            });
        }
        self.inner.borrow_mut().live += 1;
        Ok(LuaMacro {
            lua: self.lua.clone(),
            inner: self.inner.clone(),
            thread,
            arg1: call.arg1,
            arg2: call.arg2,
            started: false,
            done: false,
        })
    }
}

/// Evaluates a module source (once) and returns its cached return value.
fn ensure_module(
    lua: &Lua,
    inner: &Rc<RefCell<Inner>>,
    id: u8,
    allow_register: bool,
) -> Result<Value, LuaError> {
    {
        let state = inner.borrow();
        if let Some(value) = state.cache.get(&id) {
            return Ok(value.clone());
        }
        if state.loading.contains(&id) {
            return Err(LuaError::Lua(mlua::Error::RuntimeError(format!(
                "circular require of module {id:02X}"
            ))));
        }
        if !state.sources.contains_key(&id) {
            return Err(LuaError::UnknownModule(id));
        }
    }
    let source = inner
        .borrow()
        .sources
        .get(&id)
        .cloned()
        .ok_or(LuaError::UnknownModule(id))?;
    inner.borrow_mut().loading.insert(id);

    let outcome = (|| -> Result<Value, LuaError> {
        let env = make_env(lua, inner, allow_register)?;
        let value: Value = lua
            .load(&source)
            .set_name(format!("module {id:02X}"))
            .set_environment(env.clone())
            .eval()?;
        let run = match &value {
            Value::Function(function) => Some(function.clone()),
            Value::Table(table) => table.get::<Function>("run").ok(),
            Value::Nil => env.get::<Function>("run").ok(),
            _ => None,
        };
        {
            let mut state = inner.borrow_mut();
            if let Some(run) = run {
                state.modules.insert(id, run);
            }
            state.cache.insert(id, value.clone());
        }
        Ok(value)
    })();
    inner.borrow_mut().loading.remove(&id);
    outcome
}

fn make_env(
    lua: &Lua,
    inner: &Rc<RefCell<Inner>>,
    allow_register: bool,
) -> Result<Table, LuaError> {
    let env = lua.create_table()?;
    install_api(lua, inner, &env, allow_register)?;
    Ok(env)
}

fn install_api(
    lua: &Lua,
    inner: &Rc<RefCell<Inner>>,
    env: &Table,
    allow_register: bool,
) -> mlua::Result<()> {
    let (shared, commands, handlers) = {
        let state = inner.borrow();
        (
            state.shared.clone(),
            state.commands.clone(),
            state.handlers.clone(),
        )
    };

    let shared_emit = shared.clone();
    env.set(
        "emit",
        lua.create_function(move |_, args: MultiValue| {
            let mut it = args.into_iter();
            let status = it.next().and_then(|v| v.as_integer()).unwrap_or(0) as u8;
            let data1 = it.next().and_then(|v| v.as_integer()).map(|v| v as u8);
            let data2 = it.next().and_then(|v| v.as_integer()).map(|v| v as u8);
            shared_emit
                .borrow_mut()
                .actions
                .push(MacroAction::Emit(channel_event(status, data1, data2)));
            Ok(())
        })?,
    )?;

    let shared_sysex = shared.clone();
    env.set(
        "emit_sysex",
        lua.create_function(move |_, data: Vec<u8>| {
            shared_sysex
                .borrow_mut()
                .actions
                .push(MacroAction::Emit(Event::SysEx(data)));
            Ok(())
        })?,
    )?;

    let shared_meta = shared.clone();
    env.set(
        "emit_meta",
        lua.create_function(move |_, (kind, data): (u8, Vec<u8>)| {
            shared_meta
                .borrow_mut()
                .actions
                .push(MacroAction::Emit(Event::Meta(MetaEvent::from_parts(
                    kind, data,
                ))));
            Ok(())
        })?,
    )?;

    let shared_trigger = shared.clone();
    env.set(
        "trigger",
        lua.create_function(move |_, id: i64| {
            shared_trigger
                .borrow_mut()
                .actions
                .push(MacroAction::Trigger(PatternId(id as u8)));
            Ok(())
        })?,
    )?;

    let shared_stop = shared.clone();
    env.set(
        "stop_track",
        lua.create_function(move |_, include: Option<bool>| {
            shared_stop
                .borrow_mut()
                .actions
                .push(MacroAction::StopTrack {
                    include_current: include.unwrap_or(true),
                });
            Ok(())
        })?,
    )?;

    let shared_get = shared.clone();
    env.set(
        "get_setting",
        lua.create_function(move |_, name: String| {
            let settings = shared_get.borrow().settings;
            Ok(settings
                .map(|s| read_setting(s, &name))
                .unwrap_or(Value::Nil))
        })?,
    )?;

    let shared_set = shared.clone();
    env.set(
        "set_setting",
        lua.create_function(move |_, (name, value): (String, Value)| {
            let Some(change) = setting_change(&name, &value) else {
                return Ok(false);
            };
            let mut state = shared_set.borrow_mut();
            // Reflect the change immediately so later reads in the same resume
            // observe it; the engine applies it authoritatively.
            if let Some(settings) = state.settings.as_mut() {
                apply_change(settings, &change);
            }
            state.actions.push(MacroAction::SetSetting { change });
            Ok(true)
        })?,
    )?;

    let wait: Function = lua
        .load("return function(n) return coroutine.yield(n) end")
        .eval()?;
    env.set("wait", wait)?;

    let require_inner = inner.clone();
    env.set(
        "require",
        lua.create_function(move |lua, id: i64| {
            if !(0..=255).contains(&id) {
                return Err(mlua::Error::RuntimeError(
                    "require id must be in 0x00..=0xFF".to_string(),
                ));
            }
            ensure_module(lua, &require_inner, id as u8, false)
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))
        })?,
    )?;

    let globals = lua.globals();
    for name in SAFE_GLOBALS {
        if let Ok(value) = globals.get::<Value>(*name) {
            if !matches!(value, Value::Nil) {
                env.set(*name, value)?;
            }
        }
    }

    if allow_register {
        let commands = commands.clone();
        let handlers = handlers.clone();
        env.set(
            "register_command",
            lua.create_function(move |_, args: MultiValue| {
                let mut it = args.into_iter();
                let name = it.next().and_then(|v| v.as_string_lossy()).ok_or_else(|| {
                    mlua::Error::RuntimeError("register_command requires a name".to_string())
                })?;
                let description = it.next().and_then(|v| v.as_string_lossy());
                if let Some(Value::Function(handler)) = it.next() {
                    handlers.borrow_mut().insert(name.clone(), handler);
                }
                commands
                    .borrow_mut()
                    .push(RegisteredCommand { name, description });
                Ok(())
            })?,
        )?;
    }

    Ok(())
}

/// A macro coroutine backed by a Lua `Thread`.
pub struct LuaMacro {
    lua: Lua,
    inner: Rc<RefCell<Inner>>,
    thread: Thread,
    arg1: u8,
    arg2: u8,
    started: bool,
    done: bool,
}

impl Macro for LuaMacro {
    fn resume(&mut self, ctx: &mut MacroCtx) -> Option<Ticks> {
        if self.done {
            return None;
        }
        let outcome = self.resume_inner(ctx);
        self.started = true;
        match outcome {
            Ok((ResumeOutcome::Wait(ticks), actions)) => {
                for action in actions {
                    ctx.push(action);
                }
                Some(ticks)
            }
            Ok((ResumeOutcome::Finished, actions)) => {
                self.done = true;
                for action in actions {
                    ctx.push(action);
                }
                None
            }
            Err(error) => {
                self.done = true;
                ctx.push(MacroAction::Diagnostic(error.to_string()));
                None
            }
        }
    }
}

impl LuaMacro {
    fn resume_inner(
        &mut self,
        ctx: &mut MacroCtx,
    ) -> Result<(ResumeOutcome, Vec<MacroAction>), LuaError> {
        let shared = {
            let state = self.inner.borrow();
            state.shared.clone()
        };
        {
            let mut state = shared.borrow_mut();
            state.actions.clear();
            state.settings = Some(ctx.settings());
            state.track = ctx.track;
            state.tick = ctx.tick;
        }
        let steps = {
            let inner = self.inner.borrow();
            let fuel = inner.fuel.clone();
            let steps = (inner.limits.instructions_per_tick / HOOK_INTERVAL).max(1) as u64;
            fuel.set(steps);
            steps
        };
        let _ = steps;

        let result = if self.started {
            self.thread.resume::<MultiValue>(())
        } else {
            let context = context_table(&self.lua, ctx.settings(), ctx.track, ctx.tick)?;
            self.thread
                .resume::<MultiValue>((i64::from(self.arg1), i64::from(self.arg2), context))
        };
        let actions = std::mem::take(&mut shared.borrow_mut().actions);
        let finish = |inner: &Rc<RefCell<Inner>>| {
            let live = inner.borrow().live.saturating_sub(1);
            inner.borrow_mut().live = live;
        };
        match result {
            Ok(values) => {
                if self.thread.status() == ThreadStatus::Finished {
                    finish(&self.inner);
                    Ok((ResumeOutcome::Finished, actions))
                } else {
                    Ok((
                        ResumeOutcome::Wait(yield_ticks(&values, ctx.tpb())),
                        actions,
                    ))
                }
            }
            Err(mlua::Error::CoroutineUnresumable) => {
                finish(&self.inner);
                Ok((ResumeOutcome::Finished, actions))
            }
            Err(error) => {
                finish(&self.inner);
                Err(LuaError::Lua(error))
            }
        }
    }
}

/// A [`MacroFactory`] that creates Lua macro coroutines.
pub struct LuaMacroFactory {
    runtime: LuaRuntime,
}

impl LuaMacroFactory {
    /// Creates a factory backed by `runtime`.
    pub fn new(runtime: LuaRuntime) -> Self {
        LuaMacroFactory { runtime }
    }
}

impl MacroFactory for LuaMacroFactory {
    fn create(&mut self, call: MacroCall) -> Option<Box<dyn Macro>> {
        self.runtime
            .create_macro(call)
            .ok()
            .map(|m| Box::new(m) as Box<dyn Macro>)
    }
}

fn context_table(lua: &Lua, settings: Settings, track: usize, tick: Ticks) -> mlua::Result<Table> {
    let context = lua.create_table()?;
    context.set("track", track as i64)?;
    context.set("tick", tick as i64)?;
    let s = lua.create_table()?;
    s.set("root", i64::from(settings.root))?;
    s.set("scale", i64::from(settings.scale.mask()))?;
    s.set("modal_degree", i64::from(settings.modal_degree))?;
    s.set("scale_aware", settings.scale_aware)?;
    context.set("settings", s)?;
    Ok(context)
}

/// Converts a `wait` argument from beats into ticks.
///
/// `wait(n)` counts *beats*, not ticks, so a macro's timing is independent of
/// the file's TPB: the same source behaves identically in a TPB=24 project and
/// a TPB=960 one. Fractional beats are allowed, so `wait(0.25)` is a sixteenth
/// note in 4/4. The result is clamped to at least one tick, because a zero wait
/// would resume the coroutine forever within the same tick.
fn yield_ticks(values: &MultiValue, tpb: u32) -> Ticks {
    let beats = values
        .front()
        .and_then(|value| match value {
            Value::Integer(beats) => Some(*beats as f64),
            Value::Number(beats) => Some(*beats),
            _ => None,
        })
        .unwrap_or(1.0);
    let ticks = (beats * f64::from(tpb)).round();
    if ticks >= 1.0 {
        ticks as Ticks
    } else {
        1
    }
}

fn channel_event(status: u8, data1: Option<u8>, data2: Option<u8>) -> Event {
    match ChannelKind::from_status_nibble(status >> 4) {
        Some(kind) => Event::Channel(ChannelMessage {
            kind,
            channel: Some(status & 0x0F),
            data1,
            data2,
        }),
        None => {
            let mut bytes = vec![status];
            if let Some(d) = data1 {
                bytes.push(d);
            }
            if let Some(d) = data2 {
                bytes.push(d);
            }
            Event::Wire(bytes)
        }
    }
}

fn read_setting(settings: Settings, name: &str) -> Value {
    match name {
        "root" => Value::Integer(i64::from(settings.root)),
        "scale" => Value::Integer(i64::from(settings.scale.mask())),
        "modal_degree" | "mode" => Value::Integer(i64::from(settings.modal_degree)),
        "scale_aware" => Value::Boolean(settings.scale_aware),
        _ => Value::Nil,
    }
}

fn setting_change(name: &str, value: &Value) -> Option<SettingChange> {
    Some(match name {
        "root" => SettingChange::Root(value.as_integer()? as u8),
        "scale" => SettingChange::Scale(value.as_integer()? as u16),
        "modal_degree" | "mode" => SettingChange::ModalDegree(value.as_integer()? as u8),
        "scale_aware" => SettingChange::ScaleAware(value.as_boolean()?),
        _ => return None,
    })
}

/// Applies a [`SettingChange`] to a settings value.
pub fn apply_change(settings: &mut Settings, change: &SettingChange) {
    match change {
        SettingChange::Root(value) => settings.root = *value,
        SettingChange::Scale(mask) => settings.scale = Scale::from_mask(*mask),
        SettingChange::ModalDegree(value) => settings.modal_degree = *value,
        SettingChange::ScaleAware(value) => settings.scale_aware = *value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::Engine;
    use mtrak_core::composition::Composition;

    fn pattern_with_macro(id: u8, arg1: u8, arg2: u8) -> Composition {
        let mut composition = Composition::new();
        let p = composition.pattern_mut(PatternId(0)).unwrap();
        p.resize_rows(8).unwrap();
        p.resize_tracks(2).unwrap();
        p.set_event(0, 0, Event::Macro(MacroCall::new(id, arg1, arg2)));
        composition
    }

    fn run(composition: Composition, runtime: &LuaRuntime) -> Engine {
        let mut engine = Engine::new(composition);
        engine.set_macro_factory(Box::new(LuaMacroFactory::new(runtime.clone())));
        engine.start(PatternId(0));
        engine.run();
        engine
    }

    fn notes(engine: &Engine) -> Vec<(Ticks, u8, u8)> {
        engine
            .output()
            .iter()
            .filter_map(|te| match &te.event {
                Event::Channel(m) if m.kind == ChannelKind::NoteOn => {
                    Some((te.tick, m.channel.unwrap_or(0), m.data1.unwrap_or(0)))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sandbox_exposes_no_dangerous_globals() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x40,
                r#"
                return { run = function(a1, a2, ctx)
                    local names = {"os","io","debug","package","load","loadfile","dofile","collectgarbage","_G"}
                    local leaked = 0
                    for _, n in ipairs(names) do
                        if _ENV[n] ~= nil then leaked = leaked + 1 end
                    end
                    emit(0x90, leaked, 1)
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x40, 60, 0), &runtime);
        assert_eq!(
            notes(&engine),
            vec![(0, 0, 0)],
            "no forbidden global may be visible"
        );
    }

    #[test]
    fn macros_emit_and_wait_in_beats() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x41,
                r#"
                return { run = function(a1, a2, ctx)
                    emit(0x90, a1, 100)
                    wait(4)
                    emit(0x91, a2, 100)
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x41, 60, 62), &runtime);
        // TPB is 24, so wait(4) is four beats, i.e. 96 ticks.
        assert_eq!(notes(&engine), vec![(0, 0, 60), (96, 1, 62)]);
        assert!(engine.diagnostics().is_empty());
    }

    #[test]
    fn wait_is_independent_of_tpb() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x44,
                r#"
                return { run = function(a1, a2, ctx)
                    emit(0x90, a1, 100)
                    wait(1)
                    emit(0x91, a2, 100)
                end }
                "#,
            )
            .unwrap();
        let mut positions = Vec::new();
        for tpb in [24u32, 96] {
            let mut composition = pattern_with_macro(0x44, 60, 62);
            composition.raise_tpb(tpb).unwrap();
            let engine = run(composition, &runtime);
            positions.push(
                notes(&engine)
                    .into_iter()
                    .map(|(tick, _, _)| tick)
                    .collect::<Vec<_>>(),
            );
        }
        // One beat apart in both projects, so the tick gap scales with TPB
        // instead of the wait shrinking as the resolution grows.
        assert_eq!(positions[0], vec![0, 24]);
        assert_eq!(positions[1], vec![0, 96]);
    }

    #[test]
    fn stop_track_false_keeps_the_lua_caller_alive() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x42,
                r#"
                return { run = function(a1, a2, ctx)
                    stop_track(false)
                    wait(2)
                    emit(0x90, 99, 1)
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x42, 0, 0), &runtime);
        // wait(2) is two beats, i.e. 48 ticks at TPB 24.
        assert_eq!(notes(&engine), vec![(48, 0, 99)]);
    }

    #[test]
    fn stop_track_true_stops_the_lua_caller() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x43,
                r#"
                return { run = function(a1, a2, ctx)
                    stop_track(true)
                    wait(2)
                    emit(0x90, 99, 1)
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x43, 0, 0), &runtime);
        assert!(notes(&engine).is_empty());
        assert!(!engine.overflowed());
    }

    #[test]
    fn setup_module_registers_commands() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .run_setup(r#"register_command("hello", "says hi")"#)
            .unwrap();
        let commands = runtime.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "hello");
        assert_eq!(commands[0].description.as_deref(), Some("says hi"));
    }

    #[test]
    fn macro_errors_are_isolated_and_reported() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x44,
                r#"return { run = function(a1, a2, ctx) error("boom") end }"#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x44, 0, 0), &runtime);
        assert!(engine.diagnostics().iter().any(|d| d.contains("boom")));
        assert!(!engine.overflowed());
    }

    #[test]
    fn runaway_macro_hits_the_instruction_budget() {
        let limits = LuaLimits {
            instructions_per_tick: 20_000,
            ..LuaLimits::default()
        };
        let runtime = LuaRuntime::new(limits).unwrap();
        runtime
            .load_module(
                0x45,
                r#"return { run = function(a1, a2, ctx) while true do end end }"#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x45, 0, 0), &runtime);
        assert!(engine
            .diagnostics()
            .iter()
            .any(|d| d.contains("instruction budget")));
        assert!(!engine.overflowed());
    }

    #[test]
    fn macros_can_read_and_write_settings() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(
                0x46,
                r#"
                return { run = function(a1, a2, ctx)
                    local root = get_setting("root")
                    set_setting("root", root + 2)
                    emit(0x90, get_setting("root"), 1)
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x46, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 62)]);
    }

    #[test]
    fn registering_commands_needs_setup() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        // A macro module has no `register_command`.
        runtime
            .load_module(
                0x47,
                r#"
                return { run = function(a1, a2, ctx)
                    if register_command == nil then
                        emit(0x90, 1, 1)
                    end
                end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x47, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 1)]);
    }

    #[test]
    fn builtin_trigger_and_tempo_modules_work() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        // Build a parent with a built-in trigger to a child pattern.
        let mut composition = Composition::new();
        let child = composition.create_pattern(4).unwrap();
        {
            let p = composition.pattern_mut(child).unwrap();
            p.resize_rows(1).unwrap();
            p.set_event(
                0,
                0,
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    0,
                    72,
                    Some(100),
                )),
            );
        }
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.resize_rows(2).unwrap();
            p.set_event(0, 0, Event::Macro(MacroCall::new(1, child.value(), 0)));
            p.set_event(1, 0, Event::Macro(MacroCall::new(2, 240, 0)));
        }
        let engine = run(composition, &runtime);
        assert!(notes(&engine).iter().any(|(_, _, key)| *key == 72));
        assert!(engine
            .output()
            .iter()
            .any(|te| matches!(&te.event, Event::Meta(MetaEvent::Tempo(m)) if *m == 250_000)));
    }

    #[test]
    fn require_loads_and_caches_modules_and_detects_cycles() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime.set_module_source(0x50, "return { value = 7 }");
        runtime
            .load_module(
                0x51,
                r#"
                local util = require(0x50)
                return { run = function(a1, a2, ctx) emit(0x90, util.value, 1) end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x51, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 7)]);

        // Circular dependency is a deterministic diagnostic.
        let cyclic = LuaRuntime::new(LuaLimits::default()).unwrap();
        cyclic.set_module_source(0x52, "return require(0x53)");
        cyclic.set_module_source(0x53, "return require(0x52)");
        let error = cyclic.validate().unwrap_err().to_string();
        assert!(error.contains("circular"), "{error}");
    }

    #[test]
    fn require_reports_unavailable_ids() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .load_module(0x54, "return require(0x7E)")
            .unwrap_err();
        // The error is raised during module evaluation, so load_module errors.
        assert!(!runtime.has_module(0x7E));
    }

    #[test]
    fn module_zero_setup_side_effects_run_once() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime
            .run_setup(
                r#"
                register_command("hello")
                return { greeting = "hi" }
                "#,
            )
            .unwrap();
        assert_eq!(runtime.commands().len(), 1);
        let value = runtime.module_value(0).expect("cached");
        assert!(matches!(value, Value::Table(_)));
    }

    #[test]
    fn require_returns_any_cached_value() {
        let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
        runtime.set_module_source(0x55, "return 42");
        runtime
            .load_module(
                0x56,
                r#"
                local n = require(0x55)
                return { run = function(a1, a2, ctx) emit(0x90, n, 1) end }
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x56, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 42)]);
    }
}
