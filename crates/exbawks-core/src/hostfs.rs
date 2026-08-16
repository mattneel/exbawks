//! The host-backed file device (ADR 0014).
//!
//! Guest `Nt*File` exports resolve NT object paths against a single mounted
//! host directory — the read-only game disc. Path resolution is confined to
//! the mount root: the component-depth check in [`sandbox_components`] is the
//! security boundary, and a canonicalized containment re-check backs it up.
//! No proprietary data lives in the repository; the mount root is supplied at
//! runtime and every test uses synthetic files.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use exbawks_kernel::{FileInfo, FileOpenRequest, FileOpened, KernelServiceError};

/// The first guest handle the file table hands out. Disjoint from the thread
/// handle range (`0x0000_E000`+), so one `close_handle` serves both.
const FILE_HANDLE_BASE: u32 = 0x0000_0100;
/// The spacing between successive file handles.
const FILE_HANDLE_STEP: u32 = 4;
/// The largest single read the device serves, one guest RAM's worth. A larger
/// request is a guest bug, not a real read.
const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

/// The NT device and drive prefixes that name the game disc.
const DISC_PREFIXES: [&str; 5] =
    ["\\Device\\CdRom0", "\\Device\\Harddisk0\\Partition1", "\\??\\D:", "\\??\\CdRom0:", "D:"];

/// One open guest file object.
///
/// A `None` handle names a directory or device object (the disc root, a
/// partition): titles open these to probe that the disc and HDD exist. Reads
/// on a directory return nothing and its size is zero, which is enough for a
/// presence check to pass.
struct OpenFile {
    file: Option<File>,
    position: u64,
}

/// A host-backed file device with one read-only disc mount.
pub(crate) struct HostFileSystem {
    /// The game-disc mount root, or `None` when no disc is mounted.
    disc_root: Option<PathBuf>,
    files: HashMap<u32, OpenFile>,
    next_handle: u32,
}

impl HostFileSystem {
    /// Creates a device with an optional read-only disc mount.
    pub(crate) fn new(disc_root: Option<PathBuf>) -> Self {
        Self { disc_root, files: HashMap::new(), next_handle: FILE_HANDLE_BASE }
    }

    /// Splits a device-qualified NT path into its mount root and remainder.
    fn mount_for(&self, path: &str) -> Option<(&Path, String)> {
        let root = self.disc_root.as_deref()?;
        for prefix in DISC_PREFIXES {
            if let Some(rest) = strip_prefix_ci(path, prefix) {
                // The remainder must be empty or begin at a separator, so a
                // device name is never a prefix of a longer name.
                if rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/') {
                    return Some((root, rest.to_owned()));
                }
            }
        }
        None
    }

    /// Resolves a guest NT path to a host path inside the mount root.
    ///
    /// Returns `None` when the device is unknown, the path escapes the mount,
    /// or the resolved file does not exist.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let (root, remainder) = self.mount_for(path)?;
        let components = sandbox_components(&remainder)?;

        let mut resolved = root.to_path_buf();
        for component in &components {
            resolved.push(component);
        }

        // Belt and suspenders: canonicalize (resolving any symlinks) and
        // confirm the result is still contained by the canonical mount root.
        let canonical_root = root.canonicalize().ok()?;
        let canonical = resolved.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        Some(canonical)
    }

    /// Opens a file, honoring the read-only mount contract.
    pub(crate) fn open(
        &mut self,
        request: &FileOpenRequest,
    ) -> Result<FileOpened, KernelServiceError> {
        if request.write_access {
            // The disc is read-only until the writable mounts land (ADR 0014).
            return Err(KernelServiceError::AccessDenied);
        }
        let resolved = self.resolve(&request.path).ok_or(KernelServiceError::NotFound)?;
        // A regular file opens for reading; a directory or device object
        // (the disc root, a partition) opens as a marker so presence checks
        // pass. Anything else does not exist.
        let file = if resolved.is_file() {
            Some(File::open(&resolved).map_err(|_| KernelServiceError::NotFound)?)
        } else if resolved.is_dir() {
            None
        } else {
            return Err(KernelServiceError::NotFound);
        };

        let is_directory = file.is_none();
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(FILE_HANDLE_STEP);
        self.files.insert(handle, OpenFile { file, position: 0 });
        tracing::debug!(path = %request.path, handle, is_directory, "opened guest object");
        Ok(FileOpened { handle, created: false })
    }

    /// Reads up to `len` bytes at an explicit offset or the file pointer.
    pub(crate) fn read(
        &mut self,
        handle: u32,
        offset: Option<u64>,
        len: u32,
    ) -> Result<Vec<u8>, KernelServiceError> {
        let entry = self.files.get_mut(&handle).ok_or(KernelServiceError::InvalidHandle)?;
        // A directory or device object has no byte stream to read.
        let Some(file) = entry.file.as_mut() else {
            return Ok(Vec::new());
        };
        let start = offset.unwrap_or(entry.position);
        file.seek(SeekFrom::Start(start)).map_err(|_| KernelServiceError::AccessDenied)?;

        let want = (len as usize).min(MAX_READ_BYTES);
        let mut buffer = vec![0_u8; want];
        let mut filled = 0;
        while filled < want {
            match file.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(KernelServiceError::AccessDenied),
            }
        }
        buffer.truncate(filled);
        entry.position = start.saturating_add(filled as u64);
        Ok(buffer)
    }

    /// Returns the size and file-pointer position of one open file.
    pub(crate) fn info(&self, handle: u32) -> Result<FileInfo, KernelServiceError> {
        let entry = self.files.get(&handle).ok_or(KernelServiceError::InvalidHandle)?;
        // A directory or device object reports a zero size.
        let size = match entry.file.as_ref() {
            Some(file) => file.metadata().map_err(|_| KernelServiceError::AccessDenied)?.len(),
            None => 0,
        };
        Ok(FileInfo { size, position: entry.position })
    }

    /// Closes one file handle, returning whether it named an open file.
    pub(crate) fn close(&mut self, handle: u32) -> bool {
        self.files.remove(&handle).is_some()
    }
}

