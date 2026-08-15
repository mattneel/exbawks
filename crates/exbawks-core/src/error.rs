use exbawks_cpu::BlockDecodeError;
use exbawks_jit::JitError;
use exbawks_memory::MemoryError;
use exbawks_types::GuestVa;
use exbawks_xbe::XbeError;
use thiserror::Error;

/// An emulator composition or boot-planning failure.
#[derive(Debug, Error)]
pub enum CoreError {
    /// XBE parsing failed.
    #[error(transparent)]
    Xbe(#[from] XbeError),
    /// Guest memory setup or access failed.
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// Guest instruction decoding failed.
    #[error(transparent)]
    Decode(#[from] BlockDecodeError),
    /// Translation planning or emission failed.
    #[error(transparent)]
    Jit(#[from] JitError),
    /// The configuration contains an invalid value.
    #[error("invalid emulator configuration: {0}")]
    InvalidConfiguration(&'static str),
    /// An image is already active.
    #[error("an XBE image is already loaded")]
    ImageAlreadyLoaded,
    /// An operation requires an active image.
    #[error("no XBE image is loaded")]
    NoImageLoaded,
    /// A section contains more raw bytes than virtual bytes.
    #[error(
        "XBE section {section_index} has raw size {raw_size} \
         larger than virtual size {virtual_size}"
    )]
    SectionRawExceedsVirtual { section_index: u32, raw_size: u32, virtual_size: u32 },
    /// A section start does not use 4 KiB alignment.
    ///
    /// Retail sections are byte-contiguous rather than page-aligned, so the
    /// loader no longer produces this; it remains for external callers.
    #[error("XBE section {section_index} starts at unaligned guest address {address}")]
    UnalignedSection { section_index: u32, address: GuestVa },
    /// Two section byte ranges overlap beyond a shared page boundary.
    #[error("XBE section {section_index} byte range at {address} overlaps another section")]
    SectionByteOverlap { section_index: u32, address: GuestVa },
    /// A kernel thunk address arithmetic operation overflowed.
    #[error("kernel thunk table address overflow at {address}")]
    KernelThunkAddressOverflow { address: GuestVa },
    /// A kernel thunk entry does not use the ordinal form.
    #[error("invalid kernel thunk value 0x{value:08X} at {address}")]
    InvalidKernelThunk { address: GuestVa, value: u32 },
    /// A kernel thunk table did not terminate within the configured limit.
    #[error("kernel thunk table exceeded the configured limit of {limit} entries")]
    KernelThunkLimit { limit: usize },
}
