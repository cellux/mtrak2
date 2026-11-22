//! End-to-end tests for the `PLAN.md` §5 validation targets.
//!
//! Everything here runs without MIDI hardware. Checked-in fixtures live in
//! `tests/fixtures/` and the strategy is simply: any MIDI file the loader must
//! accept is committed as bytes next to the test that reads it.

use mtrak_core::composition::{Composition, PatternId};
use mtrak_core::event::{macro_id, ChannelKind, ChannelMessage, Event, MacroCall, MetaEvent};
use mtrak_core::timing::Bpm;
use mtrak_engine::lua::{LuaLimits, LuaMacroFactory, LuaRuntime};
use mtrak_engine::scheduler::TimedEvent;
use mtrak_midi_io::metadata::TrackerProject;
use mtrak_midi_io::smf;
use mtrak_tui::{App, Code, Key};

fn project_with(rows: usize, tracks: usize) -> TrackerProject {
    let mut composition = Composition::new();
    {
        let pattern = composition.pattern_mut(PatternId(0)).unwrap();
        pattern.resize_rows(rows).unwrap();
        pattern.resize_tracks(tracks).unwrap();
    }
    TrackerProject::new(composition)
}

fn note(key: u8) -> Event {
    Event::Channel(ChannelMessage::with_data(
        ChannelKind::NoteOn,
        0,
        key,
        Some(100),
    ))
}

fn notes(events: &[TimedEvent]) -> Vec<(u64, u8)> {
    events
        .iter()
        .filter_map(|timed| match &timed.event {
            Event::Channel(message)
                if message.kind == ChannelKind::NoteOn && message.data2 != Some(0) =>
            {
                Some((timed.tick, message.data1.unwrap_or(0)))
            }
            _ => None,
        })
        .collect()
}

// --- Fixtures ---------------------------------------------------------------

#[test]
fn committed_fixtures_import_without_hardware() {
    let simple = smf::load(include_bytes!("fixtures/simple.mid")).unwrap();
    assert!(!simple.was_project);
    assert_eq!(simple.project.composition.tpb(), 96);
    assert_eq!(
        notes(&smf::render_events(&simple.project.composition, None)),
        vec![(0, 60)]
    );

    let multi = smf::load(include_bytes!("fixtures/multitrack.mid")).unwrap();
    // The fixture declares 480 PPQ but every event sits on a beat boundary, so
    // import reduces the resolution to 96 without moving anything.
    assert_eq!(multi.project.composition.tpb(), 96);
    // Two simultaneous source notes must be split into two tracker tracks.
    assert!(
        multi
            .project
            .composition
            .pattern(PatternId(0))
            .unwrap()
            .track_count()
            >= 2
    );
    let mut rendered = notes(&smf::render_events(&multi.project.composition, None));
    rendered.sort_unstable();
    assert_eq!(rendered, vec![(0, 60), (0, 64)]);

    let running = smf::load(include_bytes!("fixtures/running_status.mid")).unwrap();
    let mut rendered = notes(&smf::render_events(&running.project.composition, None));
    rendered.sort_unstable();
    assert_eq!(rendered, vec![(0, 60), (0, 62)]);
}

// --- Deterministic scheduling -----------------------------------------------

#[test]
fn nested_patterns_render_deterministically_with_one_clock() {
    let mut composition = Composition::new();
    let child = composition.create_pattern(5).unwrap(); // raises TPB to 120
    {
        let pattern = composition.pattern_mut(child).unwrap();
        pattern.resize_rows(2).unwrap();
        pattern.set_event(0, 0, note(72));
        pattern.set_event(1, 0, note(74));
    }
    {
        let parent = composition.pattern_mut(PatternId(0)).unwrap();
        parent.resize_rows(2).unwrap();
        parent.set_event(
            0,
            0,
            Event::Macro(MacroCall::new(macro_id::BUILTIN_TRIGGER, child.value(), 0)),
        );
    }
    let first = smf::render_events(&composition, None);
    let second = smf::render_events(&composition, None);
    assert_eq!(first, second, "rendering must be deterministic");
    assert_eq!(composition.tpb(), 120);
    // Parent row = 30 ticks; child row = 24 ticks.
    assert_eq!(notes(&first), vec![(0, 72), (24, 74)]);
}

#[test]
fn track_local_replacement_stops_the_replaced_child() {
    let mut composition = Composition::new();
    let long = composition.create_pattern(4).unwrap();
    let short = composition.create_pattern(4).unwrap();
    {
        let pattern = composition.pattern_mut(long).unwrap();
        pattern.resize_rows(4).unwrap();
        for (i, key) in [60u8, 61, 62, 63].iter().enumerate() {
            pattern.set_event(i, 0, note(*key));
        }
    }
    {
        let pattern = composition.pattern_mut(short).unwrap();
        pattern.resize_rows(1).unwrap();
        pattern.set_event(0, 0, note(70));
    }
    {
        let parent = composition.pattern_mut(PatternId(0)).unwrap();
        parent.resize_rows(3).unwrap();
        parent.set_event(
            0,
            0,
            Event::Macro(MacroCall::new(macro_id::BUILTIN_TRIGGER, long.value(), 0)),
        );
        parent.set_event(
            2,
            0,
            Event::Macro(MacroCall::new(macro_id::BUILTIN_TRIGGER, short.value(), 0)),
        );
    }
    let keys: Vec<u8> = notes(&smf::render_events(&composition, None))
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    assert!(keys.contains(&70), "the replacement child must sound");
    assert!(!keys.contains(&63), "the replaced child must stop");
}

