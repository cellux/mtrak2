//! Sandboxed Fennel macro runtime.
//!
//! Every module is compiled from Fennel to correlated Lua by the separate,
//! restricted [`FennelCompiler`](crate::fennel::FennelCompiler) state, then the
//! generated Lua is evaluated in an empty `_ENV` that exposes only the injected
//! API and a curated standard library. There is therefore no filesystem,
//! network, process, `os`, `io`, `debug`, `package`, or FFI access (decision
//! D-004), and Fennel syntax cannot bypass the sandbox (D-102). Each macro runs
//! as a coroutine that the scheduler resumes once per tick; instruction and
//! memory limits keep a runaway macro from corrupting engine state (D-026).
//!
//! [`MacroRuntime`] owns both the compiler and the Lua execution state. The
//! built-in macros (`01` trigger, `02` Q8.8 tempo shorthand), the module `00`
//! setup/library module, and numeric-hexadecimal `require` with caching and
//! circular-dependency checks all keep their existing semantics.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use mlua::{Function, HookTriggers, Lua, MultiValue, Table, Thread, ThreadStatus, Value, VmState};

use mtrak_core::composition::PatternId;
use mtrak_core::event::{macro_id, ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};
use mtrak_core::modules::BUILTIN_MODULES;
use mtrak_core::settings::{Scale, Settings};
use mtrak_core::timing::Ticks;

use crate::fennel::{fennel_global_name, FennelCompiler};
use crate::macros::{Macro, MacroAction, MacroCtx, MacroFactory, SettingChange};
use crate::rng::{self, SplitMix64};

/// Interval, in VM instructions, between instruction-budget hook checks.
const HOOK_INTERVAL: u32 = 1_000;

/// Globals copied into a module environment from the privileged state.
const SAFE_GLOBALS: &[&str] = &[
    "string", "table", "math", "pairs", "ipairs", "next", "type", "tostring", "tonumber", "error",
    "assert", "select", "pcall", "xpcall",
];

