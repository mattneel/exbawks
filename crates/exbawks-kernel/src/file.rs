//! Nt* file exports (HLE-004).
//!
//! Titles open and read their assets through these exports. Each parses the
//! guest's `OBJECT_ATTRIBUTES` / `IO_STATUS_BLOCK` structures and delegates to
//! the host-backed file device (ADR 0014) through the memory-service seam;
//! the device owns the mount, the sandboxed path resolver, and the file
//! table. The device is read-only, so create/write dispositions are refused.

use exbawks_types::GuestVa;

use crate::startup::stack_argument;
use crate::{
    FileOpenRequest, KernelCallContext, KernelError, KernelExport, KernelRegistry,
    KernelServiceError, KernelStatus,
};

/// The longest guest object-name path the resolver accepts.
const MAX_PATH_BYTES: usize = 512;

/// `DesiredAccess` bits that request write or create-modifying access:
/// `FILE_WRITE_DATA`, `FILE_APPEND_DATA`, `GENERIC_WRITE`, `GENERIC_ALL`.
const WRITE_ACCESS_BITS: u32 = 0x0000_0002 | 0x0000_0004 | 0x4000_0000 | 0x1000_0000;

/// `CreateDisposition` values that overwrite existing content:
/// `FILE_SUPERSEDE`(0), `FILE_OVERWRITE`(4), `FILE_OVERWRITE_IF`(5).
fn disposition_writes(disposition: u32) -> bool {
    matches!(disposition, 0 | 4 | 5)
}

/// `CreateDisposition` values that create the object when it is missing:
/// `FILE_SUPERSEDE`(0), `FILE_CREATE`(2), `FILE_OPEN_IF`(3),
/// `FILE_OVERWRITE_IF`(5).
fn disposition_creates(disposition: u32) -> bool {
    matches!(disposition, 0 | 2 | 3 | 5)
}

/// The `FILE_DIRECTORY_FILE` create/open option: the object is a directory.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

/// `IO_STATUS_BLOCK.Information` results.
const FILE_OPENED: u32 = 1;
const FILE_CREATED: u32 = 2;

/// `FILE_STANDARD_INFORMATION` class ordinal.
const FILE_STANDARD_INFORMATION: u32 = 5;
/// `FILE_POSITION_INFORMATION` class ordinal.
const FILE_POSITION_INFORMATION: u32 = 14;
/// `FILE_NETWORK_OPEN_INFORMATION` class ordinal.
const FILE_NETWORK_OPEN_INFORMATION: u32 = 34;
/// `FILE_ATTRIBUTE_NORMAL` and `FILE_ATTRIBUTE_DIRECTORY`.
const ATTRIBUTE_NORMAL: u32 = 0x80;
const ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// Registers the Nt* file exports.
pub(crate) fn register_file_exports(registry: &KernelRegistry) -> Result<(), KernelError> {
    registry.register(NtOpenFile)?;
    registry.register(NtCreateFile)?;
    registry.register(NtReadFile)?;
    registry.register(NtQueryInformationFile)?;
    registry.register(NtSetInformationFile)?;
    registry.register(NtDeviceIoControlFile)?;
    registry.register(NtQueryVolumeInformationFile)?;
    registry.register(NtWriteFile)?;
    Ok(())
}

