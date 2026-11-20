//! The mtrak MIDI tracker application entry point.

use std::process::ExitCode;

use mtrak_midi::{MidiBackend, OutputManager};
use mtrak_tui::App;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    match first.as_deref() {
        Some("--version") | Some("-V") => {
            println!("mtrak {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help") | Some("-h") => {
            println!("usage: mtrak [FILE]");
            println!("  FILE  a tracker project or standard MIDI file to open");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    let mut app = match build_app(first) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("mtrak: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut last_revision = app.project_revision();
    let result = mtrak_tui::ui::run_with(&mut app, |app| {
        if app.project_revision() == last_revision {
            return;
        }
        last_revision = app.project_revision();
        resync_project(app);
    });

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mtrak: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build_app(path: Option<String>) -> Result<App, String> {
    let (project, path) = match path {
        Some(path) => {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let loaded = mtrak_midi_io::smf::load(&bytes).map_err(|e| e.to_string())?;
            (loaded.project, Some(std::path::PathBuf::from(path)))
        }
        None => (
            mtrak_midi_io::TrackerProject {
                composition: mtrak_core::composition::Composition::new(),
                lua_modules: Default::default(),
                setup_source: None,
            },
            None,
        ),
    };

    let mut app = App::new(project);
    if let Some(path) = path {
        app.set_path(path);
    }

    let mut manager = OutputManager::new(make_backend());
    if let Ok(ports) = manager.list_outputs() {
        if let Some(port) = ports.first() {
            let _ = manager.select(port.id);
        }
    }
    app.set_midi(manager);

    #[cfg(feature = "lua")]
    if let Some((source, commands)) = lua_support::LuaSource::build(app.project()) {
        app.set_lua_commands(commands);
        app.set_macro_source(Box::new(source));
    }

    Ok(app)
}

#[cfg(feature = "midir")]
fn make_backend() -> Box<dyn MidiBackend> {
    match mtrak_midi::midir_backend::MidirBackend::new() {
        Ok(backend) => Box::new(backend),
        Err(_) => Box::new(mtrak_midi::MockBackend::new(&[])),
    }
}

#[cfg(not(feature = "midir"))]
fn make_backend() -> Box<dyn MidiBackend> {
    Box::new(mtrak_midi::MockBackend::new(&[]))
}

#[cfg(feature = "lua")]
fn resync_project(app: &mut App) {
    match lua_support::LuaSource::build(app.project()) {
        Some((source, commands)) => {
            app.set_lua_commands(commands);
            app.set_macro_source(Box::new(source));
        }
        None => app.set_lua_commands(Vec::new()),
    }
}

#[cfg(not(feature = "lua"))]
fn resync_project(_app: &mut App) {}

#[cfg(feature = "lua")]
mod lua_support {
    use mtrak_engine::lua::{LuaLimits, LuaMacroFactory, LuaRuntime};
    use mtrak_engine::macros::MacroFactory;
    use mtrak_midi_io::TrackerProject;
    use mtrak_tui::{LuaCommand, MacroSource};

    /// A macro source that renders user macros through a Lua runtime.
    pub struct LuaSource {
        runtime: LuaRuntime,
    }

    impl LuaSource {
        /// Builds a runtime from the project's Lua sources, returning the source
        /// and the project-defined commands it registered.
        pub fn build(project: &TrackerProject) -> Option<(LuaSource, Vec<LuaCommand>)> {
            let runtime = LuaRuntime::new(LuaLimits::default()).ok()?;
            if let Some(setup) = &project.setup_source {
                let _ = runtime.run_setup(setup);
            }
            for (id, source) in &project.lua_modules {
                let _ = runtime.load_module(*id, source);
            }
            let commands = runtime
                .commands()
                .into_iter()
                .map(|command| LuaCommand {
                    name: command.name,
                    description: command.description,
                })
                .collect();
            Some((LuaSource { runtime }, commands))
        }
    }

    impl MacroSource for LuaSource {
        fn factory(&self) -> Option<Box<dyn MacroFactory>> {
            Some(Box::new(LuaMacroFactory::new(self.runtime.clone())))
        }
    }
}
