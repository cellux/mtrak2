//! Sandboxed Lua macro runtime (phase P5).
//!
//! Modules are compiled with an empty `_ENV` that exposes only the injected API
//! and a curated standard library, so there is no filesystem, network, process,
//! `os`, `io`, `debug`, `package`, or FFI access (decision D-004). Each macro
//! runs as a coroutine that the scheduler resumes once per tick; instruction and
//! memory limits keep a runaway macro from corrupting engine state (D-026).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use mlua::{Function, HookTriggers, Lua, MultiValue, Table, Thread, ThreadStatus, Value, VmState};

use mtrak_core::composition::PatternId;
use mtrak_core::event::{macro_id, ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};
use mtrak_core::settings::{EffectiveSettings, Scale, Setting, Settings};
use mtrak_core::timing::Ticks;

use crate::macros::{Macro, MacroAction, MacroCtx, MacroFactory, SettingChange, SettingScope};

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
    /// The macro ID is not in the user-authorable range (`40`–`7F`).
    #[error("macro id {0:#04X} is not a user macro id")]
    NotUserMacro(u8),
    /// No module was loaded for the macro ID.
    #[error("no module is loaded for macro id {0:#04X}")]
    UnknownModule(u8),
    /// Too many live coroutines are already running.
    #[error("the live coroutine limit has been reached")]
    TooManyCoroutines,
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
    settings: Option<EffectiveSettings>,
    track: usize,
    tick: Ticks,
}

struct ResumeInput<'a> {
    thread: &'a Thread,
    started: bool,
    arg1: u8,
    arg2: u8,
    tick: Ticks,
    track: usize,
    settings: EffectiveSettings,
}

enum ResumeOutcome {
    Wait(Ticks),
    Finished,
}

struct Inner {
    lua: Lua,
    modules: HashMap<u8, Function>,
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
    inner: Rc<RefCell<Inner>>,
}

