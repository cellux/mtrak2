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
            let canonical =
                std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve {path}: {e}"))?;
            (loaded.project, Some(canonical))
        }
        None => (
            mtrak_midi_io::TrackerProject::new(mtrak_core::composition::Composition::new()),
            None,
        ),
    };

    let mut app = App::new(project);
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            std::env::set_current_dir(parent)
                .map_err(|e| format!("cannot change directory to {}: {e}", parent.display()))?;
        }
        app.set_path(path);
    }

    let mut manager = OutputManager::new(make_backend());
    if let Ok(ports) = manager.list_outputs() {
        if let Some(port) = ports.first() {
            let _ = manager.select(port.id);
        }
    }
    app.set_midi(manager);

    if let Some((source, commands)) = macro_support::MacroRuntimeSource::build(app.project()) {
        app.set_project_commands(commands);
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

mod macro_support {
    use std::collections::BTreeMap;

    use mtrak_core::modules::SETUP;
    use mtrak_engine::macros::MacroFactory;
    use mtrak_engine::runtime::{MacroLimits, MacroRuntime, RuntimeMacroFactory};
    use mtrak_midi_io::TrackerProject;
    use mtrak_tui::{MacroSource, ProjectCommand};

    /// A macro source that renders user macros through a Fennel runtime.
    pub struct MacroRuntimeSource {
        runtime: MacroRuntime,
    }

    impl MacroRuntimeSource {
        /// Builds a runtime from the project's macro sources, returning the
        /// source and the project-defined commands it registered.
        pub fn build(
            project: &TrackerProject,
        ) -> Option<(MacroRuntimeSource, Vec<ProjectCommand>)> {
            let mut source = MacroRuntimeSource {
                runtime: MacroRuntime::new(MacroLimits::default()).ok()?,
            };
            let commands = source.reload(&project.modules).ok()?;
            Some((source, commands))
        }
    }

    impl MacroSource for MacroRuntimeSource {
        fn factory(&self) -> Option<Box<dyn MacroFactory>> {
            Some(Box::new(RuntimeMacroFactory::new(self.runtime.clone())))
        }

        fn reload(
            &mut self,
            modules: &BTreeMap<u8, String>,
        ) -> Result<Vec<ProjectCommand>, String> {
            let runtime = MacroRuntime::new(MacroLimits::default()).map_err(|e| e.to_string())?;
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
                .map(|command| ProjectCommand {
                    name: command.name,
                    description: command.description,
                })
                .collect();
            self.runtime = runtime;
            Ok(commands)
        }
    }
}
