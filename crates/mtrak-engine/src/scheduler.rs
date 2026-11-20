//! Deterministic tick scheduler.
//!
//! The engine advances one global TPB clock, maintains pattern instances with
//! track-local child ownership (decision D-008), and emits timestamped events.
//! It is fully deterministic and testable without MIDI hardware.

use std::collections::HashSet;

use mtrak_core::composition::{Composition, PatternId};
use mtrak_core::event::{macro_id, Event, MacroCall, MetaEvent};
use mtrak_core::settings::{EffectiveSettings, Scale, Setting, Settings};
use mtrak_core::timing::{row_ticks, Bpm, TempoMap, Ticks};

use crate::macros::{Macro, MacroAction, MacroCtx, MacroFactory, SettingChange, SettingScope};

/// A timestamped event emitted by the engine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TimedEvent {
    /// The global tick at which the event occurs.
    pub tick: Ticks,
    /// The event itself.
    pub event: Event,
}

/// A real-time or recording destination for engine output.
pub trait Sink {
    /// Receives an event at `tick`.
    fn emit(&mut self, tick: Ticks, event: &Event);
}

/// Default safety bound on the number of ticks a run may advance.
pub const DEFAULT_MAX_TICK: Ticks = 10_000_000;

struct Instance {
    id: u64,
    pattern: PatternId,
    start_tick: Ticks,
    row: usize,
    row_ticks: u32,
    owner: Option<(u64, usize)>,
    children: Vec<Option<u64>>,
}

struct MacroInstance {
    seq: u64,
    instance: u64,
    track: usize,
    macro_: Box<dyn Macro>,
    next_run: Ticks,
}

fn kind_rank(event: &Event) -> u8 {
    match event {
        Event::Meta(MetaEvent::Tempo(_)) => 0,
        Event::Meta(MetaEvent::TimeSignature(_)) => 1,
        Event::Meta(_) => 2,
        Event::SysEx(_) => 3,
        Event::Channel(_) | Event::Raw(_) => 4,
        Event::Macro(_) => 5,
        Event::Empty => 6,
    }
}

struct Due {
    instance: u64,
    seq: u64,
    track: usize,
    row: usize,
    offset: u32,
    event: Event,
}

impl Due {
    fn key(&self) -> (u8, u64, usize, usize, u32) {
        (
            kind_rank(&self.event),
            self.seq,
            self.track,
            self.row,
            self.offset,
        )
    }
}

/// The deterministic scheduler.
pub struct Engine {
    composition: Composition,
    tempo: TempoMap,
    tick: Ticks,
    max_tick: Ticks,
    overflowed: bool,
    next_id: u64,
    instances: Vec<Instance>,
    macros: Vec<MacroInstance>,
    factory: Option<Box<dyn MacroFactory>>,
    sink: Option<Box<dyn Sink>>,
    output: Vec<TimedEvent>,
    diagnostics: Vec<String>,
}