// --- Save / load / export ---------------------------------------------------

#[test]
fn project_round_trip_keeps_metadata_without_duplicating_the_fallback() {
    let mut project = project_with(8, 2);
    {
        let pattern = project.composition.pattern_mut(PatternId(0)).unwrap();
        pattern.set_event(0, 0, note(60));
        pattern.set_event(0, 1, Event::Meta(MetaEvent::Tempo(400_000)));
    }
    project
        .modules
        .insert(0x41, "return { run = function() end }".to_string());
    project
        .modules
        .insert(0, "register_command('x')".to_string());

    let bytes = smf::save_project(&project, None).unwrap();
    assert!(
        bytes.windows(5).any(|w| w == b"MTRAK"),
        "the file must carry tracker metadata"
    );

    let loaded = smf::load(&bytes).unwrap();
    assert!(loaded.was_project);
    // The rendered fallback must not become composition data.
    assert_eq!(
        loaded.project.composition.pattern_ids(),
        project.composition.pattern_ids()
    );
    let original = project.composition.pattern(PatternId(0)).unwrap();
    let restored = loaded.project.composition.pattern(PatternId(0)).unwrap();
    assert_eq!(original.row_count(), restored.row_count());
    assert_eq!(original.track_count(), restored.track_count());
    assert_eq!(restored.slot(0, 0).unwrap().event, note(60));
    assert_eq!(
        restored.slot(0, 1).unwrap().event,
        Event::Meta(MetaEvent::Tempo(400_000))
    );
    assert_eq!(
        loaded.project.modules.get(&0x41).map(String::as_str),
        Some("return { run = function() end }")
    );
    assert_eq!(
        loaded.project.modules.get(&0).map(String::as_str),
        Some("register_command('x')")
    );
}

#[test]
fn standard_export_renders_lua_macros_and_needs_no_metadata() {
    let runtime = LuaRuntime::new(LuaLimits::default()).unwrap();
    runtime
        .load_module(
            0x41,
            r#"return { run = function(a1, a2, ctx) emit(0x90, a1, 100) end }"#,
        )
        .unwrap();

    let mut project = project_with(4, 1);
    project
        .composition
        .pattern_mut(PatternId(0))
        .unwrap()
        .set_event(0, 0, Event::Macro(MacroCall::new(0x41, 77, 0)));

    let factory = Box::new(LuaMacroFactory::new(runtime.clone()));
    let bytes = smf::export_standard(&project.composition, Some(factory)).unwrap();
    assert!(
        !bytes.windows(5).any(|w| w == b"MTRAK"),
        "standard export must not contain tracker metadata"
    );

    // The exported file is a normal MIDI file whose macro output plays back.
    let loaded = smf::load(&bytes).unwrap();
    assert!(!loaded.was_project);
    assert!(
        notes(&smf::render_events(&loaded.project.composition, None))
            .iter()
            .any(|(_, key)| *key == 77)
    );
}

#[test]
fn export_import_export_preserves_playback() {
    let mut project = project_with(8, 2);
    {
        let pattern = project.composition.pattern_mut(PatternId(0)).unwrap();
        pattern.set_event(0, 0, note(60));
        pattern.set_event(1, 0, note(62));
        pattern.slot_mut(1, 0).unwrap().offset = 3;
        pattern.set_event(2, 1, Event::Meta(MetaEvent::Tempo(500_000)));
    }
    let exported = smf::export_standard(&project.composition, None).unwrap();
    let reimported = smf::load(&exported).unwrap();
    let reexported = smf::export_standard(&reimported.project.composition, None).unwrap();

    let first = smf::load(&exported).unwrap();
    let second = smf::load(&reexported).unwrap();
    let first_events = smf::render_events(&first.project.composition, None);
    let second_events = smf::render_events(&second.project.composition, None);
    assert_eq!(
        first_events, second_events,
        "playback must survive round-trips"
    );
}

// --- Sandbox and bounded failure --------------------------------------------

