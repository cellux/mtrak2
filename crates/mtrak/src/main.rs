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

    match mtrak_tui::ui::run(&mut app) {
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
            mtrak_midi_io::TrackerProject::new(mtrak_core::composition::Composition::new()),
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

mod lua_support {
    use std::collections::BTreeMap;

    use mtrak_core::modules::SETUP;
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
            let mut source = LuaSource {
                runtime: LuaRuntime::new(LuaLimits::default()).ok()?,
            };
            let commands = source.reload(&project.modules).ok()?;
            Some((source, commands))
        }
    }

    impl MacroSource for LuaSource {
        fn factory(&self) -> Option<Box<dyn MacroFactory>> {
            Some(Box::new(LuaMacroFactory::new(self.runtime.clone())))
        }

        fn reload(&mut self, modules: &BTreeMap<u8, String>) -> Result<Vec<LuaCommand>, String> {
            let runtime = LuaRuntime::new(LuaLimits::default()).map_err(|e| e.to_string())?;
            if let Some(source) = modules.get(&SETUP) {
                runtime.run_setup(source).map_err(|e| e.to_string())?;
            }
            for (id, source) in modules {
                if *id != SETUP {
                    runtime.set_module_source(*id, source);
                }
            }
            runtime.validate().map_err(|e| e.to_string())?;
            let commands = runtime
                .commands()
                .into_iter()
                .map(|command| LuaCommand {
                    name: command.name,
                    description: command.description,
                })
                .collect();
            self.runtime = runtime;
            Ok(commands)
        }
    }
}