impl Engine {
    /// Creates an engine for `composition` at tick 0.
    pub fn new(composition: Composition) -> Self {
        let tempo = TempoMap::from_bpm(composition.bpm());
        Engine {
            composition,
            tempo,
            tick: 0,
            max_tick: DEFAULT_MAX_TICK,
            overflowed: false,
            next_id: 1,
            instances: Vec::new(),
            macros: Vec::new(),
            factory: None,
            sink: None,
            output: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// The composition being scheduled.
    pub fn composition(&self) -> &Composition {
        &self.composition
    }

    /// The current tick.
    pub fn tick(&self) -> Ticks {
        self.tick
    }

    /// The tempo map after the run so far.
    pub fn tempo_map(&self) -> &TempoMap {
        &self.tempo
    }

    /// The emitted events so far.
    pub fn output(&self) -> &[TimedEvent] {
        &self.output
    }

    /// Diagnostics recorded for ignored, undefined, or failing macros.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Consumes the engine, returning the emitted events.
    pub fn into_output(self) -> Vec<TimedEvent> {
        self.output
    }

    /// Whether the run hit the safety bound.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Sets the run safety bound.
    pub fn set_max_tick(&mut self, max_tick: Ticks) {
        self.max_tick = max_tick;
    }

    /// Installs a factory for user macro coroutines.
    pub fn set_macro_factory(&mut self, factory: Box<dyn MacroFactory>) {
        self.factory = Some(factory);
    }

    /// Installs a streaming output sink.
    pub fn set_sink(&mut self, sink: Box<dyn Sink>) {
        self.sink = Some(sink);
    }

    /// Starts a root pattern instance at the current tick.
    pub fn start(&mut self, pattern: PatternId) -> bool {
        let Some(pat) = self.composition.pattern(pattern) else {
            return false;
        };
        let row_ticks = match row_ticks(self.composition.tpb(), pat.lpb()) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let tracks = pat.track_count();
        let id = self.next_id;
        self.next_id += 1;
        self.instances.push(Instance {
            id,
            pattern,
            start_tick: self.tick,
            row: 0,
            row_ticks,
            owner: None,
            children: vec![None; tracks],
        });
        true
    }

    /// Runs until no instances or coroutines remain (or the safety bound hits).
    pub fn run(&mut self) {
        while !(self.instances.is_empty() && self.macros.is_empty()) {
            if self.tick > self.max_tick {
                self.overflowed = true;
                break;
            }
            self.step();
        }
    }

    /// Advances exactly one tick.
    pub fn step(&mut self) {
        self.advance_rows();
        let mut processed: HashSet<(u64, usize, usize)> = HashSet::new();
        loop {
            let mut changed = self.run_macros();
            let mut due = self.collect_due(&processed);
            if !due.is_empty() {
                due.sort_by_key(|a| a.key());
                for item in due {
                    processed.insert((item.instance, item.track, item.row));
                    self.handle_due(item);
                }
                changed = true;
            }
            if !changed {
                break;
            }
        }
        self.remove_ended();
        self.tick += 1;
    }

    fn advance_rows(&mut self) {
        let tick = self.tick;
        for inst in &mut self.instances {
            let Some(row_count) = self
                .composition
                .pattern(inst.pattern)
                .map(|p| p.row_count())
            else {
                continue;
            };
            while inst.row < row_count {
                let next_start =
                    inst.start_tick + (inst.row as u64 + 1) * u64::from(inst.row_ticks);
                if tick >= next_start {
                    inst.row += 1;
                } else {
                    break;
                }
            }
        }
    }

    fn collect_due(&self, processed: &HashSet<(u64, usize, usize)>) -> Vec<Due> {
        let mut due = Vec::new();
        for inst in &self.instances {
            let Some(pattern) = self.composition.pattern(inst.pattern) else {
                continue;
            };
            if inst.row >= pattern.row_count() {
                continue;
            }
            let row_start = inst.start_tick + inst.row as u64 * u64::from(inst.row_ticks);
            if self.tick < row_start {
                continue;
            }
            let delta = self.tick - row_start;
            if delta >= u64::from(inst.row_ticks) {
                continue;
            }
            for track in 0..pattern.track_count() {
                if processed.contains(&(inst.id, track, inst.row)) {
                    continue;
                }
                let Some(slot) = pattern.slot(inst.row, track) else {
                    continue;
                };
                if slot.is_empty() || u64::from(slot.offset) != delta {
                    continue;
                }
                due.push(Due {
                    instance: inst.id,
                    seq: inst.id,
                    track,
                    row: inst.row,
                    offset: slot.offset,
                    event: slot.event.clone(),
                });
            }
        }
        due
    }

    fn handle_due(&mut self, due: Due) {
        match &due.event {
            Event::Macro(call) => self.handle_macro(due.instance, due.track, *call),
            Event::Meta(MetaEvent::Tempo(micros)) => {
                self.tempo.insert(self.tick, *micros);
                self.emit(due.event.clone());
            }
            _ => {
                if due.event.is_complete() {
                    self.emit(due.event.clone());
                }
            }
        }
    }

    fn handle_macro(&mut self, instance: u64, track: usize, call: MacroCall) {
        match call.id {
            macro_id::BUILTIN_TRIGGER => {
                let pattern = PatternId(call.arg1);
                if self.composition.pattern(pattern).is_some() {
                    self.trigger(instance, track, pattern);
                }
            }
            macro_id::BUILTIN_TEMPO => {
                if let Some(bpm) = Bpm::from_macro_args(call.arg1, call.arg2) {
                    let micros = bpm.micros_per_quarter();
                    self.tempo.insert(self.tick, micros);
                    self.emit(Event::Meta(MetaEvent::Tempo(micros)));
                }
            }
            id if macro_id::is_user(id) => {
                let created = self.factory.as_mut().and_then(|f| f.create(call));
                match created {
                    Some(macro_) => {
                        self.macros.push(MacroInstance {
                            seq: self.next_id,
                            instance,
                            track,
                            macro_,
                            next_run: self.tick,
                        });
                        self.next_id += 1;
                    }
                    None => self
                        .diagnostics
                        .push(format!("no runtime available for user macro {id:#04X}")),
                }
            }
            other => self
                .diagnostics
                .push(format!("undefined or reserved macro id {other:#04X}")),
        }
    }

    fn run_macros(&mut self) -> bool {
        let due: Vec<u64> = self
            .macros
            .iter()
            .filter(|m| m.next_run <= self.tick)
            .map(|m| m.seq)
            .collect();
        if due.is_empty() {
            return false;
        }
        let mut results = Vec::new();
        for seq in due {
            let Some(idx) = self.macros.iter().position(|m| m.seq == seq) else {
                continue;
            };
            let owner_instance = self.macros[idx].instance;
            let owner_track = self.macros[idx].track;
            let settings = self.settings_for_instance(owner_instance);
            let mut ctx = MacroCtx::new(self.tick, owner_track, settings);
            let wait = self.macros[idx].macro_.resume(&mut ctx);
            let actions = ctx.into_actions();
            results.push((seq, owner_instance, owner_track, wait, actions));
        }
        for (seq, owner_instance, owner_track, wait, actions) in results {
            match wait {
                Some(wait) => {
                    if let Some(m) = self.macros.iter_mut().find(|m| m.seq == seq) {
                        m.next_run = self.tick + wait.max(1);
                    }
                }
                None => self.macros.retain(|m| m.seq != seq),
            }
            for action in actions {
                self.apply_action(owner_instance, owner_track, seq, action);
            }
        }
        true
    }

    fn apply_action(&mut self, instance: u64, track: usize, caller: u64, action: MacroAction) {
        match action {
            MacroAction::Emit(event) => self.emit(event),
            MacroAction::Trigger(pattern) => {
                if self.composition.pattern(pattern).is_some() {
                    self.trigger(instance, track, pattern);
                }
            }
            MacroAction::StopTrack { include_current } => {
                self.stop_track(instance, track, include_current, Some(caller));
            }
            MacroAction::SetSetting { scope, change } => {
                self.apply_setting(instance, scope, change);
            }
            MacroAction::Diagnostic(message) => self.diagnostics.push(message),
        }
    }

    fn apply_setting(&mut self, instance: u64, scope: SettingScope, change: SettingChange) {
        let pattern_id = self
            .instances
            .iter()
            .find(|i| i.id == instance)
            .map(|i| i.pattern);
        let target = match scope {
            SettingScope::Global => self.composition.global_settings_mut(),
            SettingScope::Pattern => match pattern_id.and_then(|p| self.composition.pattern_mut(p))
            {
                Some(pattern) => pattern.settings_mut(),
                None => return,
            },
        };
        match change {
            SettingChange::Root(value) => target.root = Setting::Set(value),
            SettingChange::Scale(mask) => target.scale = Setting::Set(Scale::from_mask(mask)),
            SettingChange::ModalDegree(value) => target.modal_degree = Setting::Set(value),
            SettingChange::ScaleAware(value) => target.scale_aware = Setting::Set(value),
        }
    }

    fn emit(&mut self, event: Event) {
        let tick = self.tick;
        if let Some(sink) = &mut self.sink {
            sink.emit(tick, &event);
        }
        self.output.push(TimedEvent { tick, event });
    }

    fn settings_for_instance(&self, id: u64) -> EffectiveSettings {
        if let Some(inst) = self.instances.iter().find(|i| i.id == id) {
            if let Some(effective) = self.composition.effective_settings(inst.pattern) {
                return effective;
            }
        }
        Settings::local().resolve(self.composition.global_settings())
    }

    /// Triggers `pattern` on `track` of the instance `parent_id`, replacing any
    /// child previously owned by that track (decision D-008). Coroutines are not
    /// cancelled by replacement (decision D-026).
    pub fn trigger(&mut self, parent_id: u64, track: usize, pattern: PatternId) -> bool {
        let Some(pat) = self.composition.pattern(pattern) else {
            return false;
        };
        let row_ticks = match row_ticks(self.composition.tpb(), pat.lpb()) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let tracks = pat.track_count();
        let Some(parent_idx) = self.instances.iter().position(|i| i.id == parent_id) else {
            return false;
        };
        if track >= self.instances[parent_idx].children.len() {
            return false;
        }
        if let Some(old) = self.instances[parent_idx].children[track].take() {
            self.remove_tree(old);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.instances.push(Instance {
            id,
            pattern,
            start_tick: self.tick,
            row: 0,
            row_ticks,
            owner: Some((parent_id, track)),
            children: vec![None; tracks],
        });
        // Re-locate the parent in case remove_tree shifted indices.
        if let Some(parent_idx) = self.instances.iter().position(|i| i.id == parent_id) {
            if track < self.instances[parent_idx].children.len() {
                self.instances[parent_idx].children[track] = Some(id);
            }
        }
        true
    }

    /// Stops the child instance and coroutines associated with `track`.
    ///
    /// `include_current` additionally stops the instance itself; when `caller`
    /// names a coroutine, that coroutine is preserved unless `include_current`
    /// is set (decision D-026).
    pub fn stop_track(
        &mut self,
        instance_id: u64,
        track: usize,
        include_current: bool,
        caller: Option<u64>,
    ) {
        if let Some(idx) = self.instances.iter().position(|i| i.id == instance_id) {
            if track < self.instances[idx].children.len() {
                if let Some(child) = self.instances[idx].children[track].take() {
                    self.remove_tree(child);
                }
            }
        }
        self.macros.retain(|m| {
            let same_track = m.instance == instance_id && m.track == track;
            if !same_track {
                return true;
            }
            if include_current {
                return false;
            }
            caller == Some(m.seq)
        });
        if include_current {
            self.remove_tree(instance_id);
        }
    }

    fn remove_ended(&mut self) {
        let ended: Vec<u64> = self
            .instances
            .iter()
            .filter(|inst| {
                self.composition
                    .pattern(inst.pattern)
                    .map(|p| inst.row >= p.row_count())
                    .unwrap_or(true)
            })
            .map(|inst| inst.id)
            .collect();
        for id in ended {
            if self.instances.iter().any(|i| i.id == id) {
                self.remove_tree(id);
            }
        }
    }

    fn remove_tree(&mut self, root: u64) {
        let mut ids = vec![root];
        let mut cursor = 0;
        while cursor < ids.len() {
            let current = ids[cursor];
            cursor += 1;
            for inst in &self.instances {
                if let Some((owner, _)) = inst.owner {
                    if owner == current && !ids.contains(&inst.id) {
                        ids.push(inst.id);
                    }
                }
            }
        }
        self.instances.retain(|i| !ids.contains(&i.id));
        for inst in &mut self.instances {
            for child in &mut inst.children {
                if let Some(cid) = *child {
                    if ids.contains(&cid) {
                        *child = None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macros::{Macro, MacroFactory};
    use mtrak_core::composition::Slot;
    use mtrak_core::event::{ChannelKind, ChannelMessage};

    fn note(key: u8) -> Event {
        Event::Channel(ChannelMessage::with_data(
            ChannelKind::NoteOn,
            0,
            key,
            Some(100),
        ))
    }

    fn trigger(pattern: PatternId) -> Event {
        Event::Macro(MacroCall::new(
            macro_id::BUILTIN_TRIGGER,
            pattern.value(),
            0,
        ))
    }

    fn comp_with(rows: usize, tracks: usize, lpb: u32) -> Composition {
        let mut composition = Composition::new();
        let pattern = composition.pattern_mut(PatternId(0)).unwrap();
        pattern.resize_rows(rows).unwrap();
        pattern.resize_tracks(tracks).unwrap();
        if lpb != 4 {
            composition.set_pattern_lpb(PatternId(0), lpb).unwrap();
        }
        composition
    }

    fn ticks_of(engine: &Engine, key: u8) -> Vec<Ticks> {
        engine
            .output()
            .iter()
            .filter(|te| matches!(&te.event, Event::Channel(m) if m.data1 == Some(key)))
            .map(|te| te.tick)
            .collect()
    }

    #[test]
    fn emits_on_grid_and_off_grid_then_stops_at_final_row() {
        let mut composition = comp_with(4, 1, 4); // tpb 24, row = 6 ticks
        {
            let pattern = composition.pattern_mut(PatternId(0)).unwrap();
            pattern.set_event(0, 0, note(60));
            pattern.set_event(2, 0, note(62));
            *pattern.slot_mut(1, 0).unwrap() = Slot {
                event: note(64),
                offset: 3,
            };
        }
        let mut engine = Engine::new(composition);
        assert!(engine.start(PatternId(0)));
        engine.run();
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(ticks_of(&engine, 64), vec![9]); // row 1 (tick 6) + offset 3
        assert_eq!(ticks_of(&engine, 62), vec![12]);
        assert!(!engine.overflowed());
    }

    #[test]
    fn nested_patterns_with_different_lpb_share_one_clock() {
        let mut composition = comp_with(2, 1, 4);
        let child = composition.create_pattern(5).unwrap(); // tpb 24 -> 120
        {
            let pattern = composition.pattern_mut(child).unwrap();
            pattern.resize_rows(2).unwrap();
            pattern.set_event(0, 0, note(60));
            pattern.set_event(1, 0, note(62));
        }
        composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, trigger(child));
        let mut engine = Engine::new(composition);
        assert_eq!(engine.composition().tpb(), 120);
        engine.start(PatternId(0));
        engine.run();
        // Parent row = 30 ticks; child row = 24 ticks.
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(ticks_of(&engine, 62), vec![24]);
    }

    #[test]
    fn a_later_trigger_replaces_the_child_and_keeps_the_parent_running() {
        let mut composition = comp_with(3, 1, 4); // row = 6 ticks
        let long = composition.create_pattern(4).unwrap();
        let short = composition.create_pattern(4).unwrap();
        {
            let p = composition.pattern_mut(long).unwrap();
            p.resize_rows(4).unwrap();
            for (i, key) in [60u8, 61, 62, 63].iter().enumerate() {
                p.set_event(i, 0, note(*key));
            }
        }
        {
            let p = composition.pattern_mut(short).unwrap();
            p.resize_rows(2).unwrap();
            p.set_event(0, 0, note(70));
            p.set_event(1, 0, note(71));
        }
        {
            let parent = composition.pattern_mut(PatternId(0)).unwrap();
            parent.set_event(0, 0, trigger(long)); // tick 0
            parent.set_event(2, 0, trigger(short)); // tick 12 replaces `long`
        }
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();
        // `long` sounded rows 0..=2 (ticks 0, 6, 12) then was replaced.
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(ticks_of(&engine, 61), vec![6]);
        assert_eq!(ticks_of(&engine, 62), vec![12]);
        assert!(ticks_of(&engine, 63).is_empty(), "replaced child must stop");
        assert_eq!(ticks_of(&engine, 70), vec![12]);
        assert_eq!(ticks_of(&engine, 71), vec![18]);
    }

    #[test]
    fn root_end_tears_down_its_children() {
        let mut composition = comp_with(1, 1, 4); // root ends at tick 6
        let child = composition.create_pattern(4).unwrap();
        {
            let p = composition.pattern_mut(child).unwrap();
            p.resize_rows(4).unwrap();
            for (i, key) in [60u8, 61, 62, 63].iter().enumerate() {
                p.set_event(i, 0, note(*key));
            }
        }
        composition
            .pattern_mut(PatternId(0))
            .unwrap()
            .set_event(0, 0, trigger(child));
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(
            ticks_of(&engine, 61),
            vec![6],
            "tick at the root's end still fires"
        );
        assert!(ticks_of(&engine, 62).is_empty());
    }

    #[test]
    fn simultaneous_events_are_ordered_by_track_then_row() {
        let mut composition = comp_with(2, 2, 4);
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 1, note(61));
            p.set_event(0, 0, note(60));
        }
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();
        let keys: Vec<u8> = engine
            .output()
            .iter()
            .filter_map(|te| match &te.event {
                Event::Channel(m) if m.kind == ChannelKind::NoteOn => m.data1,
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![60, 61]);
    }

    #[test]
    fn tempo_macro_changes_timing_from_its_position() {
        let mut composition = comp_with(3, 1, 4);
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 0, note(60));
            // 120 BPM -> 240 BPM at row 1 (tick 6), encoded as Q8.8 (240, 0).
            p.set_event(
                1,
                0,
                Event::Macro(MacroCall::new(macro_id::BUILTIN_TEMPO, 240, 0)),
            );
        }
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();
        assert_eq!(engine.tempo_map().tempo_at(0), 500_000);
        assert_eq!(engine.tempo_map().tempo_at(6), 250_000);
        assert!(engine.output().iter().any(|te| matches!(
            &te.event,
            Event::Meta(MetaEvent::Tempo(m)) if *m == 250_000
        )));
    }

    #[test]
    fn runs_are_deterministic() {
        let mut composition = comp_with(4, 2, 4);
        let child = composition.create_pattern(5).unwrap();
        {
            let p = composition.pattern_mut(child).unwrap();
            p.set_event(0, 0, note(70));
            p.set_event(1, 0, note(71));
        }
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 0, note(60));
            p.set_event(1, 1, trigger(child));
            p.set_event(3, 0, note(62));
        }
        let mut first = Engine::new(composition.clone());
        first.start(PatternId(0));
        first.run();
        let mut second = Engine::new(composition);
        second.start(PatternId(0));
        second.run();
        assert_eq!(first.output(), second.output());
        assert!(!first.output().is_empty());
    }

    #[test]
    fn undefined_and_reserved_macro_ids_are_rejected_with_diagnostics() {
        let mut composition = comp_with(2, 1, 4);
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 0, Event::Macro(MacroCall::new(0x03, 0, 0))); // undefined built-in
            p.set_event(1, 0, Event::Macro(MacroCall::new(0x80, 0, 0))); // reserved range
        }
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();
        assert_eq!(engine.diagnostics().len(), 2);
        assert!(engine.diagnostics()[0].contains("03"));
        assert!(engine.diagnostics()[1].contains("80"));
    }

