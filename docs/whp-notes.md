# WHP execution tier — implementation notes

Working notes for `exbawks-whp` (ADR 0013). Signatures are from the Microsoft
Learn `WinHvPlatform.h` / `WinHvEmulation.h` reference, cross-checked against
QEMU `whpx-all.c`, x86matthew's WinVisor, StrikerX3's virt86/StrikeBox, and
momo5502's sogen WHP backend.

## Linkage

Load `WinHvPlatform.dll` (platform) and `WinHvEmulation.dll` (`WHvEmulator*`)
dynamically with `LoadLibraryW` + `GetProcAddress`, not as raw-dylib
imports. The optional feature's libraries may be absent, and a load-time
import would stop the whole CLI from launching on a host without WHP. Every
`WHv*` function returns an `HRESULT` (`i32`); success is non-negative.

On `x86_64-pc-windows-msvc`, `extern "system"` is `extern "C"` and the
exports are undecorated.

## Doctor

`WHvGetCapability(WHvCapabilityCodeHypervisorPresent = 0, &bool, 4, &written)`.
The output is a 4-byte `BOOL`. Absence is reported as `S_OK` with a false
payload, so both the HRESULT and the value must be checked. Report library
present, hypervisor present, and target distinctly.

## Partition bring-up (strict order)

1. `WHvCreatePartition` — opaque handle only.
2. `WHvSetPartitionProperty` × N, before setup. Always pass the full
   `sizeof(WHV_PARTITION_PROPERTY)` union. Required: `ProcessorCount`
   (code `0x1FFF`) = 1. To intercept: `ExtendedVmExits` (code `0x1`) with
   `X64CpuidExit` / `X64MsrExit` / `X64RdtscExit` / `ExceptionExit` bits, plus
   `ExceptionExitBitmap` (code `0x2`) as `(1 << #PF=0x0E) | (1 << #GP=0x0D)`.
3. `WHvSetupPartition` — instantiates; `WHV_E_INVALID_PARTITION_CONFIG` if a
   required property is missing.
4. `WHvMapGpaRange` / `WHvCreateVirtualProcessor` — only after setup.

Since 19H2, `ExtendedVmExits`, `ExceptionExitBitmap`, `X64MsrExitBitmap`, and
`CpuidExitList` can also be changed after setup.

## Memory map (exit economics, ADR 0013)

`WHvMapGpaRange(part, host_va, gpa, size, flags)`; flags are
Read=1 / Write=2 / Execute=4 / TrackDirtyPages=8. Source VA, GPA, and size
are 4 KiB-aligned; a map replaces any prior mapping of those pages.

- RAM: `VirtualAlloc` 64 MiB, map R|W|X at GPA 0.
- GPU aperture: alias the same host pages at `0xF000_0000` R|W|X so
  write-combined stores never exit.
- NV2A registers: leave `0xFD00_0000` unmapped → touch faults out as
  `MemoryAccess` with `AccessInfo.GpaUnmapped = 1`.
- Kernel gate region: leave `0xFF80_0000 + ordinal*4` unmapped → a
  `call [slot]` fetch faults out with the ordinal encoded in the GPA.
- Dirty bitmap: map with TrackDirtyPages, query with
  `WHvQueryGpaRangeDirtyBitmap` for the texture-cache invalidation signal.

## 32-bit protected-mode boot state

WHP starts a fresh vCPU in real mode at the reset vector. With VT-x
unrestricted guest (confirm `WHV_X64_PROCESSOR_FEATURES.UnrestrictedGuestSupport`)
the HLE loader sets protected-mode state directly and never runs 16-bit code:

- `CR0 = 0x1` (PE); add `0x8000_0000` (PG) only with paging. `CR4 = 0`
  (PAE off). `EFER = 0` (not long mode). `RFLAGS = 0x2`.
- Flat CS: Base 0, Limit `0xFFFF_FFFF`, Selector `0x08`, Attributes
  `0xC09B` (Type 0xB, S=1, P=1, D/B=1, G=1, L=0).
- Flat DS/ES/FS/GS/SS: Attributes `0xC093` (Type 0x3 data), Selector `0x10`.
- Set GDTR/IDTR consistently; a bad segment combination surfaces as
  `WHvRunVpExitReasonInvalidVpRegisterValue`.
- Unpaged flat physical (`CR0.PG = 0`, segment bases 0, guest-linear ==
  guest-physical) is the simplest first target and matches the software MMU.