impl LuaRuntime {
    /// Creates a new sandboxed runtime with the given limits.
    pub fn new(limits: LuaLimits) -> Result<Self, LuaError> {
        let lua = Lua::new();
        lua.set_memory_limit(limits.memory_bytes)?;
        let inner = Inner {
            lua,
            modules: HashMap::new(),
            shared: Rc::new(RefCell::new(RuntimeState::default())),
            commands: Rc::new(RefCell::new(Vec::new())),
            handlers: Rc::new(RefCell::new(HashMap::new())),
            fuel: Rc::new(Cell::new(0)),
            limits,
            live: 0,
        };
        Ok(LuaRuntime {
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    /// Loads a user macro module (`40`–`7F`) defining `run(arg1, arg2, context)`.
    pub fn load_module(&self, id: u8, source: &str) -> Result<(), LuaError> {
        if !macro_id::is_user(id) {
            return Err(LuaError::NotUserMacro(id));
        }
        let mut inner = self.inner.borrow_mut();
        let env = inner.make_env(false)?;
        let name = format!("macro {id:02X}");
        let result: Value = inner
            .lua
            .load(source)
            .set_name(name)
            .set_environment(env.clone())
            .eval()?;
        let run = match result {
            Value::Function(f) => f,
            Value::Table(t) => t.get::<Function>("run")?,
            Value::Nil => env.get::<Function>("run")?,
            _ => return Err(LuaError::UnknownModule(id)),
        };
        inner.modules.insert(id, run);
        Ok(())
    }

    /// Executes the project setup module (`00`), which may register commands.
    pub fn run_setup(&self, source: &str) -> Result<(), LuaError> {
        let inner = self.inner.borrow_mut();
        let env = inner.make_env(true)?;
        inner
            .lua
            .load(source)
            .set_name("setup")
            .set_environment(env)
            .exec()?;
        Ok(())
    }

    /// The commands registered by the setup module.
    pub fn commands(&self) -> Vec<RegisteredCommand> {
        self.inner.borrow().commands.borrow().clone()
    }

    /// Creates a macro coroutine for a user macro call.
    pub fn create_macro(&self, call: MacroCall) -> Result<LuaMacro, LuaError> {
        if !macro_id::is_user(call.id) {
            return Err(LuaError::NotUserMacro(call.id));
        }
        let mut inner = self.inner.borrow_mut();
        if inner.live >= inner.limits.max_live_coroutines {
            return Err(LuaError::TooManyCoroutines);
        }
        let run = inner
            .modules
            .get(&call.id)
            .cloned()
            .ok_or(LuaError::UnknownModule(call.id))?;
        let body: Function = inner
            .lua
            .load("local run = ...; return function(a1, a2, ctx) return run(a1, a2, ctx) end")
            .call(run)?;
        let thread = inner.lua.create_thread(body)?;
        if inner.limits.instructions_per_tick > 0 {
            let fuel = inner.fuel.clone();
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
        inner.live += 1;
        Ok(LuaMacro {
            inner: self.inner.clone(),
            thread,
            arg1: call.arg1,
            arg2: call.arg2,
            started: false,
            done: false,
        })
    }
}

impl Inner {
    fn make_env(&self, allow_register: bool) -> mlua::Result<Table> {
        let env = self.lua.create_table()?;
        self.install_api(&env, allow_register)?;
        Ok(env)
    }

    fn install_api(&self, env: &Table, allow_register: bool) -> mlua::Result<()> {
        let shared = self.shared.clone();
        env.set(
            "emit",
            self.lua.create_function(move |_, args: MultiValue| {
                let mut it = args.into_iter();
                let status = it.next().and_then(|v| v.as_integer()).unwrap_or(0) as u8;
                let data1 = it.next().and_then(|v| v.as_integer()).map(|v| v as u8);
                let data2 = it.next().and_then(|v| v.as_integer()).map(|v| v as u8);
                shared
                    .borrow_mut()
                    .actions
                    .push(MacroAction::Emit(channel_event(status, data1, data2)));
                Ok(())
            })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "emit_sysex",
            self.lua.create_function(move |_, data: Vec<u8>| {
                shared
                    .borrow_mut()
                    .actions
                    .push(MacroAction::Emit(Event::SysEx(data)));
                Ok(())
            })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "emit_meta",
            self.lua
                .create_function(move |_, (kind, data): (u8, Vec<u8>)| {
                    shared
                        .borrow_mut()
                        .actions
                        .push(MacroAction::Emit(Event::Meta(MetaEvent::from_parts(
                            kind, data,
                        ))));
                    Ok(())
                })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "trigger",
            self.lua.create_function(move |_, id: i64| {
                shared
                    .borrow_mut()
                    .actions
                    .push(MacroAction::Trigger(PatternId(id as u8)));
                Ok(())
            })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "stop_track",
            self.lua.create_function(move |_, include: Option<bool>| {
                shared.borrow_mut().actions.push(MacroAction::StopTrack {
                    include_current: include.unwrap_or(true),
                });
                Ok(())
            })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "get_setting",
            self.lua.create_function(move |_, name: String| {
                let settings = shared.borrow().settings;
                Ok(settings
                    .map(|s| read_setting(s, &name))
                    .unwrap_or(Value::Nil))
            })?,
        )?;

        let shared = self.shared.clone();
        env.set(
            "set_setting",
            self.lua.create_function(
                move |_, (name, value, scope): (String, Value, Option<String>)| {
                    let Some(change) = setting_change(&name, &value) else {
                        return Ok(false);
                    };
                    let scope = match scope.as_deref() {
                        Some("global") => SettingScope::Global,
                        _ => SettingScope::Pattern,
                    };
                    let mut state = shared.borrow_mut();
                    // Reflect the change immediately so later reads in the same
                    // resume observe it; the engine applies it authoritatively.
                    if let Some(settings) = state.settings.as_mut() {
                        match change {
                            SettingChange::Root(v) => settings.root = v,
                            SettingChange::Scale(mask) => {
                                settings.scale = Scale::from_mask(mask);
                            }
                            SettingChange::ModalDegree(v) => settings.modal_degree = v,
                            SettingChange::ScaleAware(v) => settings.scale_aware = v,
                        }
                    }
                    state
                        .actions
                        .push(MacroAction::SetSetting { scope, change });
                    Ok(true)
                },
            )?,
        )?;

        let wait: Function = self
            .lua
            .load("return function(n) return coroutine.yield(n) end")
            .eval()?;
        env.set("wait", wait)?;

        let globals = self.lua.globals();
        for name in SAFE_GLOBALS {
            if let Ok(value) = globals.get::<Value>(*name) {
                if !matches!(value, Value::Nil) {
                    env.set(*name, value)?;
                }
            }
        }

        if allow_register {
            let commands = self.commands.clone();
            let handlers = self.handlers.clone();
            env.set(
                "register_command",
                self.lua.create_function(move |_, args: MultiValue| {
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

    fn resume(
        &mut self,
        input: ResumeInput<'_>,
    ) -> Result<(ResumeOutcome, Vec<MacroAction>), LuaError> {
        let ResumeInput {
            thread,
            started,
            arg1,
            arg2,
            tick,
            track,
            settings,
        } = input;
        {
            let mut state = self.shared.borrow_mut();
            state.actions.clear();
            state.settings = Some(settings);
            state.track = track;
            state.tick = tick;
        }
        let steps = (self.limits.instructions_per_tick / HOOK_INTERVAL).max(1) as u64;
        self.fuel.set(steps);

        let result = if started {
            thread.resume::<MultiValue>(())
        } else {
            let context = context_table(&self.lua, settings, track, tick)?;
            thread.resume::<MultiValue>((i64::from(arg1), i64::from(arg2), context))
        };
        let actions = std::mem::take(&mut self.shared.borrow_mut().actions);
        match result {
            Ok(values) => {
                if thread.status() == ThreadStatus::Finished {
                    self.live = self.live.saturating_sub(1);
                    Ok((ResumeOutcome::Finished, actions))
                } else {
                    Ok((ResumeOutcome::Wait(yield_ticks(&values)), actions))
                }
            }
            Err(mlua::Error::CoroutineUnresumable) => {
                self.live = self.live.saturating_sub(1);
                Ok((ResumeOutcome::Finished, actions))
            }
            Err(error) => {
                self.live = self.live.saturating_sub(1);
                Err(LuaError::Lua(error))
            }
        }
    }
}

/// A macro coroutine backed by a Lua `Thread`.
pub struct LuaMacro {
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
        let outcome = {
            let mut inner = self.inner.borrow_mut();
            inner.resume(ResumeInput {
                thread: &self.thread,
                started: self.started,
                arg1: self.arg1,
                arg2: self.arg2,
                tick: ctx.tick,
                track: ctx.track,
                settings: ctx.settings(),
            })
        };
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

fn context_table(
    lua: &Lua,
    settings: EffectiveSettings,
    track: usize,
    tick: Ticks,
) -> mlua::Result<Table> {
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

fn yield_ticks(values: &MultiValue) -> Ticks {
    values
        .front()
        .and_then(Value::as_integer)
        .map(|v| v.max(0) as Ticks)
        .unwrap_or(1)
        .max(1)
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
            Event::Raw(bytes)
        }
    }
}

fn read_setting(settings: EffectiveSettings, name: &str) -> Value {
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

/// Convenience helper: apply a [`SettingChange`] to a settings layer.
pub fn apply_setting(settings: &mut Settings, change: SettingChange) {
    match change {
        SettingChange::Root(value) => settings.root = Setting::Set(value),
        SettingChange::Scale(mask) => settings.scale = Setting::Set(Scale::from_mask(mask)),
        SettingChange::ModalDegree(value) => settings.modal_degree = Setting::Set(value),
        SettingChange::ScaleAware(value) => settings.scale_aware = Setting::Set(value),
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
                    local names = {"os","io","debug","package","require","load","loadfile","dofile","collectgarbage","_G"}
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
    fn macros_emit_and_wait_in_global_ticks() {
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
        assert_eq!(notes(&engine), vec![(0, 0, 60), (4, 1, 62)]);
        assert!(engine.diagnostics().is_empty());
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
        assert_eq!(notes(&engine), vec![(2, 0, 99)]);
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
                    set_setting("root", root + 2, "pattern")
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
}