/// Errors from the Fennel/Lua macro runtime.
#[derive(Debug, thiserror::Error)]
pub enum MacroError {
    /// An error propagated from the embedded Lua state.
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    /// The embedded Fennel compiler could not be initialized.
    #[error("fennel compiler initialization failed: {0}")]
    Compiler(String),
    /// The Fennel source could not be compiled for a module.
    #[error("fennel compile error in module {id:02X}: {message}")]
    Compile {
        /// The module whose source failed to compile.
        id: u8,
        /// The Fennel diagnostic, including the synthetic source name and line.
        message: String,
    },
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

/// Deterministic resource limits for the Lua execution state.
#[derive(Clone, Copy, Debug)]
pub struct MacroLimits {
    /// Maximum Lua heap in bytes.
    pub memory_bytes: usize,
    /// Maximum VM instructions a single macro may execute per resume.
    pub instructions_per_tick: u32,
    /// Maximum simultaneously live macro coroutines.
    pub max_live_coroutines: usize,
}

impl Default for MacroLimits {
    fn default() -> Self {
        MacroLimits {
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
    /// The composition-seeded global series behind `math.random`.
    rng: Rc<RefCell<SplitMix64>>,
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
    limits: MacroLimits,
    live: usize,
}

/// The shared Fennel compiler and Lua execution runtime used to create macro
/// coroutines.
#[derive(Clone)]
pub struct MacroRuntime {
    lua: Lua,
    compiler: FennelCompiler,
    inner: Rc<RefCell<Inner>>,
}

impl MacroRuntime {
    /// Creates a new sandboxed runtime with the given limits and the built-in
    /// module sources pre-registered.
    pub fn new(limits: MacroLimits) -> Result<Self, MacroError> {
        let compiler =
            FennelCompiler::new().map_err(|error| MacroError::Compiler(error.message))?;
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
        Ok(MacroRuntime {
            lua,
            compiler,
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    /// The embedded compiler's reported Fennel version.
    pub fn fennel_version(&self) -> String {
        self.compiler
            .version()
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Reseeds the global random series.
    ///
    /// Called once per render with the composition's seed, so a render is a
    /// pure function of the saved project.
    pub fn set_seed(&self, seed: u64) {
        let shared = self.inner.borrow().shared.clone();
        shared.borrow().rng.borrow_mut().reseed(seed);
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
    pub fn load_module(&self, id: u8, source: &str) -> Result<(), MacroError> {
        if !macro_id::is_user(id) {
            return Err(MacroError::NotUserMacro(id));
        }
        self.set_module_source(id, source);
        ensure_module(&self.lua, &self.inner, &self.compiler, id, false).map(|_| ())
    }

    /// Executes the project setup module (`00`), which may register commands and
    /// perform one-time side effects. Its returned value is cached for
    /// `require(0x00)`.
    pub fn run_setup(&self, source: &str) -> Result<(), MacroError> {
        self.set_module_source(macro_id::SETUP, source);
        ensure_module(
            &self.lua,
            &self.inner,
            &self.compiler,
            macro_id::SETUP,
            true,
        )
        .map(|_| ())
    }

    /// Clears every cached module value and re-evaluates all stored sources.
    ///
    /// Used by the explicit `F5` reload path in the Code screen.
    pub fn reload(&self) -> Result<(), MacroError> {
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
    pub fn validate(&self) -> Result<(), MacroError> {
        let ids: Vec<u8> = {
            let inner = self.inner.borrow();
            let mut ids: Vec<u8> = inner.sources.keys().copied().collect();
            ids.sort_unstable();
            ids
        };
        for id in ids {
            let allow_register = id == macro_id::SETUP;
            ensure_module(&self.lua, &self.inner, &self.compiler, id, allow_register)?;
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
    pub fn create_macro(&self, call: MacroCall) -> Result<LuaMacro, MacroError> {
        let id = call.id;
        let defined_builtin = macro_id::is_defined_builtin(id);
        if !defined_builtin && !macro_id::is_user(id) {
            return Err(MacroError::ReservedMacro(id));
        }
        {
            let inner = self.inner.borrow();
            if inner.live >= inner.limits.max_live_coroutines {
                return Err(MacroError::TooManyCoroutines);
            }
            if !inner.sources.contains_key(&id) {
                return Err(MacroError::UnknownModule(id));
            }
        }
        ensure_module(&self.lua, &self.inner, &self.compiler, id, false)?;
        let run = self
            .inner
            .borrow()
            .modules
            .get(&id)
            .cloned()
            .ok_or(MacroError::UnknownModule(id))?;
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

/// Compiles and evaluates a module source (once) and returns its cached value.
fn ensure_module(
    lua: &Lua,
    inner: &Rc<RefCell<Inner>>,
    compiler: &FennelCompiler,
    id: u8,
    allow_register: bool,
) -> Result<Value, MacroError> {
    {
        let state = inner.borrow();
        if let Some(value) = state.cache.get(&id) {
            return Ok(value.clone());
        }
        if state.loading.contains(&id) {
            return Err(MacroError::Lua(mlua::Error::RuntimeError(format!(
                "circular require of module {id:02X}"
            ))));
        }
        if !state.sources.contains_key(&id) {
            return Err(MacroError::UnknownModule(id));
        }
    }
    let source = inner
        .borrow()
        .sources
        .get(&id)
        .cloned()
        .ok_or(MacroError::UnknownModule(id))?;
    inner.borrow_mut().loading.insert(id);

    let outcome = (|| -> Result<Value, MacroError> {
        // Compile first: a Fennel error is a load/validation error and must not
        // leave a cached module or a stale loading marker behind.
        let lua_source = compiler
            .compile(&source, id, allow_register)
            .map_err(|error| MacroError::Compile {
                id,
                message: error.message,
            })?;
        let env = make_env(lua, inner, compiler, allow_register)?;
        let value: Value = lua
            .load(&lua_source)
            .set_name(crate::fennel::source_name(id))
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
    compiler: &FennelCompiler,
    allow_register: bool,
) -> Result<Table, MacroError> {
    let env = lua.create_table()?;
    install_api(lua, inner, compiler, &env, allow_register)?;
    Ok(env)
}

/// Binds an API value under both its Fennel name and its mangled global name.
///
/// Fennel compiles kebab-case globals such as `get-setting` to the mangled
/// identifier `__fnl_global__get_2dsetting`, so the execution environment must
/// expose the mangled spelling for generated Lua to resolve it.
fn bind<V>(env: &Table, name: &str, value: V) -> mlua::Result<()>
where
    V: mlua::IntoLua + Clone,
{
    env.set(name, value.clone())?;
    let mangled = fennel_global_name(name);
    if mangled != name {
        env.set(mangled, value)?;
    }
    Ok(())
}

fn install_api(
    lua: &Lua,
    inner: &Rc<RefCell<Inner>>,
    compiler: &FennelCompiler,
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
    bind(
        env,
        "emit-sysex",
        lua.create_function(move |_, data: Vec<u8>| {
            shared_sysex
                .borrow_mut()
                .actions
                .push(MacroAction::Emit(Event::SysEx(data)));
            Ok(())
        })?,
    )?;

    let shared_meta = shared.clone();
    bind(
        env,
        "emit-meta",
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
    bind(
        env,
        "stop-track",
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
    bind(
        env,
        "get-setting",
        lua.create_function(move |_, name: String| {
            let settings = shared_get.borrow().settings;
            Ok(settings
                .map(|s| read_setting(s, &name))
                .unwrap_or(Value::Nil))
        })?,
    )?;

    let shared_set = shared.clone();
    bind(
        env,
        "set-setting",
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
    let require_compiler = compiler.clone();
    env.set(
        "require",
        lua.create_function(move |lua, id: i64| {
            if !(0..=255).contains(&id) {
                return Err(mlua::Error::RuntimeError(
                    "require id must be in 0x00..=0xFF".to_string(),
                ));
            }
            ensure_module(lua, &require_inner, &require_compiler, id as u8, false)
                .map_err(|error| mlua::Error::RuntimeError(error.to_string()))
        })?,
    )?;

    // The RNG lives in the shared state so every module draws from the same
    // composition-seeded series.
    let global_rng = shared.borrow().rng.clone();
    rng::install(lua, env, global_rng)?;

    let globals = lua.globals();
    for name in SAFE_GLOBALS {
        // `math` is installed by `rng::install` with its random entries
        // replaced, so the stock table must not be copied over it.
        if *name == "math" {
            continue;
        }
        if let Ok(value) = globals.get::<Value>(*name) {
            if !matches!(value, Value::Nil) {
                env.set(*name, value)?;
            }
        }
    }

    if allow_register {
        let commands = commands.clone();
        let handlers = handlers.clone();
        bind(
            env,
            "register-command",
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
    ) -> Result<(ResumeOutcome, Vec<MacroAction>), MacroError> {
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
            let context =
                context_table(&self.lua, ctx.settings(), ctx.track, ctx.tick, ctx.seed())?;
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
                Err(MacroError::Lua(error))
            }
        }
    }
}

/// A [`MacroFactory`] that creates Lua macro coroutines.
pub struct RuntimeMacroFactory {
    runtime: MacroRuntime,
}

impl RuntimeMacroFactory {
    /// Creates a factory backed by `runtime`.
    pub fn new(runtime: MacroRuntime) -> Self {
        RuntimeMacroFactory { runtime }
    }
}

impl MacroFactory for RuntimeMacroFactory {
    fn set_seed(&mut self, seed: u64) {
        self.runtime.set_seed(seed);
    }

    fn create(&mut self, call: MacroCall) -> Option<Box<dyn Macro>> {
        self.runtime
            .create_macro(call)
            .ok()
            .map(|m| Box::new(m) as Box<dyn Macro>)
    }
}

fn context_table(
    lua: &Lua,
    settings: Settings,
    track: usize,
    tick: Ticks,
    seed: u64,
) -> mlua::Result<Table> {
    let context = lua.create_table()?;
    context.set("track", track as i64)?;
    context.set("tick", tick as i64)?;
    context.set("seed", seed as i64)?;
    let s = lua.create_table()?;
    s.set("root", i64::from(settings.root))?;
    s.set("scale", i64::from(settings.scale.mask()))?;
    s.set("modal_degree", i64::from(settings.modal_degree))?;
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
        _ => Value::Nil,
    }
}

fn setting_change(name: &str, value: &Value) -> Option<SettingChange> {
    Some(match name {
        "root" => SettingChange::Root(value.as_integer()? as u8),
        "scale" => SettingChange::Scale(value.as_integer()? as u16),
        "modal_degree" | "mode" => SettingChange::ModalDegree(value.as_integer()? as u8),
        _ => return None,
    })
}

/// Applies a [`SettingChange`] to a settings value.
pub fn apply_change(settings: &mut Settings, change: &SettingChange) {
    match change {
        SettingChange::Root(value) => settings.root = *value,
        SettingChange::Scale(mask) => settings.scale = Scale::from_mask(*mask),
        SettingChange::ModalDegree(value) => settings.modal_degree = *value,
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

    fn run(composition: Composition, runtime: &MacroRuntime) -> Engine {
        let mut engine = Engine::new(composition);
        engine.set_macro_factory(Box::new(RuntimeMacroFactory::new(runtime.clone())));
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
    fn sandbox_rejects_and_hides_dangerous_globals() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        // A forbidden global is rejected during compilation, not at runtime.
        let error = runtime
            .load_module(0x40, r#"(fn run [a1 a2 ctx] (os.execute "ls"))"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown identifier: os"), "{error}");

        // And it is not visible in the execution environment either.
        runtime
            .load_module(
                0x40,
                r#"
                (fn run [a1 a2 ctx]
                  (var leaked 0)
                  (each [_ n (ipairs ["os" "io" "debug" "package" "load"
                                      "loadfile" "dofile" "collectgarbage" "_G"])]
                    (when (. _ENV n) (set leaked (+ leaked 1))))
                  (emit 0x90 leaked 1))
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
    fn compile_errors_report_the_fennel_source_and_cache_nothing() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        let error = runtime
            .load_module(0x40, "(fn run [a1]\n  (emit 0x90 a1")
            .unwrap_err()
            .to_string();
        assert!(error.contains("module 40.fnl"), "{error}");
        assert!(error.contains("Parse error"), "{error}");
        // A failed compilation must not leave a cached value behind.
        assert!(runtime.module_value(0x40).is_none());

        // The same module id can be loaded successfully afterwards.
        runtime
            .load_module(0x40, "(fn run [a1 a2 ctx] (emit 0x90 a1 100))")
            .unwrap();
        let engine = run(pattern_with_macro(0x40, 60, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 60)]);
    }

    #[test]
    fn macros_emit_and_wait_in_beats() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x41,
                r#"
                (fn run [a1 a2 ctx]
                  (emit 0x90 a1 100)
                  (wait 4)
                  (emit 0x91 a2 100))
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
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x44,
                r#"
                (fn run [a1 a2 ctx]
                  (emit 0x90 a1 100)
                  (wait 1)
                  (emit 0x91 a2 100))
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
    fn stop_track_false_keeps_the_macro_caller_alive() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x42,
                r#"
                (fn run [a1 a2 ctx]
                  (stop-track false)
                  (wait 2)
                  (emit 0x90 99 1))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x42, 0, 0), &runtime);
        // wait(2) is two beats, i.e. 48 ticks at TPB 24.
        assert_eq!(notes(&engine), vec![(48, 0, 99)]);
    }

    #[test]
    fn stop_track_true_stops_the_macro_caller() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x43,
                r#"
                (fn run [a1 a2 ctx]
                  (stop-track true)
                  (wait 2)
                  (emit 0x90 99 1))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x43, 0, 0), &runtime);
        assert!(notes(&engine).is_empty());
        assert!(!engine.overflowed());
    }

    #[test]
    fn math_random_is_deterministic_per_seed() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7A,
                r#"
                (fn run [a1 a2 ctx]
                  (for [_ 1 8]
                    (emit 0x90 (math.random 0 127) 100)))
                "#,
            )
            .unwrap();
        let mut first = pattern_with_macro(0x7A, 0, 0);
        first.set_seed(7);
        let mut second = pattern_with_macro(0x7A, 0, 0);
        second.set_seed(7);
        let a = run(first, &runtime);
        let b = run(second, &runtime);
        assert_eq!(notes(&a).len(), 8);
        assert_eq!(notes(&a), notes(&b), "same seed must reproduce the series");
        assert!(a.diagnostics().is_empty());
    }

    #[test]
    fn different_seeds_change_the_output() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7B,
                r#"
                (fn run [a1 a2 ctx]
                  (for [_ 1 8]
                    (emit 0x90 (math.random 0 127) 100)))
                "#,
            )
            .unwrap();
        let mut first = pattern_with_macro(0x7B, 0, 0);
        first.set_seed(1);
        let mut second = pattern_with_macro(0x7B, 0, 0);
        second.set_seed(2);
        let a = run(first, &runtime);
        let b = run(second, &runtime);
        assert_ne!(notes(&a), notes(&b), "different seeds must diverge");
    }

    #[test]
    fn random_generator_is_independent_of_the_global_series() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7C,
                r#"
                (fn run [a1 a2 ctx]
                  (let [rng (random-generator 99)]
                    (for [_ 1 4]
                      (emit 0x90 (rng:next 0 127) 100))
                    (emit 0x91 (rng 0 127) 100)))
                "#,
            )
            .unwrap();
        // The composition seed changes the global series but must not touch an
        // explicitly constructed generator.
        let mut first = pattern_with_macro(0x7C, 0, 0);
        first.set_seed(1);
        let mut second = pattern_with_macro(0x7C, 0, 0);
        second.set_seed(2);
        let a = run(first, &runtime);
        let b = run(second, &runtime);
        assert_eq!(notes(&a).len(), 5);
        assert_eq!(
            notes(&a),
            notes(&b),
            "random-generator ignores the global seed"
        );
        assert!(a.diagnostics().is_empty());
    }

    #[test]
    fn random_generator_reproduces_across_renders() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7D,
                r#"
                (fn run [a1 a2 ctx]
                  (let [rng (random-generator 1234)]
                    (for [_ 1 4]
                      (emit 0x90 (rng.next 0 127) 100))))
                "#,
            )
            .unwrap();
        let a = run(pattern_with_macro(0x7D, 0, 0), &runtime);
        let b = run(pattern_with_macro(0x7D, 0, 0), &runtime);
        assert_eq!(notes(&a), notes(&b));
    }

    #[test]
    fn math_random_follows_lua_conventions() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7E,
                r#"
                (fn run [a1 a2 ctx]
                  (when (not= (math.type (math.random)) :float)
                    (error "no-argument random should return a float"))
                  (when (not= (math.type (math.random 0)) :integer)
                    (error "zero-argument random should return an integer"))
                  (emit 0x90 (math.random 5 5) 100)
                  (emit 0x91 (math.random 1) 100)
                  (emit 0x92 (math.random 0 0) 100))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x7E, 0, 0), &runtime);
        assert!(
            engine.diagnostics().is_empty(),
            "{:?}",
            engine.diagnostics()
        );
        let keys: Vec<u8> = notes(&engine).into_iter().map(|(_, _, key)| key).collect();
        assert_eq!(keys, vec![5, 1, 0]);
    }