/// Reads the object-name path out of an `OBJECT_ATTRIBUTES` structure.
///
/// Xbox layout: `RootDirectory`@0x00, `ObjectName`@0x04 (a `PANSI_STRING`),
/// `Attributes`@0x08. The `ANSI_STRING` is `Length`(u16)@0x00,
/// `MaximumLength`(u16)@0x02, `Buffer`@0x04.
fn object_path(context: &KernelCallContext<'_>, attributes: u32) -> Option<String> {
    if attributes == 0 {
        return None;
    }
    let name = context.memory.read_u32(GuestVa(attributes + 0x04)).ok()?;
    if name == 0 {
        return None;
    }
    let length = (context.memory.read_u32(GuestVa(name)).ok()? & 0xFFFF) as usize;
    let buffer = context.memory.read_u32(GuestVa(name + 0x04)).ok()?;
    if buffer == 0 || length == 0 {
        return None;
    }
    let mut bytes = vec![0_u8; length.min(MAX_PATH_BYTES)];
    context.memory.read(GuestVa(buffer), &mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Writes an `IO_STATUS_BLOCK` (`Status`@0x00, `Information`@0x04).
fn write_io_status(
    context: &mut KernelCallContext<'_>,
    iosb: u32,
    status: KernelStatus,
    info: u32,
) {
    if iosb == 0 {
        return;
    }
    let _ = context.memory.write_u32(GuestVa(iosb), status.0);
    let _ = context.memory.write_u32(GuestVa(iosb + 0x04), info);
}

/// Maps a service error to the file-path NT status it reports.
fn open_error_status(error: KernelServiceError) -> KernelStatus {
    match error {
        KernelServiceError::AccessDenied => KernelStatus::ACCESS_DENIED,
        KernelServiceError::InvalidHandle => KernelStatus::INVALID_HANDLE,
        KernelServiceError::ResourceExhausted => KernelStatus::INSUFFICIENT_RESOURCES,
        // NotFound and an Unsupported device both read as "no such file".
        KernelServiceError::NotFound | KernelServiceError::Unsupported => {
            KernelStatus::OBJECT_NAME_NOT_FOUND
        }
    }
}

/// Opens a file object, shared by `NtOpenFile` and `NtCreateFile`.
fn open_file(
    context: &mut KernelCallContext<'_>,
    handle_out: u32,
    attributes: u32,
    iosb: u32,
    write_access: bool,
    create: bool,
    directory: bool,
) -> KernelStatus {
    if handle_out == 0 {
        return KernelStatus::INVALID_PARAMETER;
    }
    let Some(path) = object_path(context, attributes) else {
        write_io_status(context, iosb, KernelStatus::OBJECT_NAME_NOT_FOUND, 0);
        return KernelStatus::OBJECT_NAME_NOT_FOUND;
    };
    tracing::debug!(%path, write_access, create, directory, "NtOpenFile/NtCreateFile");

    match context.services.open_file(FileOpenRequest { path, write_access, create, directory }) {
        Ok(opened) => {
            let _ = context.memory.write_u32(GuestVa(handle_out), opened.handle);
            let information = if opened.created { FILE_CREATED } else { FILE_OPENED };
            write_io_status(context, iosb, KernelStatus::SUCCESS, information);
            KernelStatus::SUCCESS
        }
        Err(error) => {
            let status = open_error_status(error);
            write_io_status(context, iosb, status, 0);
            status
        }
    }
}

/// Opens an existing file.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtOpenFile;

impl KernelExport for NtOpenFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_OPEN_FILE
    }

    fn name(&self) -> &'static str {
        "NtOpenFile"
    }

    fn stack_bytes(&self) -> u16 {
        24
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtOpenFile(FileHandle, DesiredAccess, ObjectAttributes,
        //            IoStatusBlock, ShareAccess, OpenOptions).
        let (Some(handle_out), Some(access), Some(attributes), Some(iosb)) = (
            stack_argument(context, 0),
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let options = stack_argument(context, 5).unwrap_or(0);
        let write_access = access & WRITE_ACCESS_BITS != 0;
        let directory = options & FILE_DIRECTORY_FILE != 0;
        open_file(context, handle_out, attributes, iosb, write_access, false, directory)
    }
}

/// Creates or opens a file. Creation and writing succeed only on the
/// writable hard-disk mount (ADR 0016); the disc stays read-only.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtCreateFile;

impl KernelExport for NtCreateFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_CREATE_FILE
    }

    fn name(&self) -> &'static str {
        "NtCreateFile"
    }

    fn stack_bytes(&self) -> u16 {
        36
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtCreateFile(FileHandle, DesiredAccess, ObjectAttributes,
        //   IoStatusBlock, AllocationSize, FileAttributes, ShareAccess,
        //   CreateDisposition, CreateOptions).
        let (Some(handle_out), Some(access), Some(attributes), Some(iosb)) = (
            stack_argument(context, 0),
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let disposition = stack_argument(context, 7).unwrap_or(1);
        let options = stack_argument(context, 8).unwrap_or(0);
        let write_access = access & WRITE_ACCESS_BITS != 0 || disposition_writes(disposition);
        let create = disposition_creates(disposition);
        let directory = options & FILE_DIRECTORY_FILE != 0;
        open_file(context, handle_out, attributes, iosb, write_access, create, directory)
    }
}

/// Reads bytes from an open file into a guest buffer.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtReadFile;

impl KernelExport for NtReadFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_READ_FILE
    }

    fn name(&self) -> &'static str {
        "NtReadFile"
    }

    fn stack_bytes(&self) -> u16 {
        32
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtReadFile(FileHandle, Event, ApcRoutine, ApcContext, IoStatusBlock,
        //            Buffer, Length, ByteOffset).
        let (Some(handle), Some(iosb), Some(buffer), Some(length)) = (
            stack_argument(context, 0),
            stack_argument(context, 4),
            stack_argument(context, 5),
            stack_argument(context, 6),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        // ByteOffset is an optional PLARGE_INTEGER; a null pointer or the
        // sentinel FILE_USE_FILE_POINTER_POSITION reads at the file pointer.
        let offset = match stack_argument(context, 7).unwrap_or(0) {
            0 => None,
            pointer => {
                let low = context.memory.read_u32(GuestVa(pointer)).unwrap_or(0);
                let high = context.memory.read_u32(GuestVa(pointer + 4)).unwrap_or(0);
                let value = (u64::from(high) << 32) | u64::from(low);
                // 0xFFFFFFFF`FFFFFFFE means "current position".
                if value == 0xFFFF_FFFF_FFFF_FFFE { None } else { Some(value) }
            }
        };

        match context.services.read_file(handle, offset, length) {
            Ok(bytes) => {
                if buffer != 0 && !bytes.is_empty() {
                    let _ = context.memory.write(GuestVa(buffer), &bytes);
                }
                let read = bytes.len() as u32;
                // A read that returns nothing at a nonzero request is EOF.
                let status = if read == 0 && length != 0 {
                    KernelStatus::END_OF_FILE
                } else {
                    KernelStatus::SUCCESS
                };
                write_io_status(context, iosb, status, read);
                status
            }
            Err(error) => {
                let status = open_error_status(error);
                write_io_status(context, iosb, status, 0);
                status
            }
        }
    }
}

/// Writes bytes from a guest buffer to an open file (ADR 0016).
#[derive(Debug, Default, Clone, Copy)]
pub struct NtWriteFile;

impl KernelExport for NtWriteFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_WRITE_FILE
    }

    fn name(&self) -> &'static str {
        "NtWriteFile"
    }

    fn stack_bytes(&self) -> u16 {
        32
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtWriteFile(FileHandle, Event, ApcRoutine, ApcContext,
        //             IoStatusBlock, Buffer, Length, ByteOffset).
        let (Some(handle), Some(iosb), Some(buffer), Some(length)) = (
            stack_argument(context, 0),
            stack_argument(context, 4),
            stack_argument(context, 5),
            stack_argument(context, 6),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        let offset = match stack_argument(context, 7).unwrap_or(0) {
            0 => None,
            pointer => {
                let low = context.memory.read_u32(GuestVa(pointer)).unwrap_or(0);
                let high = context.memory.read_u32(GuestVa(pointer + 4)).unwrap_or(0);
                let value = (u64::from(high) << 32) | u64::from(low);
                if value == 0xFFFF_FFFF_FFFF_FFFE { None } else { Some(value) }
            }
        };

        // Bounded copy out of guest memory before the host write.
        let want = (length as usize).min(16 * 1024 * 1024);
        let mut bytes = vec![0_u8; want];
        if want > 0 && context.memory.read(GuestVa(buffer), &mut bytes).is_err() {
            write_io_status(context, iosb, KernelStatus::ACCESS_VIOLATION, 0);
            return KernelStatus::ACCESS_VIOLATION;
        }

        match context.services.write_file(handle, offset, &bytes) {
            Ok(written) => {
                write_io_status(context, iosb, KernelStatus::SUCCESS, written);
                KernelStatus::SUCCESS
            }
            Err(error) => {
                let status = open_error_status(error);
                write_io_status(context, iosb, status, 0);
                status
            }
        }
    }
}

/// Applies position and length changes to an open file.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtSetInformationFile;

impl KernelExport for NtSetInformationFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_SET_INFORMATION_FILE
    }

    fn name(&self) -> &'static str {
        "NtSetInformationFile"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtSetInformationFile(FileHandle, IoStatusBlock, FileInformation,
        //                      Length, FileInformationClass).
        let (Some(handle), Some(iosb), Some(info_in), Some(length), Some(class)) = (
            stack_argument(context, 0),
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
            stack_argument(context, 4),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        tracing::debug!(class, length, "NtSetInformationFile");
        if length < 8 {
            return buffer_too_small(context, iosb);
        }
        let low = context.memory.read_u32(GuestVa(info_in)).unwrap_or(0);
        let high = context.memory.read_u32(GuestVa(info_in.wrapping_add(4))).unwrap_or(0);
        let value = u64::from(low) | (u64::from(high) << 32);

        let outcome = match class {
            // FilePositionInformation: CurrentByteOffset.
            FILE_POSITION_INFORMATION => context.services.set_file_position(handle, value),
            // (19 = FileAllocationInformation, 20 = FileEndOfFileInformation)
            // FileAllocationInformation / FileEndOfFileInformation both set
            // the on-disk size on the host filesystem.
            19 | 20 => context.services.set_file_length(handle, value),
            _ => return buffer_too_small(context, iosb),
        };
        match outcome {
            Ok(()) => {
                write_io_status(context, iosb, KernelStatus::SUCCESS, 0);
                KernelStatus::SUCCESS
            }
            Err(KernelServiceError::InvalidHandle) => KernelStatus::INVALID_HANDLE,
            Err(_) => KernelStatus::ACCESS_DENIED,
        }
    }
}

