# Coding Agent Handoff

## Mission

Build Exbawks into a Windows 11 original Xbox research emulator.

Keep the first runtime narrow. Execute one generated synthetic XBE through one kernel HLE call.

## Repository state

The repository is an implementation scaffold with functional pure-logic components.

Implemented components include:

- Checked XBE header and section parsing.
- Retail and debug entry-point key decoding.
- A software guest address space with aliases and permissions.
- A one-million-entry sidecar page table.
- Physical-page generation tracking.
- An `iced-x86` basic-block decoder.
- Direct-rewrite translation plans.
- Code-cache dependency checks.
- Kernel and graphics HLE interfaces.
- Windows section and placeholder ownership wrappers.
- Host memory geometry queries (`MEM-001`).
- Single-owner placeholder split and coalesce operations (`MEM-002`).
- A reserved 4 GiB-aligned high guest arena (`MEM-003`).
- Coherent Windows RAM aliases through pagefile section views (`MEM-004`).
- A Windows `GuestMemory` backend with generated equivalence tests against
  the software backend (`MEM-005`).
- Single-owner executable code buffers with write and execute phases
  (`JIT-001`).
- Register-only direct emission and a dispatcher under the ADR 0006 block
  ABI, verified against an interpreter oracle (`JIT-002`).
- Sorted source ranges and fault-site metadata with binary-search lookups
  (`JIT-003`).
- Kernel thunk gate patching and runtime gate-call dispatch (`HLE-001`).
- The startup kernel export set, a guest stack, and the execution loop
  (`HLE-002`).
- A JSON Lines trace writer with sequence numbers and opt-in private host
  paths (`DBG-001`).
- Contiguous XBE section mapping with merged shared-page permissions, so
  genuine retail images load (ADR 0007).
- A CLI for host checks, XBE inspection, decoding, thunk inspection, entry
  planning, and synthetic boot execution.
- Generated public test data that boots to a controlled guest exit.

Incomplete components include:

- Memory operand and guest control-flow translation.
- Fault-site redirection and MMIO dispatch.
- The Windows backend as the active runtime address space.
- Extended XBE metadata parsing (certificates, TLS, libraries).
- Graphics decoding and rendering.

Read [the validation report](../VALIDATION.md) before code changes.

## Mandatory reading order

1. Read `AGENTS.md`.
2. Read `docs/architecture.md`.
3. Read `docs/memory.md`.
4. Read `docs/codegen-contract.md`.
5. Read the accepted ADRs in `docs/adr`.
6. Read `docs/dc3-boot-plan.md`.
7. Select one task from `docs/task-board.md`.

## Immediate objective

Tasks `MEM-001` through `MEM-005`, `JIT-001` through `JIT-003`, `HLE-001`,
and `HLE-002` are complete. The first execution milestone passes on
Windows 11 x86-64.

Active program: boot a retail Dino Crisis 3 image to its title screen with a
deterministic screenshot command. Read [the boot plan](dc3-boot-plan.md).

Milestone M0 is complete (`CORE-001`, `KRN-001`, `CLI-001`, `CLI-002`,
`DBG-002`; ADR 0008 accepted). Milestone M1 is complete except `CORE-003`:
ADRs 0009 and 0010 are accepted, `CPU-001` through `CPU-003` landed the
tier-0 interpreter (memory operands, ALU, control flow, stack, strings,
CPUID, RDTSC), and `CORE-002` wired the run-loop fallback plus shared SMC
generation bumps. The M1 checkpoint is exceeded: the retail image executes
its complete XAPI entry path and stops at
`UnimplementedKernelExport { 255 } (PsCreateSystemThreadEx)`.

Evidence update for planning: XDK 5558's XAPI startup calls
`PsCreateSystemThreadEx` as its FIRST kernel call — before heap init — so
the main guest thread is kernel-created. Thread-creation HLE (M4's `KRN-005`
scope) moves ahead of the M3 heap work in boot order; the minimal viable
form is creating the main thread's context inline and running it, deferring
the full scheduler. `CORE-003` (KPCR/TIB page, XBE-declared stack size)
naturally pairs with it.

`CORE-003` and `KRN-005` are complete (ADRs 0011 and 0012 accepted): the
boot thread has a KPCR/KTHREAD page with an fs base, an XBE-sized stack, and
guard-page-protected child stacks; `PsCreateSystemThreadEx`,
`PsTerminateSystemThread`, and a minimal `NtClose` (first `HLE-005` slice)
are real. The retail image now runs its whole XAPI startup — creating its
worker thread and closing the handle — and stops with
`GuestFault { address: 0 }`: guest code shortly after the `NtClose` return
(last translated block at guest `0x1A6A5A`) transfers control to null, with
no kernel call in between.

Execution-engine pivot: ADR 0013 makes the **Windows Hypervisor Platform the
primary execution tier**, with the interpreter retained as the deterministic
oracle/golden tier and the hand-written JIT track dropped. `exbawks-whp` has
begun (capability doctor; `exbawks doctor` confirms WHP is usable on the
build host). `docs/whp-notes.md` holds the full partition/boot/exit API
reference. Diagnostics: `exbawks coverage` reports the implemented/stub/
missing burndown across CPU/kernel/GPU surfaces (DC3: 24/4/124 of 152 imports).

