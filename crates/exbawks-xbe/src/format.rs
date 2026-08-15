use bitflags::bitflags;
use exbawks_types::{BuildFlavor, GuestVa};
use serde::{Deserialize, Serialize};

bitflags! {
    /// XBE section flags.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct XbeSectionFlags: u32 {
        /// The section can be written.
        const WRITABLE = 0x0000_0001;
        /// The section requests preload behavior.
        const PRELOAD = 0x0000_0002;
        /// The section contains executable code.
        const EXECUTABLE = 0x0000_0004;
        /// The section represents an inserted file.
        const INSERTED_FILE = 0x0000_0008;
        /// The head page requests read-only behavior.
        const HEAD_PAGE_READ_ONLY = 0x0000_0010;
        /// The tail page requests read-only behavior.
        const TAIL_PAGE_READ_ONLY = 0x0000_0020;
    }
}

/// Parsed fields from the XBE image header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbeHeader {
    /// The guest image base.
    pub base_address: GuestVa,
    /// The file-backed header size.
    pub size_of_headers: u32,
    /// The guest virtual image size.
    pub size_of_image: u32,
    /// The declared image-header size.
    pub size_of_image_header: u32,
    /// The image creation timestamp.
    pub time_date_stamp: u32,
    /// The guest certificate address.
    pub certificate_address: GuestVa,
    /// The number of section headers.
    pub section_count: u32,
    /// The guest address of the section-header array.
    pub section_headers_address: GuestVa,
    /// Raw XBE initialization flags.
    pub initialization_flags: u32,
    /// The encoded entry point from the file.
    pub encoded_entry_point: u32,
    /// The decoded guest entry point.
    pub entry_point: GuestVa,
    /// The detected image flavor.
    pub build_flavor: BuildFlavor,
    /// The guest TLS directory address.
    pub tls_address: GuestVa,
    /// The default guest stack size.
    pub stack_size: u32,
    /// The PE heap reserve value.
    pub heap_reserve: u32,
    /// The PE heap commit value.
    pub heap_commit: u32,
    /// The encoded kernel thunk address.
    pub encoded_kernel_thunk_address: u32,
    /// The decoded kernel thunk table address.
    pub kernel_thunk_address: GuestVa,
}

/// One parsed XBE section header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbeSection {
    /// The section-table index.
    pub index: u32,
    /// The section name.
    pub name: String,
    /// Section behavior flags.
    pub flags: XbeSectionFlags,
    /// The guest virtual address.
    pub virtual_address: GuestVa,
    /// The virtual byte size.
    pub virtual_size: u32,
    /// The file offset of raw bytes.
    pub raw_address: u32,
    /// The raw byte size.
    pub raw_size: u32,
    /// The guest address of the section name.
    pub name_address: GuestVa,
    /// The section reference count.
    pub reference_count: u32,
    /// The section digest bytes.
    pub digest: [u8; 20],
}