/// Answers size and position queries about an open file.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtQueryInformationFile;

impl KernelExport for NtQueryInformationFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_QUERY_INFORMATION_FILE
    }

    fn name(&self) -> &'static str {
        "NtQueryInformationFile"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtQueryInformationFile(FileHandle, IoStatusBlock, FileInformation,
        //                        Length, FileInformationClass).
        let (Some(handle), Some(iosb), Some(info_out), Some(length), Some(class)) = (
            stack_argument(context, 0),
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
            stack_argument(context, 4),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        tracing::debug!(class, length, "NtQueryInformationFile");

        let info = match context.services.file_info(handle) {
            Ok(info) => info,
            Err(error) => {
                let status = open_error_status(error);
                write_io_status(context, iosb, status, 0);
                return status;
            }
        };

        let written = match class {
            FILE_STANDARD_INFORMATION => {
                // AllocationSize(8), EndOfFile(8), NumberOfLinks(4),
                // DeletePending(1), Directory(1).
                if length < 24 {
                    return buffer_too_small(context, iosb);
                }
                write_u64(context, info_out, info.size); // AllocationSize
                write_u64(context, info_out + 8, info.size); // EndOfFile
                let _ = context.memory.write_u32(GuestVa(info_out + 16), 1); // NumberOfLinks
                let _ = context.memory.write_u32(GuestVa(info_out + 20), 0); // flags
                24
            }
            FILE_POSITION_INFORMATION => {
                if length < 8 {
                    return buffer_too_small(context, iosb);
                }
                write_u64(context, info_out, info.position);
                8
            }
            FILE_NETWORK_OPEN_INFORMATION => {
                // CreationTime, LastAccessTime, LastWriteTime, ChangeTime
                // (8 each, zero — the virtual clock is later work),
                // AllocationSize(8), EndOfFile(8), FileAttributes(4), pad(4).
                if length < 56 {
                    return buffer_too_small(context, iosb);
                }
                for offset in [0_u32, 8, 16, 24] {
                    write_u64(context, info_out + offset, 0);
                }
                write_u64(context, info_out + 32, info.size); // AllocationSize
                write_u64(context, info_out + 40, info.size); // EndOfFile
                let attributes =
                    if info.directory { ATTRIBUTE_DIRECTORY } else { ATTRIBUTE_NORMAL };
                let _ = context.memory.write_u32(GuestVa(info_out + 48), attributes);
                let _ = context.memory.write_u32(GuestVa(info_out + 52), 0);
                56
            }
            _ => return buffer_too_small(context, iosb),
        };

        write_io_status(context, iosb, KernelStatus::SUCCESS, written);
        KernelStatus::SUCCESS
    }
}

