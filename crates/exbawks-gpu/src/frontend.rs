use crate::{GraphicsBackend, GraphicsCommand, GraphicsError};

/// Stateful graphics HLE frontend.
#[derive(Debug)]
pub struct GraphicsFrontend<B> {
    backend: B,
    device_created: bool,
    command_index: u64,
}

impl<B> GraphicsFrontend<B>
where
    B: GraphicsBackend,
{
    /// Creates a frontend for one backend.
    #[must_use]
    pub const fn new(backend: B) -> Self {
        Self { backend, device_created: false, command_index: 0 }
    }

    /// Submits one normalized command after state validation.
    pub fn submit(&mut self, command: GraphicsCommand) -> Result<(), GraphicsError> {
        match &command {
            GraphicsCommand::CreateDevice { .. } if self.device_created => {
                return Err(GraphicsError::DeviceAlreadyCreated);
            }
            GraphicsCommand::CreateDevice { .. } => {}
            _ if !self.device_created => return Err(GraphicsError::DeviceNotCreated),
            _ => {}
        }

        tracing::trace!(index = self.command_index, ?command, "submitting graphics HLE command");
        self.backend.submit(&command)?;
        self.command_index = self.command_index.saturating_add(1);

        if matches!(&command, GraphicsCommand::CreateDevice { .. }) {
            self.device_created = true;
        }

        Ok(())
    }

    /// Returns true after successful device creation.
    #[must_use]
    pub const fn device_created(&self) -> bool {
        self.device_created
    }

    /// Returns the number of successfully submitted commands.
    #[must_use]
    pub const fn command_index(&self) -> u64 {
        self.command_index
    }

    /// Returns a shared backend reference.
    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Returns a mutable backend reference.
    pub const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Consumes the frontend and returns the backend.
    #[must_use]
    pub fn into_backend(self) -> B {
        self.backend
    }
}

#[cfg(test)]
mod tests {
    use crate::{ClearMask, NullGraphicsBackend, PrimitiveType};

    use super::*;

    #[test]
    fn frontend_requires_device_creation() {
        let mut frontend = GraphicsFrontend::new(NullGraphicsBackend::default());
        assert_eq!(frontend.submit(GraphicsCommand::Present), Err(GraphicsError::DeviceNotCreated));
    }

    #[test]
    fn null_backend_records_commands() {
        let mut frontend = GraphicsFrontend::new(NullGraphicsBackend::default());
        frontend
            .submit(GraphicsCommand::CreateDevice { width: 640, height: 480 })
            .expect("device creation must succeed");
        frontend
            .submit(GraphicsCommand::Clear {
                mask: ClearMask { color: true, depth: true, stencil: false },
                color: [0.0, 0.0, 0.0, 1.0],
                depth: 1.0,
                stencil: 0,
            })
            .expect("clear must succeed");
        frontend
            .submit(GraphicsCommand::Draw {
                primitive: PrimitiveType::TriangleList,
                first_vertex: 0,
                vertex_count: 3,
            })
            .expect("draw must succeed");
        frontend.submit(GraphicsCommand::Present).expect("present must succeed");

        let stats = frontend.backend().stats();
        assert_eq!(stats.commands, 4);
        assert_eq!(stats.draws, 1);
        assert_eq!(stats.presents, 1);
    }
}