#[test]
fn sandbox_has_no_host_access_and_runaways_are_bounded() {
    // A module that would only emit if a forbidden global were visible.
    let sandboxed = LuaRuntime::new(LuaLimits::default()).unwrap();
    sandboxed
        .load_module(
            0x41,
            r#"
            return { run = function(a1, a2, ctx)
                if os ~= nil or io ~= nil or debug ~= nil or package ~= nil then
                    emit(0x90, 1, 1)
                else
                    emit(0x90, 60, 1)
                end
            end }
            "#,
        )
        .unwrap();

    let mut project = project_with(2, 1);
    project
        .composition
        .pattern_mut(PatternId(0))
        .unwrap()
        .set_event(0, 0, Event::Macro(MacroCall::new(0x41, 0, 0)));
    let factory = Box::new(LuaMacroFactory::new(sandboxed.clone()));
    assert_eq!(
        notes(&smf::render_events(&project.composition, Some(factory))),
        vec![(0, 60)]
    );

    // A runaway macro must terminate rather than hang the renderer.
    let limited = LuaRuntime::new(LuaLimits {
        instructions_per_tick: 20_000,
        ..LuaLimits::default()
    })
    .unwrap();
    limited
        .load_module(0x42, "return { run = function() while true do end end }")
        .unwrap();
    let mut runaway = project_with(2, 1);
    runaway
        .composition
        .pattern_mut(PatternId(0))
        .unwrap()
        .set_event(0, 0, Event::Macro(MacroCall::new(0x42, 0, 0)));
    let factory = Box::new(LuaMacroFactory::new(limited));
    let events = smf::render_events(&runaway.composition, Some(factory));
    assert!(notes(&events).is_empty());
}

// --- TUI usability ----------------------------------------------------------

#[test]
fn tui_session_edits_notes_and_commands_and_reloads() {
    let project = project_with(8, 1);
    let mut app = App::new(project);

    // Command mode: set the tempo.
    app.handle_key(Key::ch(':'));
    for c in "bpm 150".chars() {
        app.handle_key(Key::ch(c));
    }
    app.handle_key(Key::new(Code::Enter));
    assert_eq!(app.project().composition.bpm(), Bpm::from_int(150).unwrap());

    // REC: note entry and step recording in the default chromatic scale.
    app.handle_key(Key::ctrl(Code::Char('r')));
    app.handle_key(Key::ch('z'));
    assert_eq!(app.cell_display(0, 0).trim_end(), "903C64");

    // Pattern management through the command layer.
    app.handle_key(Key::ch(':'));
    for c in "pattern new 4".chars() {
        app.handle_key(Key::ch(c));
    }
    app.handle_key(Key::new(Code::Enter));
    assert_eq!(app.current_pattern().value(), 1);

    // Save and reload the whole project.
    let path = std::env::temp_dir().join("mtrak_e2e_session.mid");
    app.save(Some(path.to_string_lossy().to_string())).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let loaded = smf::load(&bytes).unwrap();
    assert!(loaded.was_project);
    assert_eq!(
        loaded.project.composition.bpm(),
        Bpm::from_int(150).unwrap()
    );
    assert_eq!(
        loaded
            .project
            .composition
            .pattern(PatternId(0))
            .unwrap()
            .slot(0, 0)
            .unwrap()
            .event,
        {
            let mut event = note(60);
            if let Event::Channel(message) = &mut event {
                message.data2 = Some(100);
            }
            event
        }
    );
    let _ = std::fs::remove_file(&path);
}

// --- Hardening --------------------------------------------------------------

fn lcg(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

#[test]
fn loader_never_panics_on_arbitrary_bytes() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for length in 0..256usize {
        for _ in 0..8 {
            let bytes: Vec<u8> = (0..length).map(|_| lcg(&mut state) as u8).collect();
            let _ = smf::load(&bytes);
        }
    }
}

#[test]
fn loader_never_panics_on_bit_flipped_fixtures() {
    let base = include_bytes!("fixtures/multitrack.mid");
    for index in 0..base.len() {
        for bit in 0..8 {
            let mut mutated = base.to_vec();
            mutated[index] ^= 1 << bit;
            let _ = smf::load(&mutated);
        }
    }
    let truncated = &base[..base.len() / 2];
    let _ = smf::load(truncated);
}

#[test]
fn metadata_decoder_never_panics_on_arbitrary_bytes() {
    let mut state = 0xDEAD_BEEF_CAFE_1234u64;
    for length in 0..512usize {
        let bytes: Vec<u8> = (0..length).map(|_| lcg(&mut state) as u8).collect();
        let _ = mtrak_midi_io::metadata::decode_project(&bytes);
    }
}

#[test]
fn global_settings_survive_a_save_load_cycle() {
    let mut project = project_with(4, 1);
    project.composition.global_settings_mut().root = 67;
    let child = project.composition.create_pattern(4).unwrap();

    let bytes = smf::save_project(&project, None).unwrap();
    let loaded = smf::load(&bytes).unwrap();
    assert_eq!(
        loaded
            .project
            .composition
            .effective_settings(PatternId(0))
            .unwrap()
            .root,
        67
    );
    // Settings are composition-global, so a different pattern sees the same value.
    assert_eq!(
        loaded
            .project
            .composition
            .effective_settings(child)
            .unwrap()
            .root,
        67
    );
}