/// Issues a device I/O control request.
///
/// There is no real disc device yet, so this reports success with a zeroed
/// output buffer: titles issue disc IOCTLs (media detection, drive geometry)
/// during startup and a benign success lets them proceed. The specific
/// control codes get real answers as walls demand them.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtDeviceIoControlFile;

impl KernelExport for NtDeviceIoControlFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_DEVICE_IO_CONTROL_FILE
    }

    fn name(&self) -> &'static str {
        "NtDeviceIoControlFile"
    }

    fn stack_bytes(&self) -> u16 {
        40
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtDeviceIoControlFile(FileHandle, Event, ApcRoutine, ApcContext,
        //   IoStatusBlock, IoControlCode, InputBuffer, InputBufferLength,
        //   OutputBuffer, OutputBufferLength).
        let iosb = stack_argument(context, 4).unwrap_or(0);
        let control_code = stack_argument(context, 5).unwrap_or(0);
        let output = stack_argument(context, 8).unwrap_or(0);
        let output_len = stack_argument(context, 9).unwrap_or(0);
        tracing::debug!(
            control_code = format_args!("{control_code:#010x}"),
            output_len,
            "NtDeviceIoControlFile"
        );

        // Zero the output buffer so the caller reads a defined (empty) result.
        if output != 0 {
            let zeros = vec![0_u8; (output_len as usize).min(4096)];
            let _ = context.memory.write(GuestVa(output), &zeros);
        }
        write_io_status(context, iosb, KernelStatus::SUCCESS, output_len);
        KernelStatus::SUCCESS
    }
}

