//! The mtrak MIDI tracker application entry point.

use std::process::ExitCode;
use std::sync::mpsc;

use mtrak_midi::{MidiBackend, OutputManager};
use mtrak_repl::{ReplRequest, ReplService, ReplSession};
use mtrak_tui::App;

/// Default REPL port.
const DEFAULT_REPL_PORT: u16 = 6069;

fn main() -> ExitCode {
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("mtrak: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut app = match build_app(cli.file) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("mtrak: {error}");
            return ExitCode::FAILURE;
        }
    };

    let repl = if cli.repl {
        match start_repl(cli.repl_port) {
            Ok(repl) => Some(repl),
            Err(error) => {
                eprintln!("mtrak: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    match run_app(&mut app, repl) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mtrak: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Parsed command-line arguments.
#[derive(Debug)]
struct Cli {
    repl: bool,
    repl_port: u16,
    file: Option<String>,
}

fn parse_args() -> Result<Cli, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I: Iterator<Item = String>>(mut args: I) -> Result<Cli, String> {
    let mut repl = false;
    let mut repl_port = DEFAULT_REPL_PORT;
    let mut repl_port_set = false;
    let mut file = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("mtrak {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--repl" => repl = true,
            "--repl-port" => {
                let port = args
                    .next()
                    .ok_or_else(|| "--repl-port requires a port".to_string())?;
                repl_port = port.parse().map_err(|_| format!("invalid port: {port}"))?;
                repl_port_set = true;
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ => {
                if file.is_some() {
                    return Err("too many file arguments".to_string());
                }
                file = Some(arg);
            }
        }
    }
    if repl_port_set && !repl {
        return Err("--repl-port requires --repl".to_string());
    }
    Ok(Cli {
        repl,
        repl_port,
        file,
    })
}

fn print_help() {
    println!("usage: mtrak [--repl] [--repl-port PORT] [FILE]");
    println!("  FILE            a tracker project or standard MIDI file to open");
    println!("  --repl          start the localhost Fennel REPL");
    println!("  --repl-port N   REPL port (default {DEFAULT_REPL_PORT})");
}

/// A started REPL service and its application-thread session.
struct ReplRuntime {
    service: ReplService,
    receiver: mpsc::Receiver<ReplRequest>,
    session: ReplSession,
}

fn start_repl(port: u16) -> Result<ReplRuntime, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .map_err(|error| format!("cannot bind REPL port {port}: {error}"))?;
    let (sender, receiver) = mpsc::channel();
    let service = ReplService::start(listener, sender).map_err(|error| error.to_string())?;
    let session = ReplSession::new().map_err(|error| error.to_string())?;
    Ok(ReplRuntime {
        service,
        receiver,
        session,
    })
}

fn run_app(app: &mut App, repl: Option<ReplRuntime>) -> std::io::Result<()> {
    match repl {
        Some(ReplRuntime {
            service,
            receiver,
            mut session,
        }) => {
            let result = mtrak_tui::ui::run_with(app, move |app| {
                mtrak_repl::drain_requests(&mut session, app, &receiver);
            });
            drop(service);
            result
        }
        None => mtrak_tui::ui::run(app),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn defaults_to_no_repl_and_default_port() {
        let cli = parse_args_from(args(&[])).unwrap();
        assert!(!cli.repl);
        assert_eq!(cli.repl_port, DEFAULT_REPL_PORT);
        assert!(cli.file.is_none());
    }

    #[test]
    fn parses_repl_flag() {
        let cli = parse_args_from(args(&["--repl"])).unwrap();
        assert!(cli.repl);
        assert_eq!(cli.repl_port, DEFAULT_REPL_PORT);
    }

    #[test]
    fn parses_repl_port_override() {
        let cli = parse_args_from(args(&["--repl", "--repl-port", "7070"])).unwrap();
        assert!(cli.repl);
        assert_eq!(cli.repl_port, 7070);
    }

    #[test]
    fn rejects_repl_port_without_repl() {
        let error = parse_args_from(args(&["--repl-port", "7070"])).unwrap_err();
        assert!(error.contains("--repl-port requires --repl"), "{error}");
    }

    #[test]
    fn parses_file_argument() {
        let cli = parse_args_from(args(&["--repl", "project.mid"])).unwrap();
        assert!(cli.repl);
        assert_eq!(cli.file.as_deref(), Some("project.mid"));
    }

    #[test]
    fn rejects_unknown_option() {
        let error = parse_args_from(args(&["--nope"])).unwrap_err();
        assert!(error.contains("unknown option"), "{error}");
    }

    #[test]
    fn rejects_too_many_files() {
        let error = parse_args_from(args(&["a.mid", "b.mid"])).unwrap_err();
        assert!(error.contains("too many file arguments"), "{error}");
    }

    #[test]
    fn rejects_invalid_port() {
        let error = parse_args_from(args(&["--repl", "--repl-port", "nope"])).unwrap_err();
        assert!(error.contains("invalid port"), "{error}");
    }

    #[test]
    fn occupied_port_fails_startup() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        match start_repl(port) {
            Ok(_) => panic!("start_repl should fail when the port is occupied"),
            Err(error) => assert!(error.contains("cannot bind"), "{error}"),
        }
    }

    #[test]
    fn start_repl_binds_loopback() {
        let runtime = start_repl(0).unwrap();
        drop(runtime);
    }

    #[test]
    fn rejects_missing_port_value() {
        let error = parse_args_from(args(&["--repl", "--repl-port"])).unwrap_err();
        assert!(error.contains("--repl-port requires a port"), "{error}");
    }
}
