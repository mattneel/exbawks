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
use std::io::{Read, Seek, SeekFrom, Write};
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
const DISC_PREFIXES: [&str; 4] = ["\\Device\\CdRom0", "\\??\\D:", "\\??\\CdRom0:", "D:"];

/// The NT prefix that names the writable hard-disk partition (ADR 0016).
const HDD_PREFIX: &str = "\\Device\\Harddisk0\\Partition1";

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

/// A host-backed file device: a read-only disc mount and, when configured, a
/// writable hard-disk mount (ADR 0016).
pub(crate) struct HostFileSystem {
    /// The game-disc mount root, or `None` when no disc is mounted.
    disc_root: Option<PathBuf>,
    /// The writable hard-disk mount root, or `None` for a read-only world.
    hdd_root: Option<PathBuf>,
    files: HashMap<u32, OpenFile>,
    next_handle: u32,
    /// Object-namespace symbolic links the guest created (uppercased name →
    /// target path). Titles link their drive letters (`\??\D:`, `\??\T:`, …)
    /// to device paths at startup; resolution rewrites a matching prefix.
    links: HashMap<String, String>,
    /// Open symbolic-link object handles (handle → target string).
    link_objects: HashMap<u32, String>,
}

impl HostFileSystem {
    /// Creates a device with optional disc (read-only) and HDD (writable)
    /// mounts.
    pub(crate) fn new(disc_root: Option<PathBuf>, hdd_root: Option<PathBuf>) -> Self {
        Self {
            disc_root,
            hdd_root,
            files: HashMap::new(),
            next_handle: FILE_HANDLE_BASE,
            links: HashMap::new(),
            link_objects: HashMap::new(),
        }
    }

    /// Opens a handle to an existing guest symbolic link.
    pub(crate) fn open_link_object(&mut self, name: &str) -> Result<u32, KernelServiceError> {
        let target = self
            .links
            .get(&name.to_ascii_uppercase())
            .cloned()
            .ok_or(KernelServiceError::NotFound)?;
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(FILE_HANDLE_STEP);
        self.link_objects.insert(handle, target);
        Ok(handle)
    }

    /// Returns the target of an open symbolic-link handle.
    pub(crate) fn link_target(&self, handle: u32) -> Result<String, KernelServiceError> {
        self.link_objects.get(&handle).cloned().ok_or(KernelServiceError::InvalidHandle)
    }

    /// Records one guest symbolic link (case-insensitive name).
    pub(crate) fn create_link(&mut self, name: &str, target: &str) {
        tracing::debug!(name, target, "guest symbolic link created");
        self.links.insert(name.to_ascii_uppercase(), target.to_owned());
    }

    /// Removes one guest symbolic link, returning whether it existed.
    pub(crate) fn delete_link(&mut self, name: &str) -> bool {
        self.links.remove(&name.to_ascii_uppercase()).is_some()
    }