/// `FileFsSizeInformation` class ordinal.
const FILE_FS_SIZE_INFORMATION: u32 = 3;
/// `FileFsDeviceInformation` class ordinal.
const FILE_FS_DEVICE_INFORMATION: u32 = 4;

/// The emulated volume geometry is FATX (the Xbox hard disk): 512-byte
/// sectors, 32 sectors per allocation unit — 16 KiB clusters. Titles convert
/// clusters to 16 KiB save-game "blocks" as `SectorsPerAllocationUnit *
/// BytesPerSector / 16384`, which must not truncate to zero: a smaller
/// cluster (say a DVD's 2 KiB sector at one per unit) reads as zero blocks
/// per cluster, so every free-space check sees an empty disk and the title
/// reboots to the dashboard for a "hard disk cleanup".
const VOLUME_BYTES_PER_SECTOR: u32 = 512;
/// Sectors per allocation unit (16 KiB clusters).
const VOLUME_SECTORS_PER_UNIT: u32 = 32;
/// Total clusters: ~4.9 GB at 16 KiB per cluster.
const VOLUME_TOTAL_UNITS: u64 = 0x0004_B000;
/// Free clusters: 1 GiB free, comfortably positive in any signed 32-bit
/// byte count a title might compute.
const VOLUME_FREE_UNITS: u64 = 0x0001_0000;
/// `FILE_DEVICE_CD_ROM`.
const FILE_DEVICE_CD_ROM: u32 = 0x0000_0002;
/// `FILE_REMOVABLE_MEDIA | FILE_READ_ONLY_DEVICE`.
const DISC_CHARACTERISTICS: u32 = 0x0000_0001 | 0x0000_0002;

