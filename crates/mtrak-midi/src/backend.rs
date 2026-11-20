//! Portable MIDI backend abstraction.
//!
//! The engine's output is delivered to a single globally selected output port
//! (decision D-013). The real `midir` implementation lives behind the
//! `midir-backend` feature; a [`MockBackend`] keeps everything testable without
//! hardware. A missing or disconnected port drops output while the timeline
//! keeps running (decision D-028).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mtrak_core::event::Event;
use mtrak_engine::scheduler::Sink;

/// Errors from the MIDI backend layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MidiError {
    /// The backend could not be initialized.
    #[error("could not initialize MIDI backend: {0}")]
    Init(String),
    /// A device port could not be queried.
    #[error("could not query MIDI port: {0}")]
    Port(String),
    /// The requested port index does not exist.
    #[error("no MIDI output port with index {0}")]
    NoSuchPort(usize),
    /// The selected port could not be opened.
    #[error("could not open MIDI port: {0}")]
    Connect(String),
    /// Sending a message failed.
    #[error("could not send MIDI message: {0}")]
    Send(String),
}

/// A discoverable MIDI output port.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PortInfo {
    /// Stable index within the backend's current enumeration.
    pub id: usize,
    /// Human-readable port name.
    pub name: String,
}

/// An open MIDI output connection.
pub trait MidiOutput {
    /// Sends a raw MIDI message.
    fn send(&mut self, message: &[u8]) -> Result<(), MidiError>;
    /// The name of the underlying port.
    fn port_name(&self) -> &str;
}

/// A source of MIDI output ports.
pub trait MidiBackend {
    /// Lists the currently available output ports.
    fn list_outputs(&self) -> Result<Vec<PortInfo>, MidiError>;
    /// Opens the output port with the given index.
    fn open_output(&self, id: usize) -> Result<Box<dyn MidiOutput>, MidiError>;
}

impl MidiBackend for Box<dyn MidiBackend> {
    fn list_outputs(&self) -> Result<Vec<PortInfo>, MidiError> {
        (**self).list_outputs()
    }

    fn open_output(&self, id: usize) -> Result<Box<dyn MidiOutput>, MidiError> {
        (**self).open_output(id)
    }
}

/// Owns the globally selected output port and routes engine events to it.
pub struct OutputManager<B: MidiBackend> {
    backend: B,
    selected: Option<usize>,
    output: Option<Box<dyn MidiOutput>>,
    last_error: Option<String>,
}

impl<B: MidiBackend> OutputManager<B> {
    /// Creates a manager over `backend` with no port selected.
    pub fn new(backend: B) -> Self {
        OutputManager {
            backend,
            selected: None,
            output: None,
            last_error: None,
        }
    }

    /// The underlying backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Lists available output ports.
    pub fn list_outputs(&self) -> Result<Vec<PortInfo>, MidiError> {
        self.backend.list_outputs()
    }

    /// The index of the selected port, if any.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Whether a port is currently open.
    pub fn is_connected(&self) -> bool {
        self.output.is_some()
    }

    /// The most recent error, if the connection was dropped.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Selects and opens an output port.
    pub fn select(&mut self, id: usize) -> Result<(), MidiError> {
        let ports = self.backend.list_outputs()?;
        if !ports.iter().any(|port| port.id == id) {
            return Err(MidiError::NoSuchPort(id));
        }
        let output = self.backend.open_output(id)?;
        self.selected = Some(id);
        self.output = Some(output);
        self.last_error = None;
        Ok(())
    }

    /// Closes the current port without forgetting the selection.
    pub fn disconnect(&mut self) {
        self.output = None;
    }

    /// Re-opens the last selected port after a disconnect.
    pub fn reconnect(&mut self) -> Result<(), MidiError> {
        match self.selected {
            Some(id) => self.select(id),
            None => Ok(()),
        }
    }

