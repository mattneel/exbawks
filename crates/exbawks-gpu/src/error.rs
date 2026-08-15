use thiserror::Error;

/// A graphics frontend or backend failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GraphicsError {
    /// A device already exists.
    #[error("the graphics device already exists")]
    DeviceAlreadyCreated,
    /// A command requires a device.
    #[error("the graphics device is not created")]
    DeviceNotCreated,
    /// A backend rejected a command.
    #[error("graphics backend failure: {0}")]
    Backend(&'static str),
}
