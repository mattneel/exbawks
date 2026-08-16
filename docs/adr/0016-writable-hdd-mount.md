# ADR 0016: Writable Hard-Disk Mount

- Status: accepted
- Date: 2026-08-16

## Context

ADR 0014 mounted one read-only host directory as the game disc and deferred
writable mounts. The Dino Crisis 3 image now boots past its disc checks and
bootstraps its save storage: it opens `\Device\Harddisk0\Partition1\TDATA`
(title data), and when that fails — the directory does not exist in a disc rip
and the read-only device cannot create it — XAPI concludes the hard disk is
full and reboots to the dashboard's cleanup screen. Reaching the title screen
requires a hard-disk partition the title can create directories and save files
in.

## Decision

Add a second mount to the host file device: `\Device\Harddisk0\Partition1`
resolves to a **writable** host directory (the "HDD root") when one is
configured. The disc prefixes (`\Device\CdRom0`, `D:`) keep resolving to the
read-only disc mount. When no HDD root is configured, partition1 falls back to
the read-only disc root (presence probes still pass; creation fails as
before).

The CLI supplies a per-title HDD root outside any repository:
`%LOCALAPPDATA%\exbawks\hdd\<title-id>\` (falling back to the system temp
directory when `LOCALAPPDATA` is unset), created on demand at load. Titles
write saves there; the directory persists between runs like a real console's
drive. No proprietary data enters the repository, and tests keep using
temporary directories.

On the writable mount the device honors what the read-only mount refuses:

- open-for-write of an existing file,
- `FILE_CREATE`/`FILE_OPEN_IF`/`FILE_OVERWRITE_IF` dispositions creating a
  missing file, and
- `FILE_DIRECTORY_FILE` opens creating a missing directory
  (`create_dir_all` inside the sandbox).

`FileOpenRequest` carries the parsed intent (`write_access`, `create`,
`directory`) so the kernel exports stay parsers and the device stays the
policy point. `NtWriteFile` joins the export set through a symmetric
`KernelServices::write_file`. The sandbox rules of ADR 0014 (component-depth
check, canonicalized containment) apply to the HDD mount unchanged; creation
paths are sandbox-resolved before any host `create` call.

## Consequences

The guest can now modify host state, but only inside the single configured
HDD directory, which the emulator creates and owns. The disc stays read-only,
so game assets cannot be altered. A title's save bootstrap (create TDATA,
write saves) proceeds instead of reading as a full disk.

The `FileOpenRequest` widening is a public `exbawks-kernel` API change; all
in-tree implementations and tests are updated with it.