/// Answers volume metadata queries.
///
/// Size queries report FATX hard-disk geometry (16 KiB clusters) with free
/// space, so save-space checks pass; device queries report a CD-ROM. Unknown
/// classes get a zeroed, successful answer so a probe proceeds.
#[derive(Debug, Default, Clone, Copy)]
pub struct NtQueryVolumeInformationFile;

impl KernelExport for NtQueryVolumeInformationFile {
    fn ordinal(&self) -> u16 {
        crate::ordinal::NT_QUERY_VOLUME_INFORMATION_FILE
    }

    fn name(&self) -> &'static str {
        "NtQueryVolumeInformationFile"
    }

    fn stack_bytes(&self) -> u16 {
        20
    }

    fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
        // NtQueryVolumeInformationFile(FileHandle, IoStatusBlock,
        //   FsInformation, Length, FsInformationClass).
        let (Some(iosb), Some(info_out), Some(length), Some(class)) = (
            stack_argument(context, 1),
            stack_argument(context, 2),
            stack_argument(context, 3),
            stack_argument(context, 4),
        ) else {
            return KernelStatus::INVALID_PARAMETER;
        };
        tracing::debug!(class, length, "NtQueryVolumeInformationFile");

        let written = match class {
            FILE_FS_SIZE_INFORMATION => {
                // TotalAllocationUnits(8), AvailableAllocationUnits(8),
                // SectorsPerAllocationUnit(4), BytesPerSector(4).
                if length < 24 {
                    return buffer_too_small(context, iosb);
                }
                write_u64(context, info_out, VOLUME_TOTAL_UNITS);
                write_u64(context, info_out + 8, VOLUME_FREE_UNITS); // free space
                let _ = context.memory.write_u32(GuestVa(info_out + 16), VOLUME_SECTORS_PER_UNIT);
                let _ = context.memory.write_u32(GuestVa(info_out + 20), VOLUME_BYTES_PER_SECTOR);
                24
            }
            FILE_FS_DEVICE_INFORMATION => {
                // DeviceType(4), Characteristics(4).
                if length < 8 {
                    return buffer_too_small(context, iosb);
                }
                let _ = context.memory.write_u32(GuestVa(info_out), FILE_DEVICE_CD_ROM);
                let _ = context.memory.write_u32(GuestVa(info_out + 4), DISC_CHARACTERISTICS);
                8
            }
            _ => {
                // A defined, empty answer for classes not modeled yet.
                if info_out != 0 {
                    let zeros = vec![0_u8; (length as usize).min(256)];
                    let _ = context.memory.write(GuestVa(info_out), &zeros);
                }
                length.min(256)
            }
        };

        write_io_status(context, iosb, KernelStatus::SUCCESS, written);
        KernelStatus::SUCCESS
    }
}

/// Writes a little-endian u64 to guest memory, best effort.
fn write_u64(context: &mut KernelCallContext<'_>, address: u32, value: u64) {
    let _ = context.memory.write_u32(GuestVa(address), value as u32);
    let _ = context.memory.write_u32(GuestVa(address + 4), (value >> 32) as u32);
}

/// Records a buffer-too-small failure in the IO status block and returns it.
fn buffer_too_small(context: &mut KernelCallContext<'_>, iosb: u32) -> KernelStatus {
    write_io_status(context, iosb, KernelStatus::BUFFER_TOO_SMALL, 0);
    KernelStatus::BUFFER_TOO_SMALL
}

