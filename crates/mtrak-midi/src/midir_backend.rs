//! Real `midir`-backed MIDI output.
//!
//! Enabled by the `midir-backend` feature. `midir` follows the platform's
//! native MIDI stack (ALSA on Linux) and is kept behind [`crate::backend`] so
//! the rest of the workspace never depends on it.
//!
//! `midir`'s `MidiOutput::connect` consumes the handle, so the connection owns
//! the output; the backend re-creates a handle to enumerate or (re)connect.

use midir::MidiOutput as MidirOutput;

use crate::backend::{MidiBackend, MidiError, MidiOutput, PortInfo};

/// A `midir`-backed output backend.
pub struct MidirBackend;

impl MidirBackend {
    /// Verifies the platform MIDI stack is available.
    pub fn new() -> Result<Self, MidiError> {
        MidirOutput::new("mtrak").map_err(|error| MidiError::Init(format!("{error:?}")))?;
        Ok(MidirBackend)
    }
}

impl Default for MidirBackend {
    fn default() -> Self {
        MidirBackend
    }
}

impl MidiBackend for MidirBackend {
    fn list_outputs(&self) -> Result<Vec<PortInfo>, MidiError> {
        let output =
            MidirOutput::new("mtrak").map_err(|error| MidiError::Init(format!("{error:?}")))?;
        output
            .ports()
            .iter()
            .enumerate()
            .map(|(id, port)| {
                let name = output
                    .port_name(port)
                    .map_err(|error| MidiError::Port(format!("{error:?}")))?;
                Ok(PortInfo { id, name })
            })
            .collect()
    }

    fn open_output(&self, id: usize) -> Result<Box<dyn MidiOutput>, MidiError> {
        let output =
            MidirOutput::new("mtrak").map_err(|error| MidiError::Init(format!("{error:?}")))?;
        let ports = output.ports();
        let port = ports.get(id).ok_or(MidiError::NoSuchPort(id))?;
        let name = output
            .port_name(port)
            .map_err(|error| MidiError::Port(format!("{error:?}")))?;
        let connection = output
            .connect(port, "mtrak-out")
            .map_err(|error| MidiError::Connect(format!("{error:?}")))?;
        Ok(Box::new(MidirConnection { connection, name }))
    }
}

struct MidirConnection {
    connection: midir::MidiOutputConnection,
    name: String,
}

impl MidiOutput for MidirConnection {
    fn send(&mut self, message: &[u8]) -> Result<(), MidiError> {
        self.connection
            .send(message)
            .map_err(|error| MidiError::Send(format!("{error:?}")))
    }

    fn port_name(&self) -> &str {
        &self.name
    }
}
