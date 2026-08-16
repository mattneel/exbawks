# ADR 0014: Host-Backed File Device Model

- Status: accepted
- Date: 2026-08-16

## Context

Retail titles open and read their assets through the `Nt*File` kernel exports
(`NtOpenFile`, `NtCreateFile`, `NtReadFile`, `NtQueryInformationFile`, …). The
Dino Crisis 3 image reaches `NtOpenFile` immediately after it allocates its
heap, so the path to a title screen requires real file I/O: the guest must be
able to read the bytes of the files that ship alongside `default.xbe`.

On the console those exports resolve NT object paths (`\Device\CdRom0\…`,
`\Device\Harddisk0\Partition1\…`, `\??\D:\…`) against the object namespace and
the drive letters XAPI maps for the title. The bytes live on the game disc,
which for this project is a host directory outside the repository (the game
image stays in `C:\xbx`, never committed). Reading host files from
**guest-controlled paths** is a sandbox-escape risk: a malformed or hostile
path must never read a file outside the mounted game directory.

This is a new subsystem with its own object type (file handles, distinct from
thread handles), a new `KernelServices` surface, and a host trust boundary, so
it needs an ADR before implementation (per the process rule on new
architecture decisions and host access).

## Decision

Introduce a **host-backed file device** owned by `exbawks-core` and reached by
kernel exports through the `KernelServices` seam (ADR 0012). No kernel export
touches the host filesystem directly; every access goes through typed service
methods.

### Mounts

The emulator holds a small mount table mapping Xbox device/drive prefixes to
one host root directory each. The default (and, for now, only) mount binds the
title's directory — the parent of the launched `default.xbe` — as the read-only
game disc, addressable as `\Device\CdRom0\`, `\Device\Harddisk0\Partition1\`,
and the `D:` / `\??\D:\` drive letter. The host root is runtime configuration;
it is never baked into the binary and never a repository path.

### Sandboxed path resolution

Guest NT paths are parsed into a device prefix plus a relative remainder. The
remainder is split on `\` and `/`, and resolution:

- rejects any absolute host path, drive-qualified host path, or UNC form,
- rejects a `..` component that would ascend above the mount root (tracked by
  component depth, never by string prefix on the resolved path),
- rejects embedded NUL bytes and empty components from doubled separators,
- resolves case-insensitively (the console's file systems are), and
- canonicalizes the result and re-checks that it is still inside the mount
  root before any open.

A path that fails resolution returns `STATUS_OBJECT_PATH_NOT_FOUND` /
`STATUS_OBJECT_NAME_NOT_FOUND`; it never reaches a host open call. This
component-depth check is the security boundary and is unit-tested with hostile
inputs (`..\..\..\`, `\Device\…\..\..`, absolute and UNC forms).

### File objects and the service surface

Open files are a distinct object type held in a file table keyed by guest
handle, separate from the thread handle set. `KernelServices` gains:

- `open_file(request: FileOpenRequest) -> Result<FileOpened, KernelServiceError>`
  — resolves the path within a mount, opens the host file read-only, records
  the file object, and returns the guest handle plus the create disposition
  result.
- `read_file(handle, offset, len) -> Result<Vec<u8>, KernelServiceError>`
  — reads at an explicit byte offset (or the maintained file pointer when the
  guest passes none) without moving data through the caller-visible
  `GuestMemory` borrow; the export copies the returned bytes into guest memory.
- `query_file(handle, class) -> Result<FileInfo, KernelServiceError>`
  — answers the size/position classes titles read after opening.

`close_handle` (ADR 0012) closes a file object as well as a thread handle; the
handle spaces are disjoint so one method suffices.

### Read-only first

The initial device is read-only: the game disc. Create/open dispositions that
require writing (`FILE_SUPERSEDE`, `FILE_OVERWRITE`, write access) return a
typed error until the writable user/save mounts land. This keeps the first
slice's trust boundary as narrow as possible — the guest can read the mounted
directory and nothing else, and cannot modify the host at all.

## Consequences

Exports stay pure guest-facing logic (parse OBJECT_ATTRIBUTES / IO_STATUS_BLOCK,
call a service, write out-parameters, return an NTSTATUS) and unit-test against
`UnsupportedServices` plus fakes, exactly like the other families.

The host trust boundary lives in one place — the resolver in `exbawks-core` —
so it is auditable and testable in isolation, and no proprietary data enters
the repository: the mount root is supplied at runtime and all tests use
synthetic files in temporary directories.

Because the device is read-only and confined to a single mounted directory,
the first slice cannot escape the game directory or alter the host. Writable
mounts, the full object namespace (symbolic links, `Ob*`), overlapped/async
I/O, and directory enumeration are deferred and will each name their boot-plan
task.