    #[test]
    fn math_randomseed_is_refused() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x7F,
                r#"
                (fn run [a1 a2 ctx]
                  (math.randomseed 1))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x7F, 0, 0), &runtime);
        assert!(
            engine
                .diagnostics()
                .iter()
                .any(|d| d.contains("math.randomseed")),
            "{:?}",
            engine.diagnostics()
        );
    }

    #[test]
    fn setup_module_registers_commands() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .run_setup(r#"(register-command :hello "says hi")"#)
            .unwrap();
        let commands = runtime.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "hello");
        assert_eq!(commands[0].description.as_deref(), Some("says hi"));
    }

    #[test]
    fn macro_errors_are_isolated_and_reported() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(0x44, r#"(fn run [a1 a2 ctx] (error "boom"))"#)
            .unwrap();
        let engine = run(pattern_with_macro(0x44, 0, 0), &runtime);
        assert!(engine.diagnostics().iter().any(|d| d.contains("boom")));
        assert!(!engine.overflowed());
    }

    #[test]
    fn runaway_macro_hits_the_instruction_budget() {
        let limits = MacroLimits {
            instructions_per_tick: 20_000,
            ..MacroLimits::default()
        };
        let runtime = MacroRuntime::new(limits).unwrap();
        runtime
            .load_module(0x45, r#"(fn run [a1 a2 ctx] (while true nil))"#)
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
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x46,
                r#"
                (fn run [a1 a2 ctx]
                  (let [root (get-setting :root)]
                    (set-setting :root (+ root 2))
                    (emit 0x90 (get-setting :root) 1)))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x46, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 62)]);
    }

    #[test]
    fn scale_aware_is_no_longer_a_setting() {
        let settings = Settings::new();
        assert!(matches!(read_setting(settings, "scale_aware"), Value::Nil));
        assert!(setting_change("scale_aware", &Value::Boolean(true)).is_none());
    }

    #[test]
    fn registering_commands_needs_setup() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        // A macro module may not reference the setup-only name at all.
        let error = runtime
            .load_module(0x47, "(fn run [a1 a2 ctx] (register-command :x))")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unknown identifier: register-command"),
            "{error}"
        );
    }

    #[test]
    fn builtin_trigger_and_tempo_modules_work() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
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
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime.set_module_source(0x50, "{:value 7}");
        runtime
            .load_module(
                0x51,
                r#"
                (local util (require 0x50))
                (fn run [a1 a2 ctx] (emit 0x90 util.value 1))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x51, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 7)]);

        // Circular dependency is a deterministic diagnostic.
        let cyclic = MacroRuntime::new(MacroLimits::default()).unwrap();
        cyclic.set_module_source(0x52, "(require 0x53)");
        cyclic.set_module_source(0x53, "(require 0x52)");
        let error = cyclic.validate().unwrap_err().to_string();
        assert!(error.contains("circular"), "{error}");
    }

    #[test]
    fn require_reports_unavailable_ids() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime.load_module(0x54, "(require 0x7E)").unwrap_err();
        // The error is raised during module evaluation, so load_module errors.
        assert!(!runtime.has_module(0x7E));
    }

    #[test]
    fn module_zero_setup_side_effects_run_once() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .run_setup(
                r#"
                (register-command :hello)
                {:greeting "hi"}
                "#,
            )
            .unwrap();
        assert_eq!(runtime.commands().len(), 1);
        let value = runtime.module_value(0).expect("cached");
        assert!(matches!(value, Value::Table(_)));
    }

    #[test]
    fn kebab_case_api_names_resolve_through_mangling() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .load_module(
                0x48,
                r#"(fn run [a1 a2 ctx] (emit-sysex [0xF0 0x7D 0xF7]))"#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x48, 0, 0), &runtime);
        assert!(engine.output().iter().any(|te| matches!(
            &te.event,
            Event::SysEx(data) if data == &vec![0xF0, 0x7D, 0xF7]
        )));
    }

    #[test]
    fn default_module_template_compiles_and_loads() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime
            .run_setup(mtrak_core::modules::MODULE_TEMPLATE)
            .unwrap();
        let value = runtime.module_value(0).expect("the template must cache");
        assert!(matches!(value, Value::Table(_)));
    }

    #[test]
    fn require_returns_any_cached_value() {
        let runtime = MacroRuntime::new(MacroLimits::default()).unwrap();
        runtime.set_module_source(0x55, "42");
        runtime
            .load_module(
                0x56,
                r#"
                (local n (require 0x55))
                (fn run [a1 a2 ctx] (emit 0x90 n 1))
                "#,
            )
            .unwrap();
        let engine = run(pattern_with_macro(0x56, 0, 0), &runtime);
        assert_eq!(notes(&engine), vec![(0, 0, 42)]);
    }
}
