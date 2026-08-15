use exbawks_types::GuestVa;
use thiserror::Error;

/// An XBE parsing or validation failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum XbeError {
    /// The file ended before a required field.
    #[error("XBE field {field} at file offset 0x{offset:X} exceeds the file")]
    Truncated { field: &'static str, offset: usize },
    /// The file does not start with `XBEH`.
    #[error("file does not contain the XBEH magic")]
    InvalidMagic,
    /// A header range is internally inconsistent.
    #[error("invalid XBE header range: {0}")]
    InvalidHeader(&'static str),
    /// An encoded address does not produce a valid image address.
    #[error("encoded XBE {field} does not decode inside the image")]
    InvalidEncodedAddress { field: &'static str },
    /// A virtual address does not map into the file-backed header range.
    #[error("XBE virtual address {address} for {field} does not map into the headers")]
    HeaderAddressOutOfRange { field: &'static str, address: GuestVa },
    /// A section count exceeds the configured parser limit.
    #[error("XBE section count {count} exceeds the limit {limit}")]
    SectionLimit { count: u32, limit: u32 },
    /// A section raw range exceeds the file.
    #[error("XBE section {section_index} raw range exceeds the file")]
    SectionRawRange { section_index: u32 },
    /// A section virtual range exceeds the guest address space.
    #[error("XBE section {section_index} virtual range exceeds the guest address space")]
    SectionVirtualRange { section_index: u32 },
    /// A section name is not valid UTF-8.
    #[error("XBE section {section_index} name is not valid UTF-8")]
    InvalidSectionName { section_index: u32 },
}