Registers are set through `WHvSetVirtualProcessorRegisters` with parallel
name/value arrays; write 32-bit values into the low dword of each `Reg64`.

## Run loop

`WHvRunVirtualProcessor(part, 0, &exit, sizeof(exit))` blocks until the guest
needs servicing. `VpContext` is valid on every exit and carries `Rip`,
`Rflags`, `Cs`, and `InstructionLength` (advance RIP past a trapped
instruction with `Rip + InstructionLength`). Exit reasons to handle:
`MemoryAccess` (gate/MMIO — GPA, access type, and captured `InstructionBytes`),
`X64IoPortAccess`, `X64Cpuid`, `X64MsrAccess`, `X64Rdtsc`, `X64Halt`,
`X64InterruptWindow`, `Canceled` (the cross-thread doorbell via
`WHvCancelRunVirtualProcessor`), and the fatal `UnrecoverableException` /
`InvalidVpRegisterValue` / `UnsupportedFeature`.

## Instruction emulator (optional)

`WHvEmulatorTryMmioEmulation` / `WHvEmulatorTryIoEmulation` decode the
faulting instruction and call back into device handlers, so the MMIO path
needs no in-house x86 decoder. One emulator handle per vCPU (not
thread-safe shared). Its callback must service code fetches too, because the
exit does not always carry `InstructionBytes`. Check `FAILED(hr)` then
`status.EmulationSuccessful`; on `InternalEmulationFailure` (some SIMD /
non-temporal store encodings are reported to fail — unverified) fall back to
an in-house decode. WinVisor skips this and drives everything off `#PF`;
QEMU master replaced it with an in-tree decoder.

## TSC / determinism

A dedicated RDTSC exit exists: enable `ExtendedVmExits.X64RdtscExit`, handle
`WHvRunVpExitReasonX64Rdtsc`. By default WHP passes the host TSC through with
a per-partition virtual offset — not deterministic. Goldens run on the
interpreter tier, so WHP passthrough is acceptable; synthesize the TSC on the
RDTSC exit only for a title whose pacing needs it.

## Verified on hardware (M0 spike)

- Partition bring-up, `WHvMapGpaRange`, register set/get, the flat 32-bit
  boot state, the `HLT` → `X64Halt` exit, and the unmapped-gate
  `MemoryAccess` exit (exact GPA + `GpaUnmapped` + read access) all behave
  as documented above.
- **Transcribe `WHV_REGISTER_NAME` values from the SDK header, never from
  memory.** The control registers are `Cr0=0x1C, Cr2=0x1D, Cr3=0x1E,
  Cr4=0x1F, Cr8=0x20` (debug registers follow at `0x21+`), and
  `WHvX64RegisterEfer` is `0x2001` in the MSR block (`Tsc=0x2000`). A wrong
  name either lands in an adjacent register **silently** (an early Cr0=0x20
  wrote the TPR and read it back "successfully") or is rejected with
  `WHV_E_INVALID_VP_REGISTER_NAME` — and a whole set/get batch fails when
  any name in it is invalid. Passing straight-line tests do not prove the
  boot state: under unrestricted guest, injected flat segment caches run
  `HLT`-style code identically even with `CR0.PE` unset.
- On the `X64Halt` exit the platform reports `Rip` **past** the completed
  `HLT`, not at it.
- `WHvMapGpaRange` requires a page-granular `SizeInBytes` (`E_INVALIDARG`
  otherwise), and concurrent partition bring-up/teardown across threads is
  flaky — the hardware tests serialize on a mutex.
- Zeroed GDTR/IDTR pass entry checks, but any guest exception then triple
  faults into an undiagnosable `UnrecoverableException`; the M1 run loop
  should intercept exceptions via `ExtendedVmExits.ExceptionExit` +
  `ExceptionExitBitmap` before real guest code runs.

## Hazards

- XSAVE state size is host-CPU-dependent (AVX-512 grows it). Query the size
  with a zero-size `WHvGetVirtualProcessorState(..XsaveState..)` call first,
  then allocate — never a fixed buffer. sogen's fixed size is a bug.
- WinVisor documents a `WHvRunVirtualProcessor` infinite loop when the guest
  reads the hypervisor shared page near `KUSER_SHARED_DATA`; avoid mapping
  there.
- Reuse one `WHV_RUN_VP_EXIT_CONTEXT` per vCPU; do not reallocate per run.