    /// Sends a raw message, dropping it if no port is open.
    ///
    /// A send failure drops the connection and records the error so the caller
    /// can reconnect; the caller's timeline is unaffected (decision D-028).
    pub fn send_bytes(&mut self, message: &[u8]) -> Result<(), MidiError> {
        let Some(output) = self.output.as_mut() else {
            return Ok(());
        };
        match output.send(message) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.output = None;
                self.last_error = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Sends an event, ignoring events that have no live-MIDI representation.
    pub fn send_event(&mut self, event: &Event) -> Result<(), MidiError> {
        match event.to_midi_bytes() {
            Ok(bytes) => self.send_bytes(&bytes),
            Err(_) => Ok(()),
        }
    }
}

/// An [`mtrak_engine::scheduler::Sink`] that streams events to a manager.
pub struct EngineSink<B: MidiBackend> {
    manager: OutputManager<B>,
}

impl<B: MidiBackend> EngineSink<B> {
    /// Wraps `manager` as an engine sink.
    pub fn new(manager: OutputManager<B>) -> Self {
        EngineSink { manager }
    }

    /// The wrapped manager.
    pub fn manager(&self) -> &OutputManager<B> {
        &self.manager
    }

    /// Mutable access to the wrapped manager.
    pub fn manager_mut(&mut self) -> &mut OutputManager<B> {
        &mut self.manager
    }
}

impl<B: MidiBackend + 'static> Sink for EngineSink<B> {
    fn emit(&mut self, _tick: u64, event: &Event) {
        let _ = self.manager.send_event(event);
    }
}

/// A backend that records sent messages instead of using hardware.
#[derive(Clone)]
pub struct MockBackend {
    ports: Vec<String>,
    sent: Rc<RefCell<Vec<Vec<u8>>>>,
    fail: Rc<Cell<bool>>,
}

impl MockBackend {
    /// Creates a mock backend with the given port names.
    pub fn new(ports: &[&str]) -> Self {
        MockBackend {
            ports: ports.iter().map(|name| (*name).to_string()).collect(),
            sent: Rc::new(RefCell::new(Vec::new())),
            fail: Rc::new(Cell::new(false)),
        }
    }

    /// The messages sent so far.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.sent.borrow().clone()
    }

    /// Makes subsequent sends fail, simulating a disconnect.
    pub fn set_fail(&self, fail: bool) {
        self.fail.set(fail);
    }

    /// A shared handle to the recorded messages, for observing them after the
    /// backend has been moved into a manager.
    pub fn sent_handle(&self) -> Rc<RefCell<Vec<Vec<u8>>>> {
        self.sent.clone()
    }
}

impl MidiBackend for MockBackend {
    fn list_outputs(&self) -> Result<Vec<PortInfo>, MidiError> {
        Ok(self
            .ports
            .iter()
            .enumerate()
            .map(|(id, name)| PortInfo {
                id,
                name: name.clone(),
            })
            .collect())
    }

    fn open_output(&self, id: usize) -> Result<Box<dyn MidiOutput>, MidiError> {
        let name = self.ports.get(id).ok_or(MidiError::NoSuchPort(id))?.clone();
        Ok(Box::new(MockOutput {
            name,
            sent: self.sent.clone(),
            fail: self.fail.clone(),
        }))
    }
}

struct MockOutput {
    name: String,
    sent: Rc<RefCell<Vec<Vec<u8>>>>,
    fail: Rc<Cell<bool>>,
}

impl MidiOutput for MockOutput {
    fn send(&mut self, message: &[u8]) -> Result<(), MidiError> {
        if self.fail.get() {
            return Err(MidiError::Send("mock failure".to_string()));
        }
        self.sent.borrow_mut().push(message.to_vec());
        Ok(())
    }

    fn port_name(&self) -> &str {
        &self.name
    }
}

