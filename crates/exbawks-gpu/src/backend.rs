use crate::{GraphicsCommand, GraphicsError};

/// A host graphics backend that consumes normalized HLE commands.
pub trait GraphicsBackend: Send {
    /// Executes one normalized graphics command.
    fn submit(&mut self, command: &GraphicsCommand) -> Result<(), GraphicsError>;
}

/// Counters from the deterministic null graphics backend.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NullGraphicsStats {
    /// The number of submitted commands.
    pub commands: u64,
    /// The number of draw commands.
    pub draws: u64,
    /// The number of present commands.
    pub presents: u64,
    /// The number of unknown methods.
    pub unknown_methods: u64,
}

/// A graphics backend that records counters and performs no host rendering.
#[derive(Debug, Default)]
pub struct NullGraphicsBackend {
    stats: NullGraphicsStats,
}

impl NullGraphicsBackend {
    /// Returns the current command counters.
    #[must_use]
    pub const fn stats(&self) -> NullGraphicsStats {
        self.stats
    }
}

impl GraphicsBackend for NullGraphicsBackend {
    fn submit(&mut self, command: &GraphicsCommand) -> Result<(), GraphicsError> {
        self.stats.commands = self.stats.commands.saturating_add(1);
        match command {
            GraphicsCommand::Draw { .. } => {
                self.stats.draws = self.stats.draws.saturating_add(1);
            }
            GraphicsCommand::Present => {
                self.stats.presents = self.stats.presents.saturating_add(1);
            }
            GraphicsCommand::UnknownMethod { .. } => {
                self.stats.unknown_methods = self.stats.unknown_methods.saturating_add(1);
            }
            GraphicsCommand::CreateDevice { .. } | GraphicsCommand::Clear { .. } => {}
        }

        Ok(())
    }
}