/// Strips a case-insensitive prefix, returning the remainder.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// Reduces an NT path remainder to safe path components, or `None` if it
/// escapes the mount root.
///
/// This is the sandbox boundary: `..` pops a component and can never ascend
/// above the root (a pop on an empty stack is an escape), separators collapse,
/// `.` is ignored, and an embedded NUL is rejected.
fn sandbox_components(remainder: &str) -> Option<Vec<String>> {
    let mut components: Vec<String> = Vec::new();
    for raw in remainder.split(['\\', '/']) {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw == ".." {
            // Pop the parent; a pop on an empty stack escapes the mount root.
            components.pop()?;
            continue;
        }
        if raw.contains('\0') {
            return None;
        }
        components.push(raw.to_owned());
    }
    Some(components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_collapses_separators_and_dots() {
        assert_eq!(
            sandbox_components("\\media//title\\.\\data.bin"),
            Some(vec!["media".to_owned(), "title".to_owned(), "data.bin".to_owned()])
        );
    }

    #[test]
    fn sandbox_allows_descending_then_returning() {
        assert_eq!(sandbox_components("\\a\\b\\..\\c"), Some(vec!["a".to_owned(), "c".to_owned()]));
    }

    #[test]
    fn sandbox_rejects_escapes_and_nul() {
        assert_eq!(sandbox_components("\\..\\secret"), None, "ascending above the root escapes");
        assert_eq!(sandbox_components("\\a\\..\\..\\b"), None, "net-negative depth escapes");
        assert_eq!(sandbox_components("\\a\0b"), None, "an embedded NUL is rejected");
    }

    #[test]
    fn unmounted_device_resolves_to_nothing() {
        let fs = HostFileSystem::new(None);
        assert!(fs.resolve("\\Device\\CdRom0\\anything").is_none());
    }

    #[test]
    fn a_foreign_device_is_unmounted() {
        let fs = HostFileSystem::new(Some(PathBuf::from(".")));
        // The device name is not one of the disc prefixes.
        assert!(fs.mount_for("\\Device\\Harddisk0\\Partition2\\x").is_none());
        // A device name must end at a separator, never mid-name.
        assert!(fs.mount_for("D:extra\\x").is_none());
    }

    /// A self-cleaning temporary directory for the filesystem-backed tests.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("exbawks-hostfs-{tag}-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opens_read_and_close_a_real_file() {
        let scratch = ScratchDir::new("file");
        std::fs::write(scratch.0.join("data.bin"), b"HELLO-DISC").expect("write file");
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()));

        let opened = fs
            .open(&FileOpenRequest {
                path: "\\Device\\CdRom0\\data.bin".to_owned(),
                write_access: false,
            })
            .expect("open succeeds");
        assert_eq!(fs.info(opened.handle).unwrap().size, 10, "the file size is reported");
        assert_eq!(fs.read(opened.handle, Some(0), 5).unwrap(), b"HELLO");
        // The file pointer advanced past the explicit read's end.
        assert_eq!(fs.read(opened.handle, None, 5).unwrap(), b"-DISC");
        assert!(fs.close(opened.handle), "the handle closes");
        assert!(!fs.close(opened.handle), "a closed handle is unknown");
    }

    #[test]
    fn opens_a_device_directory_as_a_zero_size_object() {
        let scratch = ScratchDir::new("dir");
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()));
        // Opening the bare device (the mount root, a directory) succeeds so a
        // disc/HDD presence check passes.
        let opened = fs
            .open(&FileOpenRequest { path: "\\Device\\CdRom0".to_owned(), write_access: false })
            .expect("device open succeeds");
        assert_eq!(fs.info(opened.handle).unwrap().size, 0, "a directory has no size");
        assert!(fs.read(opened.handle, None, 16).unwrap().is_empty(), "a directory reads empty");
    }

    #[test]
    fn a_write_open_and_a_missing_file_are_refused() {
        let scratch = ScratchDir::new("deny");
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()));
        assert_eq!(
            fs.open(&FileOpenRequest {
                path: "\\Device\\CdRom0\\x".to_owned(),
                write_access: true
            }),
            Err(KernelServiceError::AccessDenied),
            "the disc is read-only"
        );
        assert_eq!(
            fs.open(&FileOpenRequest {
                path: "\\Device\\CdRom0\\nope.bin".to_owned(),
                write_access: false
            }),
            Err(KernelServiceError::NotFound),
            "a missing file is not found"
        );
    }

    #[test]
    fn an_escape_attempt_is_confined() {
        let scratch = ScratchDir::new("escape");
        // A secret one level above the mount root.
        std::fs::write(scratch.0.join("outside.txt"), b"secret").expect("write");
        let inside = scratch.0.join("disc");
        std::fs::create_dir_all(&inside).expect("create mount");
        let mut fs = HostFileSystem::new(Some(inside));
        assert_eq!(
            fs.open(&FileOpenRequest {
                path: "\\Device\\CdRom0\\..\\outside.txt".to_owned(),
                write_access: false
            }),
            Err(KernelServiceError::NotFound),
            "a `..` escape never reaches the parent directory"
        );
    }
}