#[cfg(test)]
mod tests {
    use exbawks_cpu::CpuState;
    use exbawks_memory::{GuestMemory, SoftwareAddressSpace};
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use crate::services::{FileInfo, FileOpened};
    use crate::{KernelServiceError, KernelServices};

    use super::*;

    /// A fake file service backing the export unit tests.
    #[derive(Default)]
    struct FakeFiles {
        opened_path: Option<String>,
        opened_write: bool,
        size: u64,
    }

    impl KernelServices for FakeFiles {
        fn create_thread(
            &mut self,
            _request: crate::ThreadCreateRequest,
        ) -> Result<crate::ThreadCreated, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn exit_current_thread(&mut self, _status: u32) {}

        fn close_handle(&mut self, _handle: u32) -> bool {
            false
        }

        fn allocate_virtual_memory(
            &mut self,
            _request: crate::VirtualAllocRequest,
        ) -> Result<crate::VirtualAllocation, KernelServiceError> {
            Err(KernelServiceError::Unsupported)
        }

        fn open_file(
            &mut self,
            request: FileOpenRequest,
        ) -> Result<FileOpened, KernelServiceError> {
            if request.write_access {
                return Err(KernelServiceError::AccessDenied);
            }
            self.opened_write = request.write_access;
            self.opened_path = Some(request.path);
            Ok(FileOpened { handle: 0x0104, created: false })
        }

        fn read_file(
            &mut self,
            _handle: u32,
            _offset: Option<u64>,
            len: u32,
        ) -> Result<Vec<u8>, KernelServiceError> {
            Ok(vec![0xAB; (len as usize).min(4)])
        }

        fn file_info(&mut self, _handle: u32) -> Result<FileInfo, KernelServiceError> {
            Ok(FileInfo { size: self.size, position: 0, directory: false })
        }
    }