    // --- Coroutine cancellation (decision D-026) -------------------------------

    struct StopMacro {
        include: bool,
        fired: bool,
    }

    impl Macro for StopMacro {
        fn resume(&mut self, ctx: &mut MacroCtx) -> Option<Ticks> {
            if !self.fired {
                self.fired = true;
                ctx.stop_track(self.include);
                Some(1)
            } else {
                ctx.emit(note(90));
                None
            }
        }
    }

    struct StopFactory {
        include: bool,
    }

    impl MacroFactory for StopFactory {
        fn create(&mut self, call: MacroCall) -> Option<Box<dyn Macro>> {
            (call.id == 0x40).then(|| {
                Box::new(StopMacro {
                    include: self.include,
                    fired: false,
                }) as Box<dyn Macro>
            })
        }
    }

    struct EmitMacro {
        fired: bool,
    }

    impl Macro for EmitMacro {
        fn resume(&mut self, ctx: &mut MacroCtx) -> Option<Ticks> {
            if !self.fired {
                self.fired = true;
                Some(12)
            } else {
                ctx.emit(note(90));
                None
            }
        }
    }

    struct EmitFactory;

    impl MacroFactory for EmitFactory {
        fn create(&mut self, call: MacroCall) -> Option<Box<dyn Macro>> {
            (call.id == 0x40).then(|| Box::new(EmitMacro { fired: false }) as Box<dyn Macro>)
        }
    }

