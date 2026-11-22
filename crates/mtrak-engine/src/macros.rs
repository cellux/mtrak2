//! Macro coroutine abstraction.
//!
//! The scheduler resumes each active coroutine once per engine tick. A macro may
//! emit events, trigger child patterns, and stop the track that invoked it. The
//! scheduler applies the collected [`MacroAction`]s after the coroutine yields,
//! which keeps the engine's ownership state free of aliasing during a resume.

use mtrak_core::composition::PatternId;
use mtrak_core::event::{Event, MacroCall};
use mtrak_core::settings::Settings;
use mtrak_core::timing::Ticks;

/// A setting value a macro asked to change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SettingChange {
    /// Root note as a MIDI note number.
    Root(u8),
    /// Scale as a 12-bit pitch-class mask.
    Scale(u16),
    /// Modal degree within the scale.
    ModalDegree(u8),
    /// Whether note entry is scale-aware.
    ScaleAware(bool),
}

/// An effect requested by a running macro.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MacroAction {
    /// Emit a MIDI/custom event at the current tick.
    Emit(Event),
    /// Trigger a child pattern on the invoking track.
    Trigger(PatternId),
    /// Stop the invoking track's coroutines and child instances.
    StopTrack {
        /// Whether the calling coroutine is stopped too.
        include_current: bool,
    },
    /// Change a composition-global setting.
    SetSetting {
        /// The change to apply.
        change: SettingChange,
    },
    /// Report a diagnostic without corrupting engine state.
    Diagnostic(String),
}

/// The context handed to a macro when it is resumed.
pub struct MacroCtx {
    /// The tick at which the macro is being resumed.
    pub tick: Ticks,
    /// The track that invoked the macro.
    pub track: usize,
    /// The composition's ticks per beat, used to convert beat-denominated
    /// macro waits into ticks.
    tpb: u32,
    settings: Settings,
    actions: Vec<MacroAction>,
}

impl MacroCtx {
    /// Creates a context for a macro resumed at `tick` on `track`.
    pub fn new(tick: Ticks, track: usize, tpb: u32, settings: Settings) -> Self {
        MacroCtx {
            tick,
            track,
            tpb,
            settings,
            actions: Vec::new(),
        }
    }

    /// The effective settings visible to the macro.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// The composition's ticks per beat.
    pub fn tpb(&self) -> u32 {
        self.tpb
    }

    /// Requests that `event` be emitted at the current tick.
    pub fn emit(&mut self, event: Event) {
        self.actions.push(MacroAction::Emit(event));
    }

    /// Requests a child pattern be triggered on the invoking track.
    pub fn trigger(&mut self, pattern: PatternId) {
        self.actions.push(MacroAction::Trigger(pattern));
    }

    /// Requests that the invoking track be stopped.
    ///
    /// With `include_current == true` the calling coroutine is stopped as well;
    /// with `false` it keeps running.
    pub fn stop_track(&mut self, include_current: bool) {
        self.actions
            .push(MacroAction::StopTrack { include_current });
    }

    /// Pushes an arbitrary action collected by a macro implementation.
    pub fn push(&mut self, action: MacroAction) {
        self.actions.push(action);
    }

    /// Consumes the context, yielding the actions it collected.
    pub fn into_actions(self) -> Vec<MacroAction> {
        self.actions
    }
}

/// A resumable macro coroutine.
pub trait Macro {
    /// Resumes the macro, returning how many ticks to wait before the next
    /// resume, or `None` when the macro has finished.
    fn resume(&mut self, ctx: &mut MacroCtx) -> Option<Ticks>;
}

/// Creates macro coroutines for user macro events.
pub trait MacroFactory {
    /// Creates a coroutine for `call`, or `None` if the ID is unknown.
    fn create(&mut self, call: MacroCall) -> Option<Box<dyn Macro>>;
}