    /// Maps a scratch space and lays out an `OBJECT_ATTRIBUTES` naming `path`.
    fn memory_with_object_name(path: &str) -> SoftwareAddressSpace {
        let memory = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), 8 * u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        memory
            .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        // OBJECT_ATTRIBUTES at 0x4000, ANSI_STRING at 0x4100, buffer at 0x4200.
        memory.write_u32(GuestVa(0x4000), 0).expect("write"); // RootDirectory
        memory.write_u32(GuestVa(0x4004), 0x4100).expect("write"); // ObjectName
        memory.write_u32(GuestVa(0x4008), 0x40).expect("write"); // Attributes
        let bytes = path.as_bytes();
        let packed = (bytes.len() as u32) | ((bytes.len() as u32) << 16);
        memory.write_u32(GuestVa(0x4100), packed).expect("write"); // Length/MaximumLength
        memory.write_u32(GuestVa(0x4104), 0x4200).expect("write"); // Buffer
        memory.write(GuestVa(0x4200), bytes).expect("write");
        memory
    }

    fn run_open(
        access: u32,
        path: &str,
        services: &mut dyn KernelServices,
        memory: &SoftwareAddressSpace,
    ) -> KernelStatus {
        // Stack: [esp]=return, then FileHandle, DesiredAccess, OA, IOSB,
        // ShareAccess, OpenOptions.
        let args = [0x5000_u32, access, 0x4000, 0x5010, 0, 0];
        for (slot, value) in args.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let _ = path;
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext { cpu: &mut cpu, memory, services, stop_request: None };
        NtOpenFile.call(&mut context)
    }

    #[test]
    fn open_passes_the_object_path_to_the_service() {
        let memory = memory_with_object_name("\\Device\\CdRom0\\media\\title.xbx");
        let mut files = FakeFiles::default();
        let status = run_open(0x0001 /* FILE_READ_DATA */, "", &mut files, &memory);
        assert_eq!(status, KernelStatus::SUCCESS);
        assert_eq!(files.opened_path.as_deref(), Some("\\Device\\CdRom0\\media\\title.xbx"));
        assert_eq!(
            memory.read_u32(GuestVa(0x5000)).unwrap(),
            0x0104,
            "the handle reached the guest"
        );
        // IO_STATUS_BLOCK.Information == FILE_OPENED.
        assert_eq!(memory.read_u32(GuestVa(0x5014)).unwrap(), FILE_OPENED);
    }

    #[test]
    fn a_write_open_is_denied_on_the_read_only_device() {
        let memory = memory_with_object_name("\\Device\\CdRom0\\save.dat");
        let mut files = FakeFiles::default();
        let status = run_open(0x4000_0000 /* GENERIC_WRITE */, "", &mut files, &memory);
        assert_eq!(status, KernelStatus::ACCESS_DENIED);
    }

    #[test]
    fn volume_size_information_reports_disc_geometry() {
        let memory = memory_with_object_name("x");
        let mut files = FakeFiles::default();
        // NtQueryVolumeInformationFile(FileHandle, IOSB, FsInformation,
        // Length, FsInformationClass = FileFsSizeInformation).
        let args = [0x0104_u32, 0x5010, 0x5100, 24, FILE_FS_SIZE_INFORMATION];
        for (slot, value) in args.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut files,
            stop_request: None,
        };
        assert_eq!(NtQueryVolumeInformationFile.call(&mut context), KernelStatus::SUCCESS);
        // SectorsPerAllocationUnit and BytesPerSector close the struct; the
        // cluster must be at least one 16 KiB save block, or a title's
        // clusters-to-blocks conversion truncates to zero and it reboots to
        // the dashboard for a disk cleanup.
        let sectors = memory.read_u32(GuestVa(0x5100 + 16)).unwrap();
        let bytes = memory.read_u32(GuestVa(0x5100 + 20)).unwrap();
        assert!(sectors * bytes >= 16384, "one cluster holds at least one save block");
        // Free space is nonzero so a save-space check passes.
        assert_ne!(memory.read_u32(GuestVa(0x5100 + 8)).unwrap(), 0);
    }

    #[test]
    fn device_io_control_succeeds_benignly() {
        let memory = memory_with_object_name("x");
        let mut files = FakeFiles::default();
        // Ten arguments; a media-probe IOCTL with no output buffer.
        let args = [0x0104_u32, 0, 0, 0, 0x5010, 0x0004_d014, 0, 0, 0, 0];
        for (slot, value) in args.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut files,
            stop_request: None,
        };
        assert_eq!(NtDeviceIoControlFile.call(&mut context), KernelStatus::SUCCESS);
        // IO_STATUS_BLOCK.Status is success.
        assert_eq!(memory.read_u32(GuestVa(0x5010)).unwrap(), KernelStatus::SUCCESS.0);
    }

    #[test]
    fn query_standard_information_reports_the_size() {
        let memory = memory_with_object_name("x");
        let mut files = FakeFiles { size: 0x1234, ..FakeFiles::default() };
        // Stack for NtQueryInformationFile: FileHandle, IOSB, FileInformation,
        // Length, FileInformationClass.
        let args = [0x0104_u32, 0x5010, 0x5100, 24, FILE_STANDARD_INFORMATION];
        for (slot, value) in args.iter().enumerate() {
            memory.write_u32(GuestVa(0x2004 + slot as u32 * 4), *value).expect("write");
        }
        let mut cpu = CpuState::default();
        cpu.gpr[4] = 0x2000;
        let mut context = KernelCallContext {
            cpu: &mut cpu,
            memory: &memory,
            services: &mut files,
            stop_request: None,
        };
        assert_eq!(NtQueryInformationFile.call(&mut context), KernelStatus::SUCCESS);
        // EndOfFile is at offset 8 of FILE_STANDARD_INFORMATION.
        assert_eq!(memory.read_u32(GuestVa(0x5108)).unwrap(), 0x1234);
    }
}