    fn stop_track_case(include: bool) -> Engine {
        let mut composition = comp_with(4, 1, 4); // row = 6 ticks
        let child = composition.create_pattern(4).unwrap();
        {
            let p = composition.pattern_mut(child).unwrap();
            p.resize_rows(10).unwrap();
            for i in 0..10 {
                p.set_event(i, 0, note(60 + i as u8));
            }
        }
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 0, trigger(child)); // tick 0
            p.set_event(1, 0, Event::Macro(MacroCall::new(0x40, 0, 0))); // tick 6
        }
        let mut engine = Engine::new(composition);
        engine.set_macro_factory(Box::new(StopFactory { include }));
        engine.start(PatternId(0));
        engine.run();
        engine
    }

    #[test]
    fn stop_track_false_keeps_the_caller_alive() {
        let engine = stop_track_case(false);
        // The child sounded rows 0 and 1 (ticks 0, 6) before being stopped.
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(ticks_of(&engine, 61), vec![6]);
        assert!(
            ticks_of(&engine, 62).is_empty(),
            "child instance must be stopped"
        );
        // The caller survives and emits its second-resume marker.
        assert_eq!(ticks_of(&engine, 90), vec![7]);
    }

    #[test]
    fn stop_track_true_stops_the_caller_too() {
        let engine = stop_track_case(true);
        assert_eq!(ticks_of(&engine, 60), vec![0]);
        assert_eq!(ticks_of(&engine, 61), vec![6]);
        assert!(ticks_of(&engine, 62).is_empty());
        // The caller was cancelled before it could emit its marker.
        assert!(ticks_of(&engine, 90).is_empty());
        assert!(!engine.overflowed());
    }

    #[test]
    fn replacement_does_not_cancel_a_running_coroutine() {
        let mut composition = comp_with(3, 1, 4);
        let child = composition.create_pattern(4).unwrap();
        {
            let p = composition.pattern_mut(child).unwrap();
            p.resize_rows(4).unwrap();
            p.set_event(0, 0, note(60));
            p.set_event(1, 0, note(61));
        }
        {
            let p = composition.pattern_mut(PatternId(0)).unwrap();
            p.set_event(0, 0, Event::Macro(MacroCall::new(0x40, 0, 0))); // starts coroutine
            p.set_event(1, 0, trigger(child)); // replaces child on the same track
        }
        let mut engine = Engine::new(composition);
        engine.set_macro_factory(Box::new(EmitFactory));
        engine.start(PatternId(0));
        engine.run();
        // The coroutine keeps running across the replacement and emits at tick 12.
        assert_eq!(ticks_of(&engine, 90), vec![12]);
        assert!(!engine.overflowed());
    }
}