/// A raw message received from an input source.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InputMessage {
    /// Monotonic receipt time in microseconds.
    pub timestamp_micros: u64,
    /// Raw MIDI bytes.
    pub bytes: Vec<u8>,
}

/// The input-ingestion boundary used by future recording.
///
/// v1 does not record, but the boundary keeps the eventual feature a drop-in.
pub trait InputSource {
    /// Returns the next pending message, or `None`.
    fn try_recv(&mut self) -> Option<InputMessage>;
}

/// Converts a raw input message into a typed event, if it is a channel message.
pub fn ingest_message(message: &InputMessage) -> Option<Event> {
    Event::from_smf_bytes(&message.bytes)
        .ok()
        .map(|(event, _)| event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::composition::{Composition, PatternId};
    use mtrak_core::event::{ChannelKind, ChannelMessage, MetaEvent};
    use mtrak_engine::scheduler::Engine;

    #[test]
    fn lists_and_selects_ports() {
        let backend = MockBackend::new(&["out A", "out B"]);
        let mut manager = OutputManager::new(backend);
        let ports = manager.list_outputs().unwrap();
        assert_eq!(
            ports.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["out A", "out B"]
        );
        assert!(manager.select(5).is_err());
        manager.select(1).unwrap();
        assert_eq!(manager.selected(), Some(1));
        assert!(manager.is_connected());
    }

    #[test]
    fn engine_output_reaches_the_selected_port() {
        let backend = MockBackend::new(&["out"]);
        let sent = backend.sent.clone();
        let mut manager = OutputManager::new(backend);
        manager.select(0).unwrap();

        let mut composition = Composition::new();
        {
            let pattern = composition.pattern_mut(PatternId(0)).unwrap();
            pattern.resize_rows(2).unwrap();
            pattern.set_event(
                0,
                0,
                Event::Channel(ChannelMessage::with_data(
                    ChannelKind::NoteOn,
                    0,
                    60,
                    Some(100),
                )),
            );
            // Meta-events have no live representation and must be skipped.
            pattern.set_event(1, 0, Event::Meta(MetaEvent::Tempo(500_000)));
        }
        let mut engine = Engine::new(composition);
        engine.start(PatternId(0));
        engine.run();

        for timed in engine.output() {
            manager.send_event(&timed.event).unwrap();
        }
        assert_eq!(sent.borrow().as_slice(), &[vec![0x90, 60, 100]]);
    }

    #[test]
    fn send_failure_drops_the_connection_and_reconnects() {
        let backend = MockBackend::new(&["out"]);
        backend.set_fail(true);
        let mut manager = OutputManager::new(backend);
        manager.select(0).unwrap();
        assert!(manager.send_bytes(&[0x90, 60, 100]).is_err());
        assert!(!manager.is_connected());
        assert!(manager.last_error().is_some());
        // Reconnecting restores output once the failure clears.
        manager.backend().set_fail(false);
        manager.reconnect().unwrap();
        assert!(manager.is_connected());
        manager.send_bytes(&[0x90, 60, 100]).unwrap();
        assert_eq!(manager.backend().sent().len(), 1);
    }

    #[test]
    fn output_without_a_port_is_dropped_silently() {
        let backend = MockBackend::new(&["out"]);
        let mut manager = OutputManager::new(backend);
        assert!(manager.send_bytes(&[0x90, 60, 100]).is_ok());
        assert!(!manager.is_connected());
    }

    #[test]
    fn input_messages_ingest_to_events() {
        let message = InputMessage {
            timestamp_micros: 10,
            bytes: vec![0x90, 60, 100],
        };
        let event = ingest_message(&message).unwrap();
        assert_eq!(
            event,
            Event::Channel(ChannelMessage::with_data(
                ChannelKind::NoteOn,
                0,
                60,
                Some(100)
            ))
        );
        let garbage = InputMessage {
            timestamp_micros: 11,
            bytes: vec![0x7F],
        };
        assert!(ingest_message(&garbage).is_none());
    }
}