    /// Rewrites guest-created link prefixes in a path, bounded against link
    /// cycles. A link matches case-insensitively and only at a separator
    /// boundary, like the device prefixes.
    fn apply_links(&self, path: &str) -> String {
        const MAX_HOPS: usize = 4;

        let mut current = path.to_owned();
        for _ in 0..MAX_HOPS {
            let mut rewritten = None;
            for (name, target) in &self.links {
                if let Some(rest) = strip_prefix_ci(&current, name)
                    && (rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/'))
                {
                    rewritten = Some(format!("{target}{rest}"));
                    break;
                }
            }
            match rewritten {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    /// Splits a device-qualified NT path into its mount root, the remainder,
    /// and whether the mount is writable (ADR 0016).
    fn mount_for(&self, path: &str) -> Option<(&Path, String, bool)> {
        // The hard-disk partition is the writable mount when configured;
        // without one it falls back to the read-only disc root so presence
        // probes still pass.
        if let Some(rest) = strip_prefix_ci(path, HDD_PREFIX)
            && (rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/'))
        {
            if let Some(root) = self.hdd_root.as_deref() {
                return Some((root, rest.to_owned(), true));
            }
            let root = self.disc_root.as_deref()?;
            return Some((root, rest.to_owned(), false));
        }
        let root = self.disc_root.as_deref()?;
        for prefix in DISC_PREFIXES {
            if let Some(rest) = strip_prefix_ci(path, prefix) {
                // The remainder must be empty or begin at a separator, so a
                // device name is never a prefix of a longer name.
                if rest.is_empty() || rest.starts_with('\\') || rest.starts_with('/') {
                    return Some((root, rest.to_owned(), false));
                }
            }
        }
        None
    }

    /// Resolves a guest NT path to a host path inside a mount root.
    ///
    /// Returns the joined path, whether it exists yet, and whether the mount
    /// is writable; `None` when the device is unknown or the path escapes the
    /// mount. An existing path is canonicalized and containment-checked; a
    /// missing one (a creation target) has its deepest existing ancestor
    /// checked instead, and its leaf components are sandbox-clean, so the
    /// join cannot escape.
    fn locate(&self, path: &str) -> Option<(PathBuf, bool, bool)> {
        let path = self.apply_links(path);
        let (root, remainder, writable) = self.mount_for(&path)?;
        let components = sandbox_components(&remainder)?;

        let canonical_root = root.canonicalize().ok()?;
        let mut resolved = root.to_path_buf();
        for component in &components {
            resolved.push(component);
        }

        if let Ok(canonical) = resolved.canonicalize() {
            if !canonical.starts_with(&canonical_root) {
                return None;
            }
            return Some((canonical, true, writable));
        }

        // A creation target: verify the deepest existing ancestor is still
        // inside the mount (a host symlink could otherwise lead out).
        let mut ancestor = resolved.as_path();
        while let Some(parent) = ancestor.parent() {
            if let Ok(canonical) = parent.canonicalize() {
                if !canonical.starts_with(&canonical_root) {
                    return None;
                }
                return Some((resolved.clone(), false, writable));
            }
            ancestor = parent;
        }
        None
    }

    /// Opens (or, on the writable mount, creates) a guest object.
    pub(crate) fn open(
        &mut self,
        request: &FileOpenRequest,
    ) -> Result<FileOpened, KernelServiceError> {
        let (resolved, exists, writable) =
            self.locate(&request.path).ok_or(KernelServiceError::NotFound)?;
        if (request.write_access || (request.create && !exists)) && !writable {
            // The disc mount stays read-only (ADR 0014).
            return Err(KernelServiceError::AccessDenied);
        }

        let mut created = false;
        // A regular file opens for reading (plus writing on the writable
        // mount); a directory or device object opens as a marker. A missing
        // object is created when the disposition asks for it (ADR 0016).
        let file = if exists && resolved.is_file() {
            let handle = File::options()
                .read(true)
                .write(request.write_access)
                .open(&resolved)
                .map_err(|_| KernelServiceError::NotFound)?;
            Some(handle)
        } else if exists && resolved.is_dir() {
            None
        } else if !exists && request.create {
            created = true;
            if request.directory {
                std::fs::create_dir_all(&resolved).map_err(|_| KernelServiceError::AccessDenied)?;
                None
            } else {
                if let Some(parent) = resolved.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|_| KernelServiceError::AccessDenied)?;
                }
                let handle = File::options()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&resolved)
                    .map_err(|_| KernelServiceError::AccessDenied)?;
                Some(handle)
            }
        } else {
            return Err(KernelServiceError::NotFound);
        };

        let is_directory = file.is_none();
        let handle = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(FILE_HANDLE_STEP);
        self.files.insert(handle, OpenFile { file, position: 0 });
        tracing::debug!(path = %request.path, handle, is_directory, created, "opened guest object");
        Ok(FileOpened { handle, created })
    }

    /// Writes bytes at an explicit offset or the file pointer (ADR 0016).
    /// Moves one file's pointer to an absolute offset.
    pub(crate) fn set_position(
        &mut self,
        handle: u32,
        offset: u64,
    ) -> Result<(), KernelServiceError> {
        let entry = self.files.get_mut(&handle).ok_or(KernelServiceError::InvalidHandle)?;
        entry.position = offset;
        Ok(())
    }

    /// Sets one file's length; refused for directories and devices.
    pub(crate) fn set_length(
        &mut self,
        handle: u32,
        length: u64,
    ) -> Result<(), KernelServiceError> {
        let entry = self.files.get_mut(&handle).ok_or(KernelServiceError::InvalidHandle)?;
        let file = entry.file.as_mut().ok_or(KernelServiceError::AccessDenied)?;
        file.set_len(length).map_err(|_| KernelServiceError::AccessDenied)
    }

    pub(crate) fn write(
        &mut self,
        handle: u32,
        offset: Option<u64>,
        bytes: &[u8],
    ) -> Result<u32, KernelServiceError> {
        let entry = self.files.get_mut(&handle).ok_or(KernelServiceError::InvalidHandle)?;
        let Some(file) = entry.file.as_mut() else {
            // A directory or device object has no byte stream.
            return Err(KernelServiceError::AccessDenied);
        };
        let start = offset.unwrap_or(entry.position);
        file.seek(SeekFrom::Start(start)).map_err(|_| KernelServiceError::AccessDenied)?;
        // A file opened read-only fails the write here, which is the correct
        // access-denied answer for a read-only handle or mount.
        file.write_all(bytes).map_err(|_| KernelServiceError::AccessDenied)?;
        entry.position = start.saturating_add(bytes.len() as u64);
        Ok(bytes.len() as u32)
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
        Ok(FileInfo { size, position: entry.position, directory: entry.file.is_none() })
    }

    /// Closes one handle, returning whether it named an open object.
    pub(crate) fn close(&mut self, handle: u32) -> bool {
        self.files.remove(&handle).is_some() || self.link_objects.remove(&handle).is_some()
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

    /// Builds a read-only open request for a path.
    fn read_open(path: &str) -> FileOpenRequest {
        FileOpenRequest {
            path: path.to_owned(),
            write_access: false,
            create: false,
            directory: false,
        }
    }

    #[test]
    fn unmounted_device_resolves_to_nothing() {
        let fs = HostFileSystem::new(None, None);
        assert!(fs.locate("\\Device\\CdRom0\\anything").is_none());
    }

    #[test]
    fn a_foreign_device_is_unmounted() {
        let fs = HostFileSystem::new(Some(PathBuf::from(".")), None);
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
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()), None);

        let opened = fs.open(&read_open("\\Device\\CdRom0\\data.bin")).expect("open succeeds");
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
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()), None);
        // Opening the bare device (the mount root, a directory) succeeds so a
        // disc/HDD presence check passes.
        let opened = fs.open(&read_open("\\Device\\CdRom0")).expect("device open succeeds");
        assert_eq!(fs.info(opened.handle).unwrap().size, 0, "a directory has no size");
        assert!(fs.read(opened.handle, None, 16).unwrap().is_empty(), "a directory reads empty");
    }

    #[test]
    fn a_write_open_and_a_missing_file_are_refused() {
        let scratch = ScratchDir::new("deny");
        std::fs::write(scratch.0.join("x"), b"x").expect("write");
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()), None);
        assert_eq!(
            fs.open(&FileOpenRequest { write_access: true, ..read_open("\\Device\\CdRom0\\x") }),
            Err(KernelServiceError::AccessDenied),
            "the disc is read-only"
        );
        assert_eq!(
            fs.open(&read_open("\\Device\\CdRom0\\nope.bin")),
            Err(KernelServiceError::NotFound),
            "a missing file is not found"
        );
        assert_eq!(
            fs.open(&FileOpenRequest { create: true, ..read_open("\\Device\\CdRom0\\new.bin") }),
            Err(KernelServiceError::AccessDenied),
            "creation is refused on the read-only disc"
        );
    }

    #[test]
    fn the_hdd_mount_creates_directories_and_writes_files() {
        let scratch = ScratchDir::new("hdd");
        let disc = scratch.0.join("disc");
        let hdd = scratch.0.join("hdd");
        std::fs::create_dir_all(&disc).expect("create disc");
        std::fs::create_dir_all(&hdd).expect("create hdd");
        let mut fs = HostFileSystem::new(Some(disc), Some(hdd.clone()));

        // The title creates its save directory on the hard disk.
        let dir = fs
            .open(&FileOpenRequest {
                create: true,
                directory: true,
                ..read_open("\\Device\\Harddisk0\\Partition1\\TDATA\\43430003")
            })
            .expect("directory creation succeeds");
        assert!(dir.created, "the directory was created");
        assert!(hdd.join("TDATA").join("43430003").is_dir());

        // Then creates and writes a save file inside it.
        let file = fs
            .open(&FileOpenRequest {
                write_access: true,
                create: true,
                ..read_open("\\Device\\Harddisk0\\Partition1\\TDATA\\43430003\\save.dat")
            })
            .expect("file creation succeeds");
        assert!(file.created);
        assert_eq!(fs.write(file.handle, Some(0), b"SAVEDATA").unwrap(), 8);
        assert_eq!(fs.read(file.handle, Some(4), 4).unwrap(), b"DATA");
        // The pointer advanced past the explicit write.
        assert_eq!(fs.info(file.handle).unwrap().size, 8);
    }

    #[test]
    fn a_guest_link_resolves_through_to_the_mount() {
        let scratch = ScratchDir::new("links");
        std::fs::create_dir_all(scratch.0.join("TDATA")).expect("create dir");
        std::fs::write(scratch.0.join("TDATA").join("save.bin"), b"SAVE").expect("write");
        let mut fs = HostFileSystem::new(Some(scratch.0.clone()), None);
        // The title mounts T: at a device path, then opens through it.
        fs.create_link("\\??\\T:", "\\Device\\Harddisk0\\Partition1\\TDATA");
        let opened =
            fs.open(&read_open("\\??\\t:\\save.bin")).expect("open through the link succeeds");
        assert_eq!(fs.read(opened.handle, Some(0), 4).unwrap(), b"SAVE");
        assert!(fs.delete_link("\\??\\T:"), "the link deletes");
        assert!(
            fs.open(&read_open("\\??\\T:\\save.bin")).is_err(),
            "a deleted link no longer resolves"
        );
    }

    #[test]
    fn an_escape_attempt_is_confined() {
        let scratch = ScratchDir::new("escape");
        // A secret one level above the mount root.
        std::fs::write(scratch.0.join("outside.txt"), b"secret").expect("write");
        let inside = scratch.0.join("disc");
        std::fs::create_dir_all(&inside).expect("create mount");
        let mut fs = HostFileSystem::new(Some(inside), None);
        assert_eq!(
            fs.open(&read_open("\\Device\\CdRom0\\..\\outside.txt")),
            Err(KernelServiceError::NotFound),
            "a `..` escape never reaches the parent directory"
        );
    }
}
