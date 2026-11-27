//! Local HTTP Fennel REPL for mtrak.
//!
//! The REPL owns one persistent Lua session and evaluates forms synchronously
//! on the application thread. The HTTP listener runs on its own threads and
//! communicates through channels so it never borrows or mutates [`App`]
//! directly.

#![forbid(unsafe_code)]

pub mod api;
pub mod error;
pub mod events;
pub mod http;
pub mod json;
pub mod request;
pub mod service;
pub mod session;

pub use error::{JsonError, ReplError};
pub use http::ReplService;
pub use request::{ReplRequest, ReplRequestKind, ReplResponse};
pub use service::{drain_requests, handle_request, success_response};
pub use session::ReplSession;

#[cfg(test)]
mod tests {
    use mtrak_core::composition::{Composition, PatternId};
    use mtrak_midi_io::TrackerProject;
    use mtrak_tui::App;

    use super::*;

    fn test_app() -> App {
        App::new(TrackerProject::new(Composition::new()))
    }

    fn eval_string(session: &mut ReplSession, app: &mut App, source: &str) -> String {
        match session.eval(app, source) {
            Ok(value) => crate::json::print_value(&value),
            Err(error) => format!("error: {error}"),
        }
    }

    #[test]
    fn session_persists_globals_and_resets() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let first = eval_string(&mut session, &mut app, "(global x 41)");
        assert!(!first.contains("error"), "{first}");
        let second = eval_string(&mut session, &mut app, "(+ x 1)");
        assert!(second.contains("42"), "{second}");
        session.reset().unwrap();
        let third = eval_string(&mut session, &mut app, "(+ x 1)");
        assert!(third.contains("error"), "{third}");
    }

    #[test]
    fn patterns_api_lists_and_creates() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let listed = eval_string(&mut session, &mut app, "(mtrak.patterns.list)");
        assert!(listed.contains("0"), "{listed}");
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        assert!(!created.contains("error"), "{created}");
        let listed = eval_string(&mut session, &mut app, "(mtrak.patterns.list)");
        assert!(listed.contains("3"), "{listed}");
    }

    #[test]
    fn project_api_reads_and_writes() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let bpm = eval_string(&mut session, &mut app, "(mtrak.project.bpm)");
        assert!(bpm.contains("120"), "{bpm}");
        let set = eval_string(&mut session, &mut app, "(mtrak.project.set-bpm 140)");
        assert!(!set.contains("error"), "{set}");
        let bpm = eval_string(&mut session, &mut app, "(mtrak.project.bpm)");
        assert!(bpm.contains("140"), "{bpm}");
    }

    #[test]
    fn unknown_globals_are_compile_errors() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(unknown-global 1)");
        assert!(result.contains("error"), "{result}");
    }

    #[test]
    fn instruction_budget_stops_infinite_loops() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(while true nil)");
        assert!(result.contains("error"), "{result}");
        assert!(
            result.contains("instruction limit") || result.contains("timed out"),
            "{result}"
        );
    }

    #[test]
    fn pattern_lifecycle_works() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        assert!(!created.contains("error"), "{created}");
        let got = eval_string(&mut session, &mut app, "(mtrak.patterns.get 3)");
        assert!(got.contains("3"), "{got}");
        let named = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-name 3 \"bass\")",
        );
        assert!(!named.contains("error"), "{named}");
        let lpb = eval_string(&mut session, &mut app, "(mtrak.patterns.set-lpb 3 8)");
        assert!(!lpb.contains("error"), "{lpb}");
        let resized = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.resize 3 {:rows 32 :tracks 8})",
        );
        assert!(!resized.contains("error"), "{resized}");
        let deleted = eval_string(&mut session, &mut app, "(mtrak.patterns.delete 3)");
        assert!(!deleted.contains("error"), "{deleted}");
    }

    #[test]
    fn pattern_slot_round_trips() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let _ = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        let set = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slot 3 0 0 (mtrak.events.note-on 0 60 100) 0)",
        );
        assert!(!set.contains("error"), "{set}");
        let got = eval_string(&mut session, &mut app, "(mtrak.patterns.get-slot 3 0 0)");
        assert!(got.contains("note-on"), "{got}");
        assert!(got.contains("60"), "{got}");
    }

    #[test]
    fn discovery_helpers_list_the_surface() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let keys = eval_string(&mut session, &mut app, "(mtrak.settings.keys)");
        for key in ["root", "scale", "modal-degree"] {
            assert!(keys.contains(key), "{keys}");
        }
        let kinds = eval_string(&mut session, &mut app, "(mtrak.events.kinds)");
        for kind in ["note-on", "control-change", "pitch-bend"] {
            assert!(kinds.contains(kind), "{kinds}");
        }
        let types = eval_string(&mut session, &mut app, "(mtrak.events.types)");
        for kind in ["empty", "channel", "sysex"] {
            assert!(types.contains(kind), "{types}");
        }
        let help = eval_string(&mut session, &mut app, "(mtrak.help)");
        assert!(help.contains("/status"), "{help}");
        assert!(help.contains("patterns"), "{help}");
    }

    #[test]
    fn repl_info_reports_limits() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let info = eval_string(&mut session, &mut app, "(mtrak.repl.info)");
        assert!(info.contains("instruction-budget"), "{info}");
        assert!(info.contains("timeout-seconds"), "{info}");
        assert!(info.contains("max-request-bytes"), "{info}");
        assert!(info.contains("fennel-version"), "{info}");
    }

    #[test]
    fn existence_checks_do_not_raise() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let present = eval_string(&mut session, &mut app, "(mtrak.patterns.exists 0)");
        assert!(present.contains("true"), "{present}");
        let absent = eval_string(&mut session, &mut app, "(mtrak.patterns.exists 0x7F)");
        assert!(absent.contains("false"), "{absent}");
        let module = eval_string(&mut session, &mut app, "(mtrak.macros.exists 0x40)");
        assert!(module.contains("false"), "{module}");
    }

    #[test]
    fn seed_int_matches_the_hex_seed() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        eval_string(&mut session, &mut app, "(mtrak.project.set-seed 0x10)");
        let value = eval_string(&mut session, &mut app, "(mtrak.project.seed-int)");
        assert!(value.trim() == "16", "{value}");
    }

    #[test]
    fn patterns_events_and_set_slots_round_trip() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0x10 {:lpb 4})",
        );
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slots 0x10 [{:row 1 :track 2 :event (mtrak.events.note-on 0 60 100)} {:row 3 :track 4 :event (mtrak.events.note-off 0 60 0) :offset 7}])",
        );
        assert!(!result.contains("error"), "{result}");
        let events = eval_string(&mut session, &mut app, "(mtrak.patterns.events 0x10)");
        assert!(events.contains("60"), "{events}");
        assert!(events.contains("note-off"), "{events}");
        let slot = eval_string(&mut session, &mut app, "(mtrak.patterns.get-slot 0x10 3 4)");
        assert!(slot.contains("offset"), "{slot}");
    }

    #[test]
    fn set_slots_is_atomic() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0x10 {:lpb 4})",
        );
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slots 0x10 [{:row 0 :track 0 :event (mtrak.events.note-on 0 60 100)} {:row 99 :track 0 :event (mtrak.events.note-on 0 61 100)}])",
        );
        assert!(result.contains("outside"), "{result}");
        let events = eval_string(&mut session, &mut app, "(mtrak.patterns.events 0x10)");
        assert!(!events.contains("60"), "first entry leaked: {events}");
    }

    #[test]
    fn macros_validate_compiles_without_storing() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let ok = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.validate 0x40 \"(local M {})\\n\\n(fn M.run [a b c]\\n  nil)\\n\\nM\\n\")",
        );
        assert!(ok.contains("true"), "{ok}");
        let bad = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.validate 0x40 \"(broken\")",
        );
        assert!(bad.contains("error"), "{bad}");
        let list = eval_string(&mut session, &mut app, "(mtrak.macros.list)");
        assert!(!list.contains("64"), "validate stored a module: {list}");
    }

    #[test]
    fn cancelled_requests_are_skipped() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;
        use std::sync::Arc;

        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(true));
        let (cancelled_reply, cancelled_rx) = mpsc::channel();
        sender
            .send(ReplRequest {
                kind: ReplRequestKind::Eval("(mtrak.patterns.create 0x30 {})".to_string()),
                response: cancelled_reply,
                cancel: cancelled,
            })
            .unwrap();
        let (live_reply, live_rx) = mpsc::channel();
        sender
            .send(ReplRequest {
                kind: ReplRequestKind::Eval("(+ 1 2)".to_string()),
                response: live_reply,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        drop(sender);

        drain_requests(&mut session, &mut app, &receiver);

        assert!(cancelled_rx.try_recv().is_err(), "cancelled request ran");
        let response = live_rx.try_recv().expect("live request ran");
        assert_eq!(response.status, 200);
        assert!(
            app.project().composition.pattern(PatternId(0x30)).is_none(),
            "cancelled mutation was applied"
        );
    }

    #[test]
    fn fractional_indices_are_rejected() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        for form in [
            "(mtrak.patterns.get-slot 0 1.9 0)",
            "(mtrak.patterns.set-slot 0 2.9 0 (mtrak.events.empty))",
            "(mtrak.patterns.create 0x22 {:lpb 1.9})",
            "(mtrak.patterns.resize 0 {:rows 1.9})",
            "(mtrak.patterns.get 1.5)",
            "(mtrak.macros.get 1.5)",
            "(mtrak.project.set-selected-main 1.5)",
        ] {
            let result = eval_string(&mut session, &mut app, form);
            assert!(result.contains("whole number"), "{form} -> {result}");
        }
        let result = eval_string(&mut session, &mut app, "(mtrak.patterns.get 0)");
        assert!(!result.contains("whole number"), "{result}");
    }

    #[test]
    fn parse_errors_omit_the_lua_traceback() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(1+1)");
        assert!(result.contains("Parse error"), "{result}");
        assert!(!result.contains("stack traceback"), "{result}");
        assert!(!result.contains("fennel.lua"), "{result}");
    }

    #[test]
    fn fractional_bpm_and_seed_are_rejected() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.project.set-bpm 1.9)");
        assert!(result.contains("whole number"), "{result}");
        let result = eval_string(&mut session, &mut app, "(mtrak.project.set-seed 1.5)");
        assert!(result.contains("whole number"), "{result}");
        let result = eval_string(&mut session, &mut app, "(mtrak.project.set-bpm 120)");
        assert!(!result.contains("whole number"), "{result}");
    }

    #[test]
    fn sysex_rejects_out_of_range_bytes() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.events.sysex 300)");
        assert!(result.contains("out of range"), "{result}");
        let result = eval_string(&mut session, &mut app, "(mtrak.events.sysex -1)");
        assert!(result.contains("out of range"), "{result}");
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.events.sysex 0xF0 0x7D 0xF7)",
        );
        assert!(result.contains("240"), "{result}");
    }

    #[test]
    fn runtime_errors_are_not_double_prefixed() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.settings.set :nope 1)");
        assert!(result.contains("unknown setting nope"), "{result}");
        assert!(
            !result.contains("runtime error: runtime error:"),
            "{result}"
        );
    }

    #[test]
    fn compile_errors_are_not_double_prefixed() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(1 2)");
        assert!(result.contains("compile error"), "{result}");
        assert!(
            !result.contains("compile error: runtime error:"),
            "{result}"
        );
    }

    #[test]
    fn settings_reject_out_of_range_scale_mask() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.settings.set :scale 70000)");
        assert!(result.contains("65535"), "{result}");
        let result = eval_string(&mut session, &mut app, "(mtrak.settings.set :scale -1)");
        assert!(result.contains("65535"), "{result}");
    }

    #[test]
    fn create_rejects_invalid_geometry_options() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0x12 {:lpb \"x\" :rows \"y\" :tracks -1})",
        );
        assert!(result.contains("error"), "{result}");
        assert_eq!(app.project().composition.pattern_ids().len(), 1);
    }

    #[test]
    fn failed_create_leaves_no_pattern() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let before = app.project().composition.pattern_ids();
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0x31 {:rows 0})",
        );
        assert!(result.contains("error"), "{result}");
        assert_eq!(app.project().composition.pattern_ids(), before);
    }

    #[test]
    fn failed_resize_preserves_geometry() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.resize 0 {:rows 32 :tracks 0})",
        );
        assert!(result.contains("error"), "{result}");
        let pattern = app.project().composition.pattern(PatternId(0)).unwrap();
        assert_eq!(pattern.row_count(), 64);
        assert_eq!(pattern.track_count(), 16);
    }

    #[test]
    fn event_constructors_work() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let _ = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        let set = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slot 3 0 0 (mtrak.events.sysex 0x7D 1 2 3) 0)",
        );
        assert!(!set.contains("error"), "{set}");
        let got = eval_string(&mut session, &mut app, "(mtrak.patterns.get-slot 3 0 0)");
        assert!(got.contains("sysex"), "{got}");
    }

    #[test]
    fn settings_round_trip() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let root = eval_string(&mut session, &mut app, "(mtrak.settings.get :root)");
        assert!(root.contains("0"), "{root}");
        let set = eval_string(&mut session, &mut app, "(mtrak.settings.set :root 64)");
        assert!(!set.contains("error"), "{set}");
        let root = eval_string(&mut session, &mut app, "(mtrak.settings.get :root)");
        assert!(root.contains("64"), "{root}");
    }

    #[test]
    fn project_metadata_works() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let seed = eval_string(&mut session, &mut app, "(mtrak.project.seed)");
        assert!(seed.contains("0x"), "{seed}");
        let _ = eval_string(&mut session, &mut app, "(mtrak.project.set-seed 42)");
        let seed = eval_string(&mut session, &mut app, "(mtrak.project.seed)");
        assert!(seed.contains("2A"), "{seed}");
    }

    #[test]
    fn local_bindings_are_scoped() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let first = eval_string(&mut session, &mut app, "(let [x 1] x)");
        assert!(first.contains("1"), "{first}");
        let second = eval_string(&mut session, &mut app, "x");
        assert!(second.contains("error"), "{second}");
    }

    #[test]
    fn replace_form_works() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let _ = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        let replaced = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.replace-form 0x40 \"(emit 0x90 a1 100)\" \"(emit 0x91 a1 101)\")",
        );
        assert!(!replaced.contains("error"), "{replaced}");
        let got = eval_string(&mut session, &mut app, "(mtrak.macros.get 0x40)");
        assert!(got.contains("0x91"), "{got}");
    }

    #[test]
    fn reload_all_works() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.macros.reload-all)");
        assert!(!result.contains("error"), "{result}");
    }

    #[test]
    fn macro_lifecycle_works() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        assert!(!created.contains("error"), "{created}");
        let listed = eval_string(&mut session, &mut app, "(mtrak.macros.list)");
        assert!(listed.contains("64"), "{listed}");
        let got = eval_string(&mut session, &mut app, "(mtrak.macros.get 0x40)");
        assert!(got.contains("0x90"), "{got}");
        let updated = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.update 0x40 \"(fn run [a1] (emit 0x91 a1 101))\")",
        );
        assert!(!updated.contains("error"), "{updated}");
        let deleted = eval_string(&mut session, &mut app, "(mtrak.macros.delete 0x40)");
        assert!(!deleted.contains("error"), "{deleted}");
    }

    #[test]
    fn editor_conflict_rejects_macro_change() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        assert!(!created.contains("error"), "{created}");
        app.select_module(0x40);
        app.repl_set_editor_text("(fn run [a1] (emit 0x91 a1 101))");
        let updated = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.update 0x40 \"(fn run [a1] (emit 0x92 a1 102))\")",
        );
        assert!(updated.contains("unsaved"), "{updated}");
    }

    #[test]
    fn local_scope_does_not_persist() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let first = eval_string(&mut session, &mut app, "(let [x 41] x)");
        assert!(first.contains("41"), "{first}");
        let second = eval_string(&mut session, &mut app, "x");
        assert!(second.contains("error"), "{second}");
    }

    #[test]
    fn sandbox_blocks_io_os_debug() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        for name in ["io", "os", "debug"] {
            let result = eval_string(&mut session, &mut app, name);
            assert!(result.contains("error"), "{name}: {result}");
        }
    }

    #[test]
    fn callable_settings_reads() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let root = eval_string(&mut session, &mut app, "(mtrak.settings :root)");
        assert!(root.contains("0"), "{root}");
    }

    #[test]
    fn selected_main_deletion_is_protected() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.patterns.delete 0)");
        assert!(result.contains("selected main"), "{result}");
    }

    #[test]
    fn invalid_source_preserves_module() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        assert!(!created.contains("error"), "{created}");
        let invalid = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.update 0x40 \"(fn run [a1]\")",
        );
        assert!(invalid.contains("error"), "{invalid}");
        let source = eval_string(&mut session, &mut app, "(mtrak.macros.get 0x40)");
        assert!(source.contains("0x90"), "{source}");
    }

    #[test]
    fn reload_all_failure_preserves_runtime() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        assert!(!created.contains("error"), "{created}");
        app.repl_project_mut()
            .modules
            .insert(0x40, "(fn run [a1]".to_string());
        let failed = eval_string(&mut session, &mut app, "(mtrak.macros.reload-all)");
        assert!(failed.contains("error"), "{failed}");
        app.repl_project_mut()
            .modules
            .insert(0x40, "(fn run [a1] (emit 0x90 a1 100))".to_string());
        let recovered = eval_string(&mut session, &mut app, "(mtrak.macros.reload-all)");
        assert!(!recovered.contains("error"), "{recovered}");
    }

    #[test]
    fn dirty_state_is_set_by_repl_mutation() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        assert!(!app.is_dirty());
        let _ = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        assert!(app.is_dirty());
    }

    #[test]
    fn end_to_end_workflow() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 3 {:lpb 4 :rows 16 :tracks 4})",
        );
        assert!(!created.contains("error"), "{created}");
        let set = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slot 3 0 0 (mtrak.events.note-on 0 60 100) 0)",
        );
        assert!(!set.contains("error"), "{set}");
        let macro_created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(fn run [a1] (emit 0x90 a1 100))\")",
        );
        assert!(!macro_created.contains("error"), "{macro_created}");
        let replaced = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.replace-form 0x40 \"(emit 0x90 a1 100)\" \"(emit 0x91 a1 101)\")",
        );
        assert!(!replaced.contains("error"), "{replaced}");
        let reloaded = eval_string(&mut session, &mut app, "(mtrak.macros.reload-all)");
        assert!(!reloaded.contains("error"), "{reloaded}");
    }

    #[test]
    fn builtin_module_is_read_only() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let update = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.update 0x01 \"(fn run [a1] nil)\")",
        );
        assert!(update.contains("built-in"), "{update}");
        let delete = eval_string(&mut session, &mut app, "(mtrak.macros.delete 0x01)");
        assert!(delete.contains("built-in"), "{delete}");
        let replace = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.replace-form 0x01 \"(trigger a1)\" \"(trigger a2)\")",
        );
        assert!(replace.contains("built-in"), "{replace}");
    }

    #[test]
    fn setup_module_is_writable() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let update = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.update 0x00 \"(local M {})\\n\\n(fn M.run [a1 a2 ctx]\\n  nil)\\n\\nM\\n\")",
        );
        assert!(!update.contains("error"), "{update}");
        let delete = eval_string(&mut session, &mut app, "(mtrak.macros.delete 0x00)");
        assert!(delete.contains("cannot be deleted"), "{delete}");
    }

    #[test]
    fn invalid_pattern_operations_are_rejected() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let occupied = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0 {:lpb 4 :rows 16 :tracks 4})",
        );
        assert!(occupied.contains("error"), "{occupied}");
        let zero_lpb = eval_string(&mut session, &mut app, "(mtrak.patterns.create 3 {:lpb 0})");
        assert!(zero_lpb.contains("error"), "{zero_lpb}");
    }

    #[test]
    fn tpb_change_api_is_absent() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(&mut session, &mut app, "(mtrak.project.set-tpb 8)");
        assert!(result.contains("error"), "{result}");
    }

    #[test]
    fn repl_reset_api_clears_globals() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let first = eval_string(&mut session, &mut app, "(global y 7)");
        assert!(!first.contains("error"), "{first}");
        let _ = eval_string(&mut session, &mut app, "(mtrak.repl.reset)");
        let after = eval_string(&mut session, &mut app, "(+ y 1)");
        assert!(after.contains("error"), "{after}");
    }

    #[test]
    fn eval_stops_playback() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        app.start_playback();
        assert!(app.is_playing());
        let response = crate::handle_request(
            &mut session,
            &mut app,
            crate::ReplRequestKind::Eval("(+ 1 2)".to_string()),
        );
        assert_eq!(response.status, 200);
        assert!(!app.is_playing());
    }

    #[test]
    fn pattern_mutation_bumps_revision_and_marks_dirty() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let before = app.project_revision();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.create 0x10 {:rows 8 :tracks 2})",
        );
        assert!(!created.contains("error"), "{created}");
        assert!(app.is_dirty());
        assert!(app.project_revision() > before);
    }

    #[test]
    fn replace_form_rejects_non_string_values() {
        let mut app = test_app_with_macros();
        let mut session = ReplSession::new().unwrap();
        let created = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.create 0x40 \"(local M {})\\n\\n(fn run [] nil)\\n\\nM\\n\")",
        );
        assert!(!created.contains("error"), "{created}");
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.macros.replace-form 0x40 \"(local M {})\" {:a 1})",
        );
        assert!(result.contains("must be"), "{result}");
    }

    #[test]
    fn rejected_macro_globals_are_friendly() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        for form in ["(stop-track 1)", "(emit 0x90 60 100)", "(wait 1)"] {
            let result = eval_string(&mut session, &mut app, form);
            assert!(
                result.contains("not available in the REPL"),
                "{form}: {result}"
            );
        }
    }

    #[test]
    fn hyphenated_globals_persist_across_evals() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let defined = eval_string(&mut session, &mut app, "(global inc-it (fn [x] (+ x 1)))");
        assert!(!defined.contains("error"), "{defined}");
        let called = eval_string(&mut session, &mut app, "(inc-it 41)");
        assert!(called.contains("42"), "{called}");
    }

    #[test]
    fn instruction_limit_is_reported_as_its_own_kind() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let error = session.eval(&mut app, "(while true nil)").unwrap_err();
        assert_eq!(error.kind(), "instruction_limit", "{error}");
    }

    #[test]
    fn event_constructors_validate_midi_ranges() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let channel = eval_string(&mut session, &mut app, "(mtrak.events.note-on 200 60 100)");
        assert!(channel.contains("channel 200"), "{channel}");
        let data1 = eval_string(&mut session, &mut app, "(mtrak.events.note-on 0 200 100)");
        assert!(data1.contains("data1 200"), "{data1}");
        let data2 = eval_string(&mut session, &mut app, "(mtrak.events.note-on 0 60 200)");
        assert!(data2.contains("data2 200"), "{data2}");
        let valid = eval_string(&mut session, &mut app, "(mtrak.events.note-on 15 127 127)");
        assert!(!valid.contains("error"), "{valid}");
    }

    #[test]
    fn set_slot_rejects_out_of_range_channel() {
        let mut app = test_app();
        let mut session = ReplSession::new().unwrap();
        let result = eval_string(
            &mut session,
            &mut app,
            "(mtrak.patterns.set-slot 0 0 0 {:type \"channel\" :kind \"note-on\" :channel 200 :data1 60 :data2 100})",
        );
        assert!(result.contains("channel 200"), "{result}");
    }

    // --- Test macro runtime ---------------------------------------------------

    use std::collections::BTreeMap;

    use mtrak_engine::macros::MacroFactory;
    use mtrak_engine::runtime::{MacroLimits, MacroRuntime, RuntimeMacroFactory};
    use mtrak_tui::{MacroSource, ProjectCommand};

    struct TestMacroSource {
        runtime: MacroRuntime,
    }

    impl TestMacroSource {
        fn build(project: &TrackerProject) -> (Self, Vec<ProjectCommand>) {
            let mut source = TestMacroSource {
                runtime: MacroRuntime::new(MacroLimits::default()).unwrap(),
            };
            let commands = source.reload(&project.modules).unwrap();
            (source, commands)
        }
    }

    impl MacroSource for TestMacroSource {
        fn factory(&self) -> Option<Box<dyn MacroFactory>> {
            Some(Box::new(RuntimeMacroFactory::new(self.runtime.clone())))
        }

        fn reload(
            &mut self,
            modules: &BTreeMap<u8, String>,
        ) -> Result<Vec<ProjectCommand>, String> {
            let runtime = MacroRuntime::new(MacroLimits::default()).map_err(|e| e.to_string())?;
            if let Some(source) = modules.get(&mtrak_core::modules::SETUP) {
                runtime.run_setup(source).map_err(|e| e.to_string())?;
            }
            for (id, source) in modules {
                if *id != mtrak_core::modules::SETUP {
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

    fn test_app_with_macros() -> App {
        let project = TrackerProject::new(Composition::new());
        let (source, commands) = TestMacroSource::build(&project);
        let mut app = App::new(project);
        app.set_project_commands(commands);
        app.set_macro_source(Box::new(source));
        app
    }
}