Two active threads:

1. **WHP-M0 spike: DONE, verified on hardware.** `exbawks-whp` now has the
   dynamically loaded `WhpApi` (never raw-dylib), the RAII `Machine`
   (partition + one vCPU, strict bring-up order, vCPU deleted before
   partition), `HostRegion` + `map_gpa`/`unmap_gpa`, register set/get, the
   flat 32-bit boot state, and a decoded exit surface. Hardware tests prove:
   `HLT` → `X64Halt` (RIP reported *past* the halt), an unmapped gate read →
   `MemoryAccess` with the exact GPA `0xFF80_0008` + `GpaUnmapped`, an
   unmapped fetch → execute-type fault, and register round-trips. An
   adversarial review against the installed SDK headers caught
   mistranscribed register names (`Cr0` was addressing the TPR; `Efer` is
   `0x2001`), a `map_gpa` lifetime hole (the machine now owns mapped
   regions), and non-page-granular sizes — all fixed and re-proven on
   hardware; the hard-won platform findings live in `docs/whp-notes.md`
   ("Verified on hardware").

   **WHP-M1: DONE — the retail image runs natively.** Guest RAM lives in a
   page-aligned stable `AlignedBuffer` (exbawks-platform) that the partition
   maps directly; `Emulator::run_whp` mirrors the software page table into
   GPA space (epoch-triggered resync), boots from `CpuState` (CR4
   OSFXSR/OSXMMEXCPT so SSE executes), dispatches kernel gates off
   fetch-fault exits via the existing gate-by-EIP path (register sync incl.
   the per-thread `fs` base keeps the cooperative scheduler working
   unchanged), honors null/sentinel thread exits and the ADR 0015 relaunch,
   maps exception exits to typed stops, and ticks the virtual clock from a
   cancel pump. `exbawks run --engine whp`. Hardware-verified findings: an
   execute fault's GPA is page-aligned — take the exact gate address from
   the exit RIP; SSE requires CR4 bits or every SSE op is #UD.

   **WHP-M2: MMIO dispatch DONE.** Device space
   (`0xFD00_0000..0xFF00_0000`) stays unmapped; on a data-access exit the
   engine runs exactly one interpreter step over `MmioView`
   (`core/src/mmio.rs`) — instruction semantics come from the oracle, no
   hand decoder. The stub latches writes for readback (device init programs
   base addresses and rereads them), reads zero for unwritten registers,
   answers all-ones for `0xFE82_0010` (an APU GP-DSP status the retail image
   polls for a hardware-set bit; a zero read spins 100M+ exits), and counts
   accesses per region (`whp heartbeat` lines carry the summary). Also
   landed: `KeStallExecutionProcessor` (benign), `MmQueryAllocationSize`,
   `NtSetInformationFile` (position/allocation/EOF), and a workspace-wide
   `exbawks_whp::hardware_serial_lock()` every partition-creating test must
   hold (parallel 64 MiB partitions hit transient
   `HV_STATUS_INSUFFICIENT_MEMORY`, HRESULT `0xC0370008`).

   **The audio-path burndown so far** (each found with the cancel-pump
   sampling profiler — `RUST_LOG=exbawks_core::emulator=trace`, tally
   `cancel sample` RIPs; strip ANSI before grepping for `rip=`):
   `DirectSoundCreate` needed the AC'97 `GLOB_STA` ready bits (`0xFEC00130`
   override) — its failure left a NULL singleton the image dereferences
   unchecked; the AC'97 channel-control registers (`0xFEC001n0+0xB`, SPDIF
   at `+0x70`) are self-clearing, so their reads bypass the latch; and the
   GP-DSP command FIFO needed instant-consumer mailboxes — the guest
   programs comm-region physical bases into `0xFE820808` (twice: two
   regions), and the engine unmaps each region's mailbox page
   (`base|0x8000_0000 + 0x1000`) so writes are consumed on landing.
   Interrupt plumbing landed alongside: `HalGetInterruptVector`,
   `KeInitializeInterrupt` (28 stack bytes — 24 corrupted the caller's
   stack into a wild jump), `KeConnectInterrupt` returning TRUE.

   Since then: the mailbox model moved to write-through semantics (only
   the polled FIFO head reads consumed; setup data in the comm page
   survives), which completed DirectSound initialization, and
   `MmClaimGpuInstanceMemory` landed — Direct3D is initializing.

   That quiet run was two things: DirectSound's sample-mixer thread
   (`fs=0x80223000`, loop `0x216017`) monopolizing the cooperative
   scheduler, and the main thread waiting on an event nobody could signal.
   Both fixed: **ADR 0017 preemptive time slices** (the cancel pump rotates
   ready threads) and **real blocking waits**
   (`NtWaitForSingleObject`/`Ex` park on a `Waiting` state; `NtSetEvent` and
   thread exit wake them; no-runnable-thread → `STATUS_TIMEOUT`). Then D3D
   init advanced through: `PRAMIN` RAM aliasing of the GPU instance claim,
   wall-time TSC advance, serviced **port I/O** (new WHP exit `0x2`,
   latching port model), and `HalReadWritePCISpace` (synthetic NV2A config
   with real BARs).

   **The cached physical window is REAL (ADR 0010).** `map_physical_window`
   aliases `[0x8000_0000, +ram)` onto physical `[0, ram)`; kernel blocks are
   physical allocations with `VA = 0x8000_0000 | PA` (scrubbed on allocation
   — the guest can dirty any page through the window first); the loader
   reserves physical `[0, 0x11000)` (zero page for D3D's WBINVD-priming
   write + the synthetic kernel image at PA `0x10000`); RAM is capped at
   128 MiB so the window can never reach device space.
   `MmGetPhysicalAddress` does a real page-table walk. An adversarial review
   caught the refactor silently killing the ADR 0015 relaunch restore
   (window VAs are always mapped, so the old `map_anonymous` gate failed →
   launch data zeroed): persisted window regions now reserve their physical
   pages BEFORE the fresh boot allocates (`pending_persist_reservations` in
   `load_xbe`) and restore by writing straight through the window;
   `reserve_cursor_through` is gone. A content-integrity relaunch test
   (magic word round-trips a reboot) pins the path.

   Past the window, D3D advanced through: the PFB trigger register
   (`0xFD10_0410`, self-clearing) and **the first pushbuffer submission** —
   D3D writes `DMA_PUT` (NV_USER channel + 0x40, 64 KiB stride) and polls
   `DMA_GET`; the stub GPU snaps GET to PUT ("infinitely fast") and logs
   `pushbuffer submitted put=... channel=...`.

   **GPU-M0 landed: the NV2A pushbuffer engine**
   (`exbawks-gpu/src/nv2a.rs`): DMA_PUT submissions replay through the
   hardware pusher rules (methods, jumps/calls/returns), objects resolve
   via `RAMHT` in instance memory, and `BACK_END_WRITE_SEMAPHORE_RELEASE`
   writes fence values into guest RAM. Consumed in `run_whp` after the
   MMIO step that observed the PUT write (`consume_gpu_submissions`).
   The semaphore method numbering is provisional (`0x1A4`/`0x1D6C`/
   `0x1D70`) — verify against the live stream once real submissions flow
   (`engine.top_methods`).

   After it, the D3D bring-up burndown continued: W1C interrupt-status
   registers (a latched `NV_PGRAPH_INTR` ACK read as phantom errors —
   D3D's exception handler crashed on them), idle-FIFO status answers
   (`0xFD002080/2400/3214 = 0x10`, `0xFD003220 = 0x101` — the drain loop
   at `0x1CFACC` needs low-marks SET and the push busy bit CLEAR), the
   `Av*` family (the image now SETS ITS DISPLAY MODE — watch for the
   `display mode set` info line), `KeDisconnectInterrupt`,
   `NtFreeVirtualMemory` (leaking), and X:/Y:/Z: cache-drive mounts (an
   unchecked `fopen(Z:\DATA\Ini.itk)` NULL crashed fread). **The title
   now loads its title-screen assets**: `us_font.xtx`, `BG.itk`,
   `pack_00.itk`.

   **The device wall fell, and the retail title now renders.** The cause was
   one wrong answer: `AvSendTVEncoderOption`'s capability query (option 6)
   returned `1`. Direct3D keys a 185-entry display-mode table on that word
   (video standard in bits 8..15, AV pack class in bits 0..7, and a
   capability bit each SDTV entry carries), found no matching mode, returned
   `E_FAIL`, and `Direct3D_CreateDevice` tore its own device down — the game
   then used the NULL global. Answering `AV_STANDARD_NTSC_M`
   (`0x0040_0100`) fixed it. Technique worth reusing: `EXBAWKS_GATE_FRAME=<ordinals>`
   dumps a kernel call's registers and stack frame, which is how the failing
   `E_FAIL` was read off the destructor's frame beside `Direct3D_CreateDevice`'s
   return address.

   After it, in wall order: `KeWaitForSingleObject`/`KeSetEvent` over guest
   dispatcher objects, a scheduler rule that releases parked waits instead
   of reporting a guest exit when the last ready thread ends, mutants,
   `NtResumeThread`/`NtSuspendThread`, `NtWaitForMultipleObjectsEx`, and a
   **real GDT** (BINK's CPUID probe ends in `pop es`, which `#GP`s against
   an empty descriptor table; the table now carries null/code/data plus an
   `fs` entry holding the running thread's KPCR base).

   **Where DC3 is now:** it creates its Direct3D device, sets its display
   mode repeatedly (640x480, pitch 2560, `D3DFMT_LIN_A8R8G8B8`), loads the
   title-screen assets (`title/mdl/title.xom`, `tex_title1.xtx`,
   `title.eff`, `title_voice.xsp`), enters its main loop (mode word
   `[0x3D6E14] = 1`), and drives tens of millions of pushbuffer methods with
   thousands of semaphore releases while the audio mixer runs.

   **The graphics fence was the thing holding rendering back.** Direct3D's
   pushbuffer wait spins on a counter in memory (`[device+0x30]` names the
   block) until the GPU catches up. The fence writes were landing on the
   report object — all of RAM at offset zero — instead of the semaphore
   object, so the wait never finished and the title stalled after four
   submissions. What each method *binds* settles the numbering better than
   any table: `0x01A4` binds the 32-byte block the wait polls, `0x01A0`
   binds the whole of RAM. With that fixed, two minutes of run time gives
   72,853 pushbuffer submissions, 12,601 surface clears, and 66,164 fences.

   **`exbawks run --screenshot <file.png>` captures the scanned-out frame**
   through the cached window, and the clears now land in exactly the buffer
   it reads (`0x040AC000`, pitch 2560). The image is black because the title
   clears to opaque black and nothing rasterizes its geometry yet — the
   capture path itself is proven. **GPU-M2 draws.**
   `exbawks-gpu::fill_triangle` is a half-space rasterizer over the color
   surface, and the engine assembles primitives from `SET_BEGIN_END` plus
   `INLINE_ARRAY` using the declared attribute layout. The stream is legible
   through `exbawks run --gpu-methods <n>` (a Kelvin object on subchannel 0,
   matrices at `0x0480`/`0x0580`/`0x0680`, `ARRAY_ELEMENT16` at `0x1800`),
   and `RUST_LOG=exbawks_gpu=trace` prints each primitive's vertex layout —
   that is how the retail format was read: attribute 0 a float4 position,
   attribute 3 a packed `D3DCOLOR`, attributes 9-12 float2 texture
   coordinates, stride 16 dwords.

   **GPU-M3 textures and blends, and the title screen renders.** The engine
   follows the first texture unit's state and samples linear and swizzled
   `A8R8G8B8` plus `DXT1`/`DXT3`/`DXT5`, modulates by the vertex color, and
   blends by source alpha when the title asks for it. That last part is what
   made the difference: the art is mostly transparent, and writing those
   texels opaquely painted black over every frame.
   `exbawks run --dump-texture <file.png>` writes the most recently sampled
   texture — the retail title's atlas comes out as its own title screen,
   logo and menu text intact, which is how the decode was proven.

   A capture picks its frame by looking rather than guessing: every frame
   the title finishes is scored by how much it varies, and the best is kept.
   A fade passes through flat black and flat white, so brightness is the
   wrong measure and contrast is the right one — with it,
   `exbawks run --screenshot` on the retail image produces the title logo.

   **GPU-M4 runs the vertex program.** `exbawks-gpu::execute` decodes and
   runs the 128-bit instructions a title uploads — a vector and a scalar
   operation per instruction, over attribute, temporary, and constant banks
   — and the engine feeds each vertex through it. Two facts the retail
   stream settled, both easy to get backwards: the constant load index
   addresses the hardware bank directly (no bias), and a program applies the
   **viewport transform itself**, from the scale and offset it receives as
   constants, so only the perspective divide is left to do. Getting either
   wrong collapses every triangle to a point.
   `exbawks run --gpu-program` prints the instruction words, and
   `scratchpad/vpdis.py`-style disassembly off those words is how the field
   layout was confirmed: identity swizzles read `0x1B`, and exactly one
   instruction carries the final bit.

   Vertex arrays assemble too: indices through `ARRAY_ELEMENT16`/`32`,
   attributes fetched at their own offsets and strides from a vertex context
   DMA. **The title draws its scene through the fixed pipeline**
   (`SET_TRANSFORM_EXECUTION_MODE` reads `4`, not `6`), transformed by the
   composite matrix at `0x0680`: each clip component is one matrix *row*
   dotted with the position, and the viewport is already folded in — the
   matrix's third row carries the depth scale (about 2^24) and the viewport
   register is left holding a sub-pixel offset, so nothing but the
   perspective divide and that offset follows.

   Two measurements settled it, and both are worth repeating when a
   transform looks wrong. First, transform a few million vertices under each
   candidate convention and ask which puts most of them on screen: the right
   one gave 64% with medians near the screen centre, the others clustered
   every vertex at the origin. Second, when geometry lands on screen and
   still shades nothing, suspect the color rather than the position — a mesh
   that carries no diffuse attribute is **white**, not transparent black,
   and alpha-zero pixels were being skipped by the blend.

   Depth testing works against the title's own depth surface, by the
   function and write mask it programs — and `CLEAR_SURFACE` clears depth as
   well as color, which matters more than it sounds: without that clear
   every fragment compares against whatever the memory last held, and all
   but a twentieth are rejected. Texture coordinates interpolate in the
   plane of the triangle rather than across the screen.

   **Goldens work.** `exbawks run --frame-digest` prints a captured frame's
   digest and `--expect-frame <digest>` fails the run when it differs. The
   retail title's frame digests identically on consecutive runs, so the
   emulation is deterministic enough for the comparison to mean something.
   A title cannot be committed, so its digest belongs beside the private
   image under `fixtures/private/`, never in a test this repository runs.

   The graphics stack has since been reviewed adversarially, and the review
   was worth its cost: malformed pushbuffer data could panic the engine four
   ways and hang it two more, all of it now fixed and pinned by regression
   tests that submit the hostile streams. The pattern behind most of them is
   worth carrying forward — a guest value used as an index, a length, or a
   loop bound needs its ceiling written down at the point it enters, not
   where it is used. Two correctness bugs came out of the same pass: `DXT3`
   and `DXT5` colour halves always carry four colours, and a compressed
   texture's size comes from its format word rather than a rectangle an
   earlier linear texture left behind.

   **Where the title stops now:** a forty-million-exit run ends in
   `GuestFault { address: 4 }` — a null dereference somewhere past the title
   logo, and the next thing to chase if the sequence should go further. The
   frame the run keeps is unchanged, so whatever fails happens after the
   part that renders.

   The blend factors, face culling, and alpha test a title programs are now
   obeyed rather than assumed — worth knowing why that mattered: **half this
   title's blended draws ask for a destination factor of `ONE`**, the
   additive passes that add light, and rendering them as source-alpha
   darkened exactly what should glow. Culling removed about a fifth of the
   shading work. Neither changed the captured frame, because the frame the
   capture keeps is 2D interface art drawn with a plain over-blend; both
   change the scene, which sits under the title's own fade.

   **`exbawks run --watch-write <address>`** reports every guest instruction
   that writes an address, by mapping its page read-only and stepping the
   writes. It named eighty-seven writers of a Direct3D device page in one
   run. Its reach is bounded by what the interpreter can step, and that
   bound is now the thing worth lifting: the write this was built to catch
   is an `fstp`, and only the x87 control subset is modelled. Extending the
   interpreter's x87 and SSE data instructions widens this tool, the MMIO
   step path, and the oracle tier at once.

   **The wall a long run hits:** Direct3D faults at `0x1CE0FC` reading
   `[device+0x1A04]`, which is null — `cmp ebx,[edx+4]` with `edx` zero,
   called from `0x1CB220`. Nothing in the D3D section writes that field with
   an immediate displacement, so it is reached through a pointer; the
   watchpoint is the right instrument once it can step x87.

   **The register combiners now run per pixel**, and they were the reason
   the title screen came out nearly black: the fixed texture-times-diffuse
   this engine assumed multiplies by a diffuse color the title's own program
   never applies. Its actual program, read off the retail stream with
   `--gpu-combiner`, is one stage computing `spare0 = diffuse` (`ICW`
   `0x04200000`, `OCW` `0x00000C00`) and a final stage of `spare0 +
   specular` with alpha from `spare0` (`0x0000000E` / `0x00001C80`). The
   golden digest moved to `ef73c1a20b4e0234`, and the frame is now lit.

   Method numbering, confirmed against that stream: alpha `ICW` `0x0260`,
   final words `0x0288`/`0x028C`, factors `0x0A60` and `0x0A80`, alpha `OCW`
   `0x0AA0`, colour `ICW` `0x0AC0`, colour `OCW` `0x1E40`, control `0x1E60`
   — each an eight-entry block, and the control word's **low byte is a
   plain stage count** (not the nibble the first attempt assumed, which
   silently produced zero stages and a black frame). In the final stage the
   variables sit one byte higher than in a general stage: `E` at 24, `F` at
   16, `G` at 8, flags in the low byte.

   **All four texture units now sample**, and the second one earns its
   keep: the dominant draw modulates the base texture by an environment
   map with a fourfold scale. That unit is fed by **reflection-map texgen**
   (`SET_TEXGEN` at `0x03C0`, four methods per unit, mode `0x8512`) and a
   per-unit texture matrix (`0x06C0`, stride `0x40`, enabled at `0x0420`),
   because the title supplies it no coordinate array at all — a fact that
   only came out of counting draws whose coordinate set was entirely zero
   (247,235 of 247,550).

   Consulting xemu's decoder was worth it and is worth repeating. It
   corrected three things this project had inferred: the combiner output
   word's two product destinations are ordered `CD` then `AB`, its dot and
   mux flags sit at bits 12-14 rather than 18-20, and texture format
   `0x11` is `LU_IMAGE_R5G6B5` rather than a linear `X8R8G8B8` (which is
   `0x1E`). It also placed texgen at `0x03C0`, not the `0x0FD8` guessed
   from another layout.

   **Fixed-function lighting is modelled**, and it was carrying the
   screen's colour: the title lights its title screen with one infinite
   light (`SET_LIGHTING_ENABLE` `0x0314`, `SET_LIGHT_ENABLE_MASK` `0x03BC`,
   the light's own registers from `0x1000` at a `0x80` stride, only light
   zero programmed), and drawing with an unlit white vertex colour is why
   every frame before this came out grey. The teal appeared the moment the
   light's diffuse term reached the vertex.

   `exbawks run --gpu-method-value <methods>` reads back the last value
   submitted for any graphics method, which is faster than adding a
   counter per guess. It settled four questions at once: this title wraps
   its textures on both axes (`0x1B08` = `0x00010101`), filters them
   linearly (`0x1B14` = `0x02063F01`), runs every texture stage as plain
   2D (`0x1E70` = 0), and enables separate specular (`0x03B8` = 1,
   `0x0294` = `0x00020001`).

   **Specular was measured and deliberately not implemented.** The title
   enables it (`0x03B8` = 1, separate specular via `0x0294` =
   `0x00020001`) and uploads a light specular colour of **0.05** in every
   channel — the final combiner's `SPARE0 + SECONDARY` can therefore add
   at most about 1.3% brightness, which cannot account for any visible
   difference. Its six `SET_SPECULAR_PARAMS` floats
   (`-0.839, -2.887, 3.048, -0.707, -2.507, 2.801`) do not fit
   `pow(x, n)` in the obvious form — both quadratics fall through zero
   near `x = 0.85` — so implementing them would be guesswork for a term
   with no visible budget. Revisit only for a title that sets a real
   specular colour.

   Still approximate, in the order they are worth attacking: **specular is
   never computed**
   though the title enables it and uploads the light's specular colour and
   half vector, the mip level is chosen once per triangle rather than per
   pixel (a steeply perspective triangle wants it varying across), local
   and spot lights are not computed (this title uses neither), and
   material colours are taken from the vertex rather than the material
   registers (harmless here — `SET_COLOR_MATERIAL` is 0 and the vertex
   colour is white).

   Two techniques earned their keep and are worth reaching for again: the
   cancel pump's RIP sampling (`RUST_LOG=exbawks_core::emulator=trace`,
   which also reports `rax/rbx/rcx/rdx/rsi`) pinned the spin to three
   instructions at `0x1CBC20`, and resolving *every* context-DMA bind to the
   object it names identified the semaphore by what it pointed at rather
   than by a constant anyone had written down.

      The interpreter tier remains the deterministic oracle (frontier: `movss`
   at `0x1685A0`).

   **Input, and the wall behind it.** The title renders its title screen and
   asks for a button it can never receive. Its USB stack is real and running
   — `HcControl` `0xBE` (operational), `HcHCCA` `0x010D_A000` — and it drives
   the controller registers directly, because XAPI is statically linked just
   as Direct3D is. ADR 0019 accepts modelling the OHCI controller and a
   synthetic gamepad rather than intercepting `XInput*`, and `exbawks-usb`
   now implements the register file, root hub, frame counter, and done
   queue. `exbawks run --gamepad` attaches one.

   **The driver still does not enumerate, and the reason is now measured
   rather than guessed.** With a device attached it reads the port, clears
   the connect change, sweeps every register once, and stops. A kernel-call
   census over a full run says why: `KeInitializeInterrupt` and
   `KeConnectInterrupt` are each called **four** times, and
   `KeInsertQueueDpc` is called **zero** times. The title connects four
   interrupt service routines and waits; this emulator delivers no
   interrupts at all, so no ISR runs, no DPC is queued, and nothing
   enumerates.

   **Interrupt delivery now exists**, and it needed no programmable
   interrupt controller: `KeInitializeInterrupt` records the guest's
   service routine, `KeConnectInterrupt` marks it live, and the runtime
   calls it on the guest processor between instructions when a device
   raises one, restoring the displaced state when it returns.
   `KeInsertQueueDpc` queues the deferred procedure and the runtime calls
   that too. The vectors this title connects are **49 (USB), 51, 53, and
   55** — `HalGetInterruptVector` maps IRQ *n* to vector `0x30 + n`, so
   USB is IRQ 1.

   The chain runs end to end: the gamepad is attached once the driver has
   its controller running and an interrupt connected (a device present
   before that is announced and then swept away by the driver's own
   initialisation, exactly as a controller plugged in before the console is
   switched on would be), the service routine is called and returns TRUE,
   its deferred procedure runs, and the driver writes the root port back.

   **The descriptor walk is built and tested, and the title still does not
   enumerate.** `exbawks-usb` answers the standard control requests and
   runs the transfer descriptors a driver queues; a test drives a full
   enumeration — descriptor, address, configure, report — through lists
   built the way a driver builds them. The retail title never submits one.

   What it does, exactly: the port write trace shows the driver writing
   `0x00000000` during its initialisation sweep, then — after the interrupt
   and its deferred procedure — writing `0x00010000`, which clears the
   connect-change bit and nothing else. It never reads the port again
   (one read, from the sweep), never sets the reset bit, and never sets
   `ControlListFilled` a second time, so no control transfer is ever
   submitted. Two hypotheses were tested against the running title and both
   are wrong: it is not waiting out a debounce in frames (the frame counter
   advances), and it does not want the port already enabled (bringing it up
   enabled changes nothing).

   So enumeration is driven by something further up the stack that has not
   been identified — most likely a worker thread inside XAPI's USB stack
   that is not being woken or not being scheduled. `NtSetEvent` is called
   585 times and `NtWaitForSingleObjectEx` 815 in a run, so there are
   threads waiting on events; which of them owns the hub is the thing to
   find. Timers still do not fire (`KeSetTimer` records and does nothing)
   and remain a candidate, though the driver arms none after the connect.

2. **Kernel HLE burndown** (substrate-independent, needed under either
   engine). Done this session, in order the retail image demanded them:
   `CORE-004` (gate dispatch when EIP *enters* the gate region, for the
   `mov reg,[slot]; call reg` and `jmp [slot]` forms, not only a decoded
   `call [slot]`); `ExQueryNonVolatileSetting` (a synthetic NTSC-U EEPROM
   profile); `RtlNtStatusToDosError`; `KeInitializeDpc`/`KeInitializeTimerEx`/
   `KeSetTimer` (dispatcher objects; timers do not fire yet);
   `NtAllocateVirtualMemory` (the user-range reserve/commit allocator — the
   Mm/physical-memory model the earlier wall needed, `HLE-003`); the host file
   device (ADR 0014: `NtOpenFile`/`NtCreateFile`/`NtReadFile`/
   `NtQueryInformationFile` over a sandboxed read-only disc mount, `HLE-004`);
   and the `Mm*` contiguous family (`MmAllocateContiguousMemory`(+`Ex`),
   `MmGetPhysicalAddress`, `MmFreeContiguousMemory`, `MmPersistContiguousMemory`,
   `HLE-009`). A directory/device object now opens as a zero-size marker so the
   disc/HDD presence check passes. The retail image runs its **entire early
   initialization** — EEPROM probe, heap allocation, disc/HDD presence check,
   contiguous GPU-buffer allocation — with no reboot.

   Also landed the disc-metadata exports (`NtDeviceIoControlFile` benign media
   probe; `NtQueryVolumeInformationFile` DVD geometry + nonzero free space +
   CD-ROM characteristics), so the game completes its disc and HDD-partition
   probe.

   **Soft-reboot relaunch implemented (ADR 0015).** After the disc/HDD probe
   the game self-relaunches: a launch helper at guest `0x1A6381` (called via
   `0x1A64F9`) does `MmAllocateContiguousMemory` → `MmPersistContiguousMemory`
   → `HalReturnToFirmware(2)`, setting the `LaunchDataPage` global (ordinal
   164 cell at `0x8010_0080`) to the persisted page at `0x8022_5000`; the page
   holds `dwLaunchDataType=1`, `dwTitleId=0x43430003` (DC3 itself),
   `szLaunchPath=""` — an `XLaunchNewImage(NULL, …)` self-relaunch. `Emulator::
   run` now preserves the persisted regions and the `LaunchDataPage` pointer
   across a reset, reloads the same image, and continues (`relaunch_title`).

   **Reboot loop SOLVED: it was FATX volume geometry.** The decisive evidence
   was the launch data itself, dumped at `HalReturnToFirmware` time: the
   `LAUNCH_DATA_PAGE`'s data area (page + `0x400`) held
   `LD_TO_DASHBOARD { dwReason=1, dwContext=0, dwParameter1=2 }` — reason 1 is
   `XLD_LAUNCH_DASHBOARD_HARDDISK_CLEANUP`, "reboot to the dashboard and free
   up 2 blocks". DC3 was not relaunching to apply settings; it was rebooting
   to a dashboard *cleanup screen* we do not host, because its save-space
   check on `\Device\Harddisk0\partition1\` computed **zero free blocks**.
   Root cause: `NtQueryVolumeInformationFile` (class 3, the exact class DC3
   passes) reported DVD geometry — 2 KiB clusters. Titles convert clusters to
   16 KiB save blocks as `SectorsPerAllocationUnit * BytesPerSector / 16384`,
   which is 0 at 2 KiB clusters, so free = `Avail * 0 = 0`. Reporting real
   FATX geometry (512-byte sectors, 32 sectors/unit = 16 KiB clusters, 1 GiB
   free) clears the check: DC3 now passes its first boot with **no relaunch at
   all** and advances to drive-letter mounting.

   Two earlier misreads, corrected for the record: (1) `[0x2661BC, 0x2661C0)`
   is not a "relaunch gate" — it is `XLaunchNewImage`'s internal pre-launch
   *hook table* (an empty linker-registration section; a non-zero slot is
   **called as a function pointer**, which is why forcing it "past the gate"
   really made XLaunchNewImage call the launch page as code and fault at
   `0x8023_0000`). (2) The launch chain is: game code checks the volume →
   builds a 3072-byte `LAUNCH_DATA` on the stack (`[ebp-0xC00]`) →
   `XLaunchNewImage(NULL, &data)` at `0x1A633F` → worker `0x1AB046` → builder
   `0x1A6381` copies the data to page+`0x400` (`add esi,400h; rep movsd`) →
   reboot at `0x1A651B`. The ADR 0015 relaunch-with-persistence machinery is
   correct and stays (it is how a *real* settings-style relaunch will work);
   it simply no longer triggers on DC3's boot.

   After the geometry fix, landed in wall order: `IoCreateSymbolicLink` /
   `IoDeleteSymbolicLink` (drive-letter mounting through a link table the
   file device consults during resolution); the **writable hard-disk mount**
   (ADR 0016: partition1 → `%LOCALAPPDATA%\exbawks\hdd\<title-id>\`, with
   directory/file creation and `NtWriteFile` — DC3 creates its `TDATA` dir
   instead of reading the disk as full); and `RtlInitAnsiString` /
   `RtlEqualString`. Debug recipe that cracked the reboot wall, for reuse:
   dump the `LAUNCH_DATA_PAGE` **data area** (+`0x400`), not just the header
   — the dashboard reason code names the failing subsystem precisely (it is
   now a permanent `tracing::debug` in `relaunch_title`).

   Landed past the save bootstrap, in wall order: stack-top TLS (the XDK CRT
   contract — `_tls_index = -size/4`, block pointer at `[StackBase - size]`,
   decoded from DC3's own CRT at `0x1A9AE9`; the emulator reserves the region
   above the initial ESP, `CORE-003`); `FileNetworkOpenInformation`;
   `XeLoadSection`/`XeUnloadSection`; `NtOpenSymbolicLinkObject`/
   `NtQuerySymbolicLinkObject`; the x87 control subset
   (`fninit`/`fnstsw`/`fnstcw`/`fldcw`/`fnclex`); `KeQuerySystemTime`
   (tsc-derived deterministic clock) and `MmQueryStatistics`; real
   `NtCreateEvent`/`NtSetEvent`; and the ticking `KeTickCount` (KRN-003).

   **Then the big one: a 1000× interpreter speedup.** A "frozen" run
   (100%-CPU, no progress) was stack-sampled with cdb:
   `bump_physical_generation` walked all 2^20 page-table entries on **every
   guest write** (~3 ms each) to sync the descriptors' embedded generation
   stamps. The per-physical-page array is the authoritative store (ADR
   0005); dependencies are now captured from it too, the walk is deleted,
   and guest writes are O(1). Also: REP string ops now yield every 64 Ki
   iterations (interruptible, EIP stays put) so a giant memset cannot freeze
   the loop, and the run loop emits a heartbeat every 16 Mi blocks.
   Diagnosis recipe that cracked it, for reuse: bisect `--max-blocks` to
   find the stall, check the process is CPU-pegged vs blocked, then
   `cdb -pv -p <pid> -y target\debug -c ".reload /f; ~0 k; qd"` for the hot
   stack.

   **Current DC3 wall: SSE — the game proper.** The retail image now
   completes its ENTIRE kernel-facing initialization in **under one second**
   and stops at `movss xmm0, [esp+4]` (guest `0x1685A0`) — real game-engine
   float math. This is the CPU-tier frontier the WHP pivot (ADR 0013) exists
   for: implementing x87/SSE arithmetic in the interpreter is the long tail
   WHP gives us for free. The **WHP-M0 spike** (see the top of this section)
   is now clearly the highest-leverage next step; an interim
   SSE-data-movement subset (`movss` load/store/move is just 32-bit moves
   through `CpuState.xmm`) could probe a little further first but real
   arithmetic follows immediately. Graphics HLE (the screenshot command's
   long pole) remains after the CPU tier.

The retail image stays outside the repository; automated tests remain
synthetic.

Do not start runtime graphics work before memory operands translate. The
portable pure-logic graphics subset (`GPU-001` command vocabulary, `GPU-002`
pushbuffer parser, `GPU-005` software rasterizer) may proceed once the
`graphics-interception` ADR is accepted.

## First commands

```powershell
.\scripts\bootstrap.ps1
python scripts/static-validate.py
cargo xtask check
cargo exbawks doctor
cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe --json
```

`Cargo.lock` is committed. Keep it current with dependency changes.

## Engineering constraints

Keep guest addresses as typed values.

Keep HLE code independent from low identity mappings.

Track compiled code by physical page.

Keep Windows handles and views under single-owner RAII types.

Keep executable memory non-writable outside emission.

Keep complex work outside exception handlers.

Do not add proprietary Xbox data.

Do not treat malformed guest input as a Rust panic.

## Pull request contract

Use one task identifier in each pull request title.

Add tests for each new behavior and each corrected failure path.

Update the related design document when a contract changes.

Add one `CHANGELOG.md` entry for user-visible behavior.

Run this command before merge:

```powershell
cargo xtask check
```

## Stop conditions

Stop the change when one of these conditions occurs:

- The task requires a new architecture decision.
- A Windows API rule conflicts with an accepted ADR.
- A test needs copyrighted input.
- The public crate contract must break.
- The change expands beyond one task identifier.

Create or update an ADR before work continues.

## Definition of done for the first execution milestone

The milestone completed on August 15, 2026. Every condition passes:

- Windows maps two coherent guest aliases from one physical section.
- The direct backend emits the approved register-only subset.
- The dispatcher enters and leaves generated code under the ADR 0006 ABI.
- The synthetic XBE reaches its entry point.
- Patched kernel thunks call the registered `DbgPrint` and
  `HalReturnToFirmware` exports.
- Execution returns to translated code after each export.
- The runtime exits with `GuestExit { code: 0 }`.
- `cargo xtask check` passes on Windows 11 x86-64.
- No proprietary data exists in the repository.

Run the milestone: `cargo exbawks run .\fixtures\synthetic\minimal-retail.xbe`.

Use [the task board](task-board.md) for issue details and acceptance tests.
