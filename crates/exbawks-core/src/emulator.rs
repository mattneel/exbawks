use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use exbawks_cpu::{
    BasicBlockDecoder, CpuState, DecodeConfig, DecodedBlock, ExecError, classify_register_op,
    indirect_call_slot,
};
use exbawks_debug::{NoopTrace, TraceEvent, TraceSink};
use exbawks_gpu::{GraphicsFrontend, NullGraphicsBackend};
use exbawks_jit::{
    BlockExit, BlockKey, CodeCache, CodegenBackend, CompiledBlock, CraneliftBackend,
    DirectRewriteBackend, Dispatcher, PhysicalPageDependency,
};
use exbawks_kernel::{
    KernelCallContext, KernelRegistry, KernelStatus, gate_address, gate_ordinal,
    register_startup_exports,
};
use exbawks_memory::{GuestMemory, MemoryError, PageKind, SoftwareAddressSpace};
use exbawks_types::{
    BackendKind, GUEST_PAGE_SHIFT, GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions,
    StopReason,
};
use exbawks_xbe::{XbeImage, XbeSectionFlags};

use crate::threads::{PendingAction, THREAD_EXIT_SENTINEL, ThreadManager};
use crate::{BootPlanReport, CoreError, EmulatorConfig, KernelThunkTable, LoadedImage};

/// The outcome of one kernel gate call attempt at the current guest EIP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAssist {
    /// One export ran; execution resumes at the address.
    Dispatched {
        /// The dispatched export ordinal.
        ordinal: u16,
        /// The guest address after the call instruction.
        resume: GuestVa,
        /// The status the export returned.
        status: KernelStatus,
        /// A controlled stop the export requested.
        stop: Option<StopReason>,
    },
    /// A gate call named an ordinal without a registered export.
    MissingExport {
        /// The unregistered ordinal.
        ordinal: u16,
    },
    /// The current instruction is not a kernel gate call.
    NotAGateCall,
}

/// How the run loop treats the instruction at one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryClass {
    /// The direct backend translates the first instruction.
    Translatable,
    /// Decodable but untranslatable: probe the gate, then interpret.
    Assisted,
    /// Unfetchable or undecodable: the interpreter reports the typed fault.
    Interpreted,
}

/// Extracts the faulting guest address a memory error carries, when any.
fn memory_fault_address(error: &MemoryError) -> Option<GuestVa> {
    match error {
        MemoryError::Unmapped { address, .. }
        | MemoryError::AccessDenied { address, .. }
        | MemoryError::Mmio { address, .. }
        | MemoryError::AlreadyMapped { address } => Some(*address),
        _ => None,
    }
}

/// The guest stack range for the first synthetic thread.
const GUEST_STACK_BASE: GuestVa = GuestVa(0x03FF_0000);
/// The guest stack size in bytes.
const GUEST_STACK_BYTES: u32 = 64 * 1024;
/// The scratch bytes kept above the initial stack pointer.
const GUEST_STACK_SCRATCH: u32 = 16;

/// The base of the cached physical window (ADR 0010): guest physical
/// address `pa` is readable at `0x8000_0000 | pa`.
const KERNEL_WINDOW_BASE: u32 = 0x8000_0000;

/// One captured frame's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// The frame width in pixels.
    pub width: u32,
    /// The frame height in pixels.
    pub height: u32,
    /// Row-major 8-bit RGBA pixels, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
    /// The physical address the frame was scanned out of.
    pub frame_buffer: u32,
}

/// Why a frame capture found no image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// The title never programmed the video encoder.
    #[error("the title has not set a display mode")]
    NoDisplayMode,
    /// The surface format is not one this capture decodes.
    #[error("the display format {format:#x} is not a linear 32-bit surface")]
    UnsupportedFormat {
        /// The Xbox surface format code the title programmed.
        format: u32,
    },
    /// The scanline stride is zero or not a whole number of pixels.
    #[error("the display pitch {pitch} is not a whole number of 32-bit pixels")]
    UnsupportedPitch {
        /// The programmed stride in bytes.
        pitch: u32,
    },
    /// The frame buffer is not readable guest memory.
    #[error("the frame buffer at {address} is not readable")]
    Unreadable {
        /// The first unreadable scanline address.
        address: GuestVa,
    },
}

/// The guest address of the global descriptor table.
///
/// It sits on the second physical page, inside the low range the loader
/// reserves for the emulator's own structures (ADR 0010), addressed through
/// the cached physical window; guest-linear equals guest-physical with
/// paging off, so this is the address the processor loads descriptors from.
const GDT_WINDOW_VA: u32 = 0x8000_1000;
/// The descriptor table's byte size: null, code, data, and `fs` entries.
const GDT_BYTES: u16 = 4 * 8;
/// The selector naming the `fs` descriptor (index 3).
const GDT_FS_SELECTOR: u16 = 0x18;

/// A decoded and compiled plan for the current XBE entry block.
#[derive(Debug, Clone)]
pub struct EntryBlockPlan {
    /// The loaded image metadata and retained bytes.
    pub image: Arc<LoadedImage>,
    /// The decoded guest block.
    pub decoded: DecodedBlock,
    /// The backend artifact.
    pub compiled: Arc<CompiledBlock>,
}

impl EntryBlockPlan {
    /// Creates a serializable report.
    #[must_use]
    pub fn report(&self) -> BootPlanReport {
        BootPlanReport::from_plan(self)
    }
}

/// Constructs a configured emulator instance.
pub struct EmulatorBuilder {
    config: EmulatorConfig,
    trace: Arc<dyn TraceSink>,
}

impl EmulatorBuilder {
    /// Creates a builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the complete emulator configuration.
    #[must_use]
    pub fn config(mut self, config: EmulatorConfig) -> Self {
        self.config = config;
        self
    }

    /// Replaces the structured trace destination.
    #[must_use]
    pub fn trace(mut self, trace: Arc<dyn TraceSink>) -> Self {
        self.trace = trace;
        self
    }

    /// Validates settings and creates an emulator.
    pub fn build(self) -> Result<Emulator, CoreError> {
        validate_config(&self.config)?;
        let memory = Arc::new(SoftwareAddressSpace::new(self.config.physical_memory_bytes)?);
        let backend = make_backend(self.config.backend);

        let kernel = KernelRegistry::new();
        register_startup_exports(&kernel)
            .map_err(|_| CoreError::InvalidConfiguration("startup exports must register once"))?;

        Ok(Emulator {
            config: self.config,
            memory,
            cpu: CpuState::default(),
            kernel,
            graphics: GraphicsFrontend::new(NullGraphicsBackend::default()),
            code_cache: CodeCache::default(),
            backend,
            trace: self.trace,
            address_space_epoch: 0,
            loaded: None,
            threads: None,
            disc_root: None,
            hdd_root: None,
            devices: crate::mmio::DeviceSpace::default(),
            gpu_pusher: exbawks_gpu::PushbufferEngine::default(),
            pending_persist_reservations: Vec::new(),
            last_relaunch_data: None,
        })
    }
}

impl Default for EmulatorBuilder {
    fn default() -> Self {
        Self { config: EmulatorConfig::default(), trace: Arc::new(NoopTrace) }
    }
}

/// The root object for one Exbawks emulation session.
pub struct Emulator {
    config: EmulatorConfig,
    memory: Arc<SoftwareAddressSpace>,
    cpu: CpuState,
    kernel: KernelRegistry,
    graphics: GraphicsFrontend<NullGraphicsBackend>,
    code_cache: CodeCache,
    backend: Box<dyn CodegenBackend>,
    trace: Arc<dyn TraceSink>,
    address_space_epoch: u64,
    loaded: Option<Arc<LoadedImage>>,
    threads: Option<ThreadManager>,
    /// The host directory mounted as the read-only game disc (ADR 0014).
    disc_root: Option<PathBuf>,
    /// The host directory mounted as the writable hard disk (ADR 0016).
    hdd_root: Option<PathBuf>,
    /// The device model behind unmapped hardware register blocks (WHP-M2).
    devices: crate::mmio::DeviceSpace,
    /// The NV2A pushbuffer engine consuming submitted command streams
    /// (GPU-M0).
    gpu_pusher: exbawks_gpu::PushbufferEngine,
    /// Physical ranges (base, len) the next `load_xbe` must keep the
    /// allocator away from: soft-reboot persisted regions live at fixed
    /// window addresses, so their physical pages are reserved before the
    /// fresh boot allocates anything (ADR 0015 under the ADR 0010 window).
    pending_persist_reservations: Vec<(u32, u32)>,
    /// The persisted launch data of the previous relaunch (ADR 0015). A
    /// relaunch that persists identical data is a reboot loop, not progress,
    /// so the run stops instead of spinning.
    last_relaunch_data: Option<Vec<u8>>,
}

impl Emulator {
    /// Creates an emulator with default settings.
    pub fn new() -> Result<Self, CoreError> {
        EmulatorBuilder::new().build()
    }

    /// Returns the active configuration.
    #[must_use]
    pub const fn config(&self) -> &EmulatorConfig {
        &self.config
    }

    /// Returns the checked guest address space.
    #[must_use]
    pub fn memory(&self) -> &SoftwareAddressSpace {
        &self.memory
    }

    /// Mounts a host directory as the read-only game disc (ADR 0014).
    ///
    /// Set before [`Emulator::load_xbe`]; the mount backs the guest
    /// `Nt*File` exports. Guest file access is confined to this directory.
    pub fn set_disc_root(&mut self, root: PathBuf) {
        self.disc_root = Some(root);
    }

    /// Mounts a host directory as the writable hard-disk partition
    /// (ADR 0016).
    ///
    /// Set before [`Emulator::load_xbe`]. Titles create their save
    /// directories and files here; guest writes are confined to this
    /// directory.
    pub fn set_hdd_root(&mut self, root: PathBuf) {
        self.hdd_root = Some(root);
    }

    /// Returns the guest CPU state.
    #[must_use]
    pub const fn cpu(&self) -> &CpuState {
        &self.cpu
    }

    /// Returns mutable guest CPU state.
    pub const fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu
    }

    /// Returns the kernel HLE registry.
    #[must_use]
    pub const fn kernel(&self) -> &KernelRegistry {
        &self.kernel
    }

    /// Returns the null graphics frontend.
    pub const fn graphics_mut(&mut self) -> &mut GraphicsFrontend<NullGraphicsBackend> {
        &mut self.graphics
    }

    /// Returns the active XBE image.
    #[must_use]
    pub fn loaded_image(&self) -> Option<&Arc<LoadedImage>> {
        self.loaded.as_ref()
    }

    /// Returns the selected backend identifier.
    #[must_use]
    pub fn backend_kind(&self) -> BackendKind {
        self.backend.kind()
    }

    /// Parses and maps one XBE into a fresh software address space.
    pub fn load_xbe(&mut self, bytes: Vec<u8>) -> Result<Arc<LoadedImage>, CoreError> {
        if self.loaded.is_some() {
            return Err(CoreError::ImageAlreadyLoaded);
        }

        let bytes: Arc<[u8]> = bytes.into();
        let image = XbeImage::parse(&bytes)?;
        let memory = Arc::new(SoftwareAddressSpace::new(self.config.physical_memory_bytes)?);
        // The console's kernel-owned low physical memory (ADR 0010): page 0
        // is the scratch/zero page (Direct3D primes cache flushes by writing
        // through the window base), and the page at physical `0x10000` holds
        // the synthetic kernel image at its architectural window address.
        let low_reservation = memory
            .allocate_physical(KERNEL_IMAGE_BASE - 0x8000_0000 + GUEST_PAGE_SIZE)
            .map_err(|_| {
                CoreError::InvalidConfiguration(
                    "physical memory is too small for the kernel-owned low region",
                )
            })?;
        debug_assert_eq!(low_reservation.0, 0, "the low reservation must start at physical zero");
        // The cached physical window: every physical page is reachable at
        // `VA = 0x8000_0000 | PA`, the invariant Xbox software (and D3D's
        // GPU programming in particular) relies on.
        memory.map_physical_window()?;
        // Keep the fresh allocator away from soft-reboot persisted regions:
        // their window addresses are fixed, so their physical pages must
        // survive the new boot's allocations (ADR 0015).
        for (base, len) in self.pending_persist_reservations.drain(..) {
            memory.reserve_physical_through(exbawks_types::GuestPa(base.saturating_add(len)));
        }
        map_xbe(&memory, &image, &bytes)?;
        map_synthetic_kernel_image(&memory)?;

        let thunks = KernelThunkTable::read(
            memory.as_ref(),
            image.header.kernel_thunk_address,
            self.config.max_kernel_thunks,
        )?;

        // The kernel-side state (thread table, object handles, and the
        // bump-allocated kernel region) is built before thunk patching so
        // DATA-export slots can point at real kernel variables (ADR 0010).
        let mut threads =
            ThreadManager::new(memory.clone(), self.disc_root.clone(), self.hdd_root.clone());
        // The image's TLS directory (IMAGE_TLS_DIRECTORY at the header's TLS
        // address) drives each thread's TLS block; a missing or unreadable
        // directory means the image has no TLS.
        let tls_directory = image.header.tls_address;
        if tls_directory.0 != 0 {
            let field = |offset: u32| memory.read_u32(GuestVa(tls_directory.0 + offset));
            if let (Ok(raw_start), Ok(raw_end), Ok(zero_fill)) = (field(0), field(4), field(16)) {
                threads.set_tls_template(crate::threads::TlsTemplate {
                    raw_start,
                    raw_end,
                    zero_fill,
                });
            }
        }
        let data_ordinals: Vec<u16> = thunks
            .entries
            .iter()
            .filter(|thunk| {
                exbawks_kernel::kernel_ordinal_info(thunk.ordinal)
                    .is_some_and(|info| info.kind == exbawks_kernel::ExportKind::Data)
            })
            .map(|thunk| thunk.ordinal)
            .collect();
        let image_name = image_device_name(&image);
        let kernel_variables = threads.build_kernel_variables(&data_ordinals, &image_name)?;
        patch_kernel_thunks(&memory, &thunks, &kernel_variables)?;

        // The boot thread's stack is sized from the XBE header, page-rounded
        // and clamped to a working minimum (ADR 0010).
        let stack_bytes = image
            .header
            .stack_size
            .max(GUEST_STACK_BYTES)
            .div_ceil(GUEST_PAGE_SIZE)
            .saturating_mul(GUEST_PAGE_SIZE);
        let stack = GuestRange::page_aligned(GUEST_STACK_BASE, u64::from(stack_bytes))
            .map_err(MemoryError::from)?;
        memory.map_anonymous(stack, MemoryPermissions::READ | MemoryPermissions::WRITE)?;
        // The initial ESP sits below both the scratch words and the TLS
        // region the CRT claims at the top of the stack (ADR 0010).
        let stack_top = GUEST_STACK_BASE
            .0
            .saturating_add(stack_bytes)
            .saturating_sub(threads.tls_reserve_bytes())
            .saturating_sub(GUEST_STACK_SCRATCH);

        // Build the boot thread's KPCR/KTHREAD pages and wire the fs base so
        // XAPI startup can read its TIB and current-thread pointers.
        let kpcr = threads.create_boot_environment(GUEST_STACK_BASE, stack_bytes)?;

        self.cpu = CpuState { eip: image.header.entry_point.0, ..CpuState::default() };
        self.cpu.gpr[4] = stack_top;
        self.cpu.set_segment(
            exbawks_cpu::Segment::Fs,
            exbawks_cpu::SegmentState { base: kpcr.0, ..exbawks_cpu::SegmentState::default() },
        );
        self.memory = memory;
        self.threads = Some(threads);
        self.code_cache.clear();
        self.address_space_epoch = self.address_space_epoch.wrapping_add(1);

        let loaded = Arc::new(LoadedImage::new(image, bytes, thunks));
        self.loaded = Some(loaded.clone());
        Ok(loaded)
    }

    /// Attempts one kernel gate call at the current guest EIP.
    ///
    /// A dispatched call advances the guest EIP past the call instruction,
    /// so execution resumes in translated code.
    pub fn try_kernel_gate_call(&mut self) -> Result<GateAssist, CoreError> {
        let address = GuestVa(self.cpu.eip);
        let bytes = self.fetch_executable(address, 15)?;

        let decoder =
            BasicBlockDecoder::new(DecodeConfig { max_instructions: 1, max_bytes: bytes.len() });
        let block = decoder.decode(address, &bytes)?;
        let Some(instruction) = block.instructions.first() else {
            return Ok(GateAssist::NotAGateCall);
        };
        let Some(slot) = indirect_call_slot(instruction) else {
            return Ok(GateAssist::NotAGateCall);
        };

        let target = GuestVa(self.memory.read_u32(slot)?);
        let Some(ordinal) = gate_ordinal(target) else {
            return Ok(GateAssist::NotAGateCall);
        };
        let Some(export) = self.kernel.get(ordinal) else {
            return Ok(GateAssist::MissingExport { ordinal });
        };

        let resume = address
            .checked_add(u32::try_from(instruction.len()).unwrap_or(u32::MAX))
            .ok_or(CoreError::KernelThunkAddressOverflow { address })?;
        // Push the return address so the export sees the stdcall frame with
        // arguments above the return slot, then dispatch and return.
        let pushed_esp = self.cpu.gpr[4].wrapping_sub(4);
        self.memory.write_u32(GuestVa(pushed_esp), resume.0)?;
        self.cpu.gpr[4] = pushed_esp;
        self.dispatch_registered_export(ordinal, export, address)
    }

    /// Runs one registered export whose return address is already on the
    /// guest stack, then pops the stdcall frame and resumes at the return.
    ///
    /// The `call [slot]` fast path pushes the return address itself; a gate
    /// reached by EIP (`mov reg,[slot]; call reg`, or a tail `jmp [slot]`)
    /// already has it on the stack.
    fn dispatch_registered_export(
        &mut self,
        ordinal: u16,
        export: Arc<dyn exbawks_kernel::KernelExport>,
        caller: GuestVa,
    ) -> Result<GateAssist, CoreError> {
        tracing::trace!(ordinal, name = export.name(), "dispatching kernel gate call");
        // A named ordinal's call frame, for tracking a value (an HRESULT, a
        // handle) through guest code that never stores it to memory.
        if std::env::var("EXBAWKS_GATE_FRAME").is_ok_and(|value| {
            value.split(',').any(|wanted| wanted.trim().parse::<u16>() == Ok(ordinal))
        }) {
            let esp = self.cpu.gpr[4];
            let mut stack = String::new();
            for slot in 0..24_u32 {
                let value = self.memory.read_u32(GuestVa(esp + slot * 4)).unwrap_or(0);
                stack.push_str(&format!("{value:#010x} "));
            }
            tracing::info!(
                ordinal,
                name = export.name(),
                registers = format_args!("{:08x?}", self.cpu.gpr),
                %stack,
                "kernel gate frame"
            );
        }
        self.trace.record(TraceEvent::KernelCall {
            ordinal,
            name: exbawks_kernel::kernel_ordinal_info(ordinal).map(|info| info.name.to_owned()),
            caller,
        });
        let memory = self.memory.clone();

        let mut fallback = exbawks_kernel::UnsupportedServices;
        let services: &mut dyn exbawks_kernel::KernelServices = match self.threads.as_mut() {
            Some(threads) => threads,
            None => &mut fallback,
        };
        let mut context = KernelCallContext {
            cpu: &mut self.cpu,
            memory: memory.as_ref(),
            services,
            stop_request: None,
        };
        let status = export.call(&mut context);
        let stop = context.stop_request;

        // Pop the return address and the stdcall argument bytes the export
        // declares.
        let return_address = memory.read_u32(GuestVa(self.cpu.gpr[4]))?;
        self.cpu.gpr[4] =
            self.cpu.gpr[4].wrapping_add(4).wrapping_add(u32::from(export.stack_bytes()));
        self.cpu.eip = return_address;
        // Xbox kernel exports return their NTSTATUS in EAX; EAX is a
        // caller-clobbered register under the guest ABI.
        self.cpu.gpr[0] = status.0;

        Ok(GateAssist::Dispatched { ordinal, resume: GuestVa(return_address), status, stop })
    }

    /// Runs translated blocks from the current guest EIP.
    ///
    /// Execution continues until a controlled stop reason occurs or the
    /// block budget expires. A guest soft reboot that carries launch data
    /// relaunches the title in place (ADR 0015) rather than ending the run;
    /// `max_blocks` bounds each boot, not the whole relaunch chain.
    pub fn run(&mut self, max_blocks: usize) -> Result<StopReason, CoreError> {
        /// The most self-relaunches one run tolerates before ending, so a
        /// title whose launch data keeps changing still stops eventually.
        /// A stuck loop (identical launch data) is caught sooner, by content.
        const MAX_RELAUNCHES: u32 = 8;

        let mut relaunches = 0;
        loop {
            let stop = self.run_blocks(max_blocks)?;
            if let StopReason::Reboot { routine } = stop {
                if relaunches < MAX_RELAUNCHES && self.relaunch_title()? {
                    relaunches += 1;
                    tracing::info!(routine, relaunches, "soft reboot: relaunching title");
                    continue;
                }
                tracing::info!(routine, relaunches, "soft reboot: ending run (no relaunch)");
            }
            self.trace.record(TraceEvent::Stop { reason: format!("{stop:?}") });
            return Ok(stop);
        }
    }

    /// Relaunches the current title across a soft reboot (ADR 0015).
    ///
    /// Returns `true` when the title was relaunched (the guest set a
    /// `LaunchDataPage`), `false` when the reboot has no relaunch target and
    /// the run should end. The persisted regions and the `LaunchDataPage`
    /// pointer survive the machine reset; the same image reloads (only the
    /// self-relaunch, empty-launch-path form is handled).
    fn relaunch_title(&mut self) -> Result<bool, CoreError> {
        let Some(cell) = self.threads.as_ref().and_then(ThreadManager::launch_data_page_cell)
        else {
            return Ok(false);
        };
        let launch_pointer = self.memory.read_u32(cell)?;
        if launch_pointer == 0 {
            // A reboot with no launch data is a dashboard return, not a
            // self-relaunch.
            return Ok(false);
        }
        // The launch header and the first data words name the reboot's cause
        // precisely (an LDT_TO_DASHBOARD carries its reason code), which is
        // the single most useful diagnostic for a boot that bails.
        let mut head = [0_u8; 8];
        let mut data = [0_u8; 16];
        if self.memory.read(GuestVa(launch_pointer), &mut head).is_ok()
            && self.memory.read(GuestVa(launch_pointer.wrapping_add(0x400)), &mut data).is_ok()
        {
            let dw = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
            tracing::debug!(
                launch_type = format_args!("{:#x}", dw(&head, 0)),
                title = format_args!("{:#x}", dw(&head, 4)),
                data0 = format_args!("{:#x}", dw(&data, 0)),
                data1 = format_args!("{:#x}", dw(&data, 4)),
                data2 = format_args!("{:#x}", dw(&data, 8)),
                data3 = format_args!("{:#x}", dw(&data, 12)),
                "soft reboot launch data"
            );
        }

        // The most launch data one soft reboot preserves. The size is a raw
        // guest argument, so bounding it keeps a hostile persist from forcing
        // a huge host allocation (a launch-data page is one or two pages).
        const MAX_PERSIST_BYTES: u32 = 16 * 1024 * 1024;

        // Snapshot each persisted region's current bytes before the reset.
        // Skip a misaligned base — a real contiguous allocation is page
        // aligned — and cap the guest-controlled size.
        let regions =
            self.threads.as_ref().map(|t| t.persisted_regions().to_vec()).unwrap_or_default();
        let mut snapshots: Vec<(u32, Vec<u8>)> = Vec::new();
        for (base, size) in regions {
            if !base.is_multiple_of(GUEST_PAGE_SIZE) {
                continue;
            }
            let rounded = round_up_page(size).min(MAX_PERSIST_BYTES);
            let mut bytes = vec![0_u8; rounded as usize];
            if self.memory.read(GuestVa(base), &mut bytes).is_ok() {
                snapshots.push((base, bytes));
            }
        }

        // A relaunch that persists byte-identical launch data to the previous
        // one is a reboot loop (the title cannot satisfy some condition our
        // environment does not model, e.g. a display mode). Stop rather than
        // spin. The fingerprint is content only (region lengths and bytes),
        // not the base, which drifts as the kernel cursor advances each boot.
        let mut fingerprint = Vec::new();
        for (_, bytes) in &snapshots {
            fingerprint.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            fingerprint.extend_from_slice(bytes);
        }
        if !fingerprint.is_empty() && self.last_relaunch_data.as_ref() == Some(&fingerprint) {
            return Ok(false);
        }

        // Reload the same image (the empty launch path is a self-relaunch).
        // `reset` clears the loop-detection state, so it is set after restore.
        // Window-resident regions have fixed physical pages; recording them
        // here makes the fresh `load_xbe` reserve those pages before it
        // allocates anything.
        let window_len = self.memory.physical_len() as u32;
        let in_window = |base: u32, len: u32| {
            base >= 0x8000_0000 && (base - 0x8000_0000).saturating_add(len) <= window_len
        };
        self.pending_persist_reservations = snapshots
            .iter()
            .filter(|(base, bytes)| in_window(*base, bytes.len() as u32))
            .map(|(base, bytes)| (base & 0x1FFF_FFFF, bytes.len() as u32))
            .collect();
        let bytes = self.loaded.as_ref().ok_or(CoreError::NoImageLoaded)?.bytes().to_vec();
        self.reset()?;
        self.load_xbe(bytes)?;

        // Restore the persisted regions at their original (page-aligned)
        // addresses. A window address is always mapped (the window alias
        // covers all of physical RAM), so its bytes write straight back; a
        // non-window region is remapped first. Failure skips the region, not
        // the run.
        for (base, bytes) in &snapshots {
            if in_window(*base, bytes.len() as u32) {
                let _ = self.memory.write(GuestVa(*base), bytes);
                continue;
            }
            let Ok(range) = GuestRange::page_aligned(GuestVa(*base), bytes.len() as u64) else {
                continue;
            };
            if self
                .memory
                .map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
                .is_ok()
            {
                let _ = self.memory.write(GuestVa(*base), bytes);
            }
        }

        // Point the freshly rebuilt LaunchDataPage cell back at the page, so
        // the relaunched title reads its launch data.
        if let Some(new_cell) = self.threads.as_ref().and_then(ThreadManager::launch_data_page_cell)
        {
            let _ = self.memory.write_u32(new_cell, launch_pointer);
        }
        self.last_relaunch_data = Some(fingerprint);
        Ok(true)
    }

    /// Fetches executable bytes across contiguous mapped pages.
    ///
    /// The fetch spans as many mapped executable pages as the length allows,
    /// so an instruction straddling a page boundary decodes when both pages
    /// are mapped. It falls back to the current page when a later page is
    /// unmapped, and an instruction that straddles into an unmapped page
    /// then surfaces as a genuine fault through the decoder.
    fn fetch_executable(&self, start: GuestVa, max_len: usize) -> Result<Vec<u8>, CoreError> {
        let to_top = (u64::from(u32::MAX) - u64::from(start.0) + 1) as usize;
        let want = max_len.min(to_top);
        let mut bytes = vec![0_u8; want];
        if self.memory.fetch(start, &mut bytes).is_ok() {
            return Ok(bytes);
        }

        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - start.page_offset())
            .map_err(|_| CoreError::InvalidConfiguration("guest page size does not fit usize"))?;
        let fallback = want.min(page_remaining);
        bytes.truncate(fallback);
        self.memory.fetch(start, &mut bytes)?;
        Ok(bytes)
    }

    fn run_blocks(&mut self, max_blocks: usize) -> Result<StopReason, CoreError> {
        /// Executed blocks per KeTickCount millisecond (KRN-003): the
        /// deterministic virtual clock a title polls for pacing.
        const BLOCKS_PER_TICK: usize = 4096;

        if self.loaded.is_none() {
            return Err(CoreError::NoImageLoaded);
        }
        let tick_cell = self.threads.as_ref().and_then(ThreadManager::tick_count_cell);

        /// Blocks between progress heartbeats (tens of seconds of interpreter
        /// time), so a long or spinning run stays visible without spam.
        const BLOCKS_PER_HEARTBEAT: usize = 1 << 24;

        for executed in 0..max_blocks {
            if executed.is_multiple_of(BLOCKS_PER_TICK) {
                if let Some(cell) = tick_cell
                    && let Ok(ticks) = self.memory.read_u32(cell)
                {
                    let _ = self.memory.write_u32(cell, ticks.wrapping_add(1));
                }
                if executed > 0 && executed.is_multiple_of(BLOCKS_PER_HEARTBEAT) {
                    tracing::info!(
                        executed,
                        eip = format_args!("{:#010x}", self.cpu.eip),
                        tsc = self.cpu.tsc,
                        "run heartbeat"
                    );
                }
            }
            let start = GuestVa(self.cpu.eip);
            // A thread's start routine returned (ADR 0011): a created thread
            // returns to the exit sentinel; the boot thread, whose stack has
            // no set-up return address, returns to null. Either exits the
            // current thread with its EAX status and switches to the next
            // ready thread. A null reached by any other thread is a genuine
            // fault (a null call or corrupt return), not an exit.
            let null_exit = start == GuestVa(0)
                && self.threads.as_ref().is_some_and(ThreadManager::active_exits_on_null_return);
            if start == THREAD_EXIT_SENTINEL || null_exit {
                if let Some(stop) = self.exit_current_thread(self.cpu.gpr[0]) {
                    return Ok(stop);
                }
                continue;
            }
            // The guest jumped into the kernel gate region, so its EIP now
            // sits at a gate address. This is the `mov reg,[slot]; call reg`
            // (or tail `jmp [slot]`) form, where the transfer already ran and
            // the return address is on the stack — unlike the `call [slot]`
            // fast path, which `assist_at_current_eip` intercepts before the
            // call executes. Dispatch the ordinal from the EIP (CORE-004).
            if let Some(ordinal) = gate_ordinal(start) {
                if let Some(stop) = self.dispatch_gate_by_eip(ordinal, start)? {
                    return Ok(stop);
                }
                continue;
            }
            match self.classify_entry(start) {
                EntryClass::Translatable => {
                    let (_, compiled) = self.compiled_block_at(start)?;
                    let Some(emitted) = compiled.executable.as_ref() else {
                        return Ok(StopReason::RuntimeIncomplete);
                    };

                    let exit = Dispatcher.run(emitted, &mut self.cpu)?;
                    self.cpu.tsc =
                        self.cpu.tsc.wrapping_add(emitted.translated_instructions() as u64);
                    match exit {
                        BlockExit::DirectSuccessor => {}
                        BlockExit::UnsupportedInstruction => {
                            if let Some(stop) = self.assist_at_current_eip()? {
                                return Ok(stop);
                            }
                        }
                        _ => {
                            return Ok(StopReason::UnsupportedInstruction {
                                address: GuestVa(self.cpu.eip),
                            });
                        }
                    }
                }
                EntryClass::Assisted => {
                    if let Some(stop) = self.assist_at_current_eip()? {
                        return Ok(stop);
                    }
                }
                EntryClass::Interpreted => {
                    if let Some(stop) = self.interpret_step()? {
                        return Ok(stop);
                    }
                }
            }
        }

        Ok(StopReason::BudgetExhausted)
    }

    /// Decides how the run loop treats the instruction at one address.
    ///
    /// Only translatable entries reach the code cache, so untranslatable
    /// regions neither churn executable buffers nor pollute the cache with
    /// zero-coverage blocks.
    fn classify_entry(&self, start: GuestVa) -> EntryClass {
        let Ok(bytes) = self.fetch_executable(start, 15) else {
            return EntryClass::Interpreted;
        };
        let decoder = BasicBlockDecoder::new(DecodeConfig {
            max_instructions: 1,
            max_bytes: bytes.len().max(1),
        });
        let Ok(block) = decoder.decode(start, &bytes) else {
            return EntryClass::Interpreted;
        };
        match block.instructions.first() {
            Some(instruction) if classify_register_op(instruction).is_some() => {
                EntryClass::Translatable
            }
            Some(_) => EntryClass::Assisted,
            None => EntryClass::Interpreted,
        }
    }

    /// Probes the kernel gate at the current EIP, then falls back to one
    /// interpreter step.
    fn assist_at_current_eip(&mut self) -> Result<Option<StopReason>, CoreError> {
        match self.try_kernel_gate_call()? {
            GateAssist::Dispatched { stop: Some(stop), .. } => Ok(Some(stop)),
            // An unimplemented stub cannot correctly clean the stdcall
            // stack or return meaningful values, so halt rather than
            // continue past it with corrupted guest state.
            GateAssist::Dispatched { ordinal, status, .. }
                if status == KernelStatus::NOT_IMPLEMENTED =>
            {
                Ok(Some(StopReason::UnimplementedKernelExport { ordinal }))
            }
            GateAssist::Dispatched { .. } => Ok(self.apply_pending_scheduling()),
            GateAssist::MissingExport { ordinal } => {
                Ok(Some(StopReason::MissingKernelExport { ordinal }))
            }
            GateAssist::NotAGateCall => self.interpret_step(),
        }
    }

    /// Dispatches an export the guest reached by jumping into the gate region.
    ///
    /// The caller already pushed the return address (the transfer executed),
    /// so this runs the export against the existing stdcall frame rather than
    /// synthesizing one. A gate address with no registered ordinal is a
    /// missing export, not a fault, so the burn-down tooling names it.
    fn dispatch_gate_by_eip(
        &mut self,
        ordinal: u16,
        gate: GuestVa,
    ) -> Result<Option<StopReason>, CoreError> {
        let Some(export) = self.kernel.get(ordinal) else {
            return Ok(Some(StopReason::MissingKernelExport { ordinal }));
        };
        match self.dispatch_registered_export(ordinal, export, gate)? {
            GateAssist::Dispatched { stop: Some(stop), .. } => Ok(Some(stop)),
            GateAssist::Dispatched { ordinal, status, .. }
                if status == KernelStatus::NOT_IMPLEMENTED =>
            {
                Ok(Some(StopReason::UnimplementedKernelExport { ordinal }))
            }
            GateAssist::Dispatched { .. } => Ok(self.apply_pending_scheduling()),
            // `dispatch_registered_export` only ever reports `Dispatched`.
            GateAssist::MissingExport { ordinal } => {
                Ok(Some(StopReason::MissingKernelExport { ordinal }))
            }
            GateAssist::NotAGateCall => Ok(None),
        }
    }

    /// Applies the scheduling action the last kernel call recorded (ADR 0011).
    fn apply_pending_scheduling(&mut self) -> Option<StopReason> {
        let action = self.threads.as_mut().and_then(ThreadManager::take_pending)?;
        match action {
            PendingAction::Exit { status } => self.exit_current_thread(status),
            PendingAction::Wait { handle } => {
                let cpu = &mut self.cpu;
                let parked =
                    self.threads.as_mut().is_some_and(|threads| threads.park_active(handle, cpu));
                // The wait service only reports Pending when another thread
                // is runnable, so a failed park cannot strand the guest;
                // continuing on the caller is the safe fallback.
                if !parked {
                    tracing::warn!(handle, "a pending wait found no runnable thread");
                }
                None
            }
        }
    }

    /// Terminates the active thread and switches to the next ready one, or
    /// stops with a guest exit when none remain.
    fn exit_current_thread(&mut self, status: u32) -> Option<StopReason> {
        let cpu = &mut self.cpu;
        let switched = self.threads.as_mut().is_some_and(|threads| threads.exit_active(cpu));
        if switched { None } else { Some(StopReason::GuestExit { code: status }) }
    }

    /// Executes one instruction through the tier-0 interpreter (ADR 0008),
    /// converting typed interpreter failures into controlled stop reasons.
    fn interpret_step(&mut self) -> Result<Option<StopReason>, CoreError> {
        let memory = self.memory.clone();
        match exbawks_cpu::step(&mut self.cpu, memory.as_ref()) {
            Ok(()) => Ok(None),
            Err(ExecError::Unsupported { address } | ExecError::InvalidInstruction { address }) => {
                Ok(Some(StopReason::UnsupportedInstruction { address }))
            }
            Err(ExecError::Divide { address }) => Ok(Some(StopReason::GuestFault { address })),
            Err(ExecError::Memory(error)) => {
                let address = memory_fault_address(&error).unwrap_or(GuestVa(self.cpu.eip));
                Ok(Some(StopReason::GuestFault { address }))
            }
        }
    }

    /// Decodes and plans the current entry block.
    /// Captures the scanned-out frame as 8-bit RGBA pixels.
    ///
    /// The title programs the encoder with a **physical** frame-buffer
    /// address, which the cached physical window (ADR 0010) makes readable
    /// at `0x8000_0000 | address`. Only the linear 32-bit surface formats
    /// the SDTV modes use are decoded; anything else reports its format so
    /// the caller can say why no image came back.
    pub fn capture_frame(&self) -> Result<CapturedFrame, CaptureError> {
        /// `D3DFMT_LIN_A8R8G8B8`, the format every SDTV mode scans out.
        const LINEAR_A8R8G8B8: u32 = 0x12;
        /// `D3DFMT_LIN_X8R8G8B8`: the same bytes with the alpha ignored.
        const LINEAR_X8R8G8B8: u32 = 0x1E;
        /// The scanline count of the SDTV modes (480i/480p).
        const SDTV_HEIGHT: u32 = 480;

        let mode = self
            .threads
            .as_ref()
            .and_then(ThreadManager::display_mode)
            .ok_or(CaptureError::NoDisplayMode)?;
        if mode.format != LINEAR_A8R8G8B8 && mode.format != LINEAR_X8R8G8B8 {
            return Err(CaptureError::UnsupportedFormat { format: mode.format });
        }
        if mode.pitch == 0 || mode.pitch % 4 != 0 {
            return Err(CaptureError::UnsupportedPitch { pitch: mode.pitch });
        }

        let width = mode.pitch / 4;
        let height = SDTV_HEIGHT;
        let base = KERNEL_WINDOW_BASE | mode.frame_buffer;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        let mut scanline = vec![0_u8; mode.pitch as usize];
        for row in 0..height {
            let at = GuestVa(base.wrapping_add(row * mode.pitch));
            self.memory
                .read(at, &mut scanline)
                .map_err(|_| CaptureError::Unreadable { address: at })?;
            // The surface stores each pixel as little-endian ARGB, so the
            // bytes arrive blue, green, red, alpha.
            for pixel in scanline.chunks_exact(4) {
                pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xFF]);
            }
        }
        Ok(CapturedFrame { width, height, pixels, frame_buffer: mode.frame_buffer })
    }

    pub fn plan_entry_block(&self) -> Result<EntryBlockPlan, CoreError> {
        let image = self.loaded.clone().ok_or(CoreError::NoImageLoaded)?;
        let (decoded, compiled) = self.compiled_block_at(GuestVa(self.cpu.eip))?;
        Ok(EntryBlockPlan { image, decoded, compiled })
    }

    /// Decodes and compiles one block through the code cache.
    fn compiled_block_at(
        &self,
        start: GuestVa,
    ) -> Result<(DecodedBlock, Arc<CompiledBlock>), CoreError> {
        let bytes = self.fetch_executable(start, self.config.max_block_bytes)?;

        let decoder = BasicBlockDecoder::new(DecodeConfig {
            max_instructions: self.config.max_block_instructions,
            max_bytes: self.config.max_block_bytes,
        });
        let decoded = decoder.decode(start, &bytes)?;
        self.trace.record(TraceEvent::BlockEnter { address: start });

        let key = BlockKey {
            guest_start: start,
            address_space_epoch: self.address_space_epoch,
            backend: self.backend.kind(),
        };
        if let Some(compiled) = self.code_cache.get(key, self.memory.page_table()) {
            return Ok((decoded, compiled));
        }

        let dependencies = physical_dependencies(self.memory.page_table(), &decoded)?;
        let compiled = Arc::new(self.backend.compile(&decoded)?);
        self.code_cache.insert(key, compiled.clone(), dependencies);

        Ok((decoded, compiled))
    }

    /// Removes the active image and resets session state.
    pub fn reset(&mut self) -> Result<(), CoreError> {
        self.reset_inner()
    }
}

/// The Windows Hypervisor Platform execution tier (ADR 0013, WHP-M1).
///
/// The guest runs natively on one WHP virtual processor. The partition's
/// guest-physical mappings mirror the software page table (GPA = guest VA,
/// backed by the same physical buffer the HLE reads and writes, so the two
/// tiers stay coherent by construction). Kernel gates stay unmapped: a
/// `call [slot]` jumps to the gate address, the fetch exits with the gate
/// GPA, and the existing gate-by-EIP dispatch services it. A cancel pump
/// interrupts the processor every millisecond to advance the virtual clock.
#[cfg(all(windows, target_arch = "x86_64"))]
impl Emulator {
    /// Runs the guest on the WHP tier until a controlled stop reason.
    ///
    /// `max_exits` bounds serviced exits (cancellations excluded), the
    /// hypervisor-tier analogue of the interpreter's block budget.
    pub fn run_whp(&mut self, max_exits: usize) -> Result<StopReason, CoreError> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const MAX_RELAUNCHES: u32 = 8;

        if self.loaded.is_none() {
            return Err(CoreError::NoImageLoaded);
        }
        if !exbawks_whp::probe_whp().usable() {
            return Err(CoreError::Hypervisor(
                "the Windows Hypervisor Platform is not usable on this host".to_owned(),
            ));
        }

        let mut machine = self.whp_build_machine()?;
        let mut mapped_epoch = self.memory.mapping_epoch();

        // The cancel pump: kicks the processor out of `run` every
        // millisecond so the virtual clock advances even while the guest
        // spins without exiting. The guard stops and joins the pump before
        // the machine drops.
        struct PumpGuard {
            stop: Arc<AtomicBool>,
            handle: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for PumpGuard {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Release);
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
            }
        }
        let stop_flag = Arc::new(AtomicBool::new(false));
        let canceller = machine.canceller();
        let pump_stop = stop_flag.clone();
        let handle = std::thread::spawn(move || {
            while !pump_stop.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
                canceller.cancel();
            }
        });
        let _pump = PumpGuard { stop: stop_flag, handle: Some(handle) };

        let tick_cell = self.threads.as_ref().and_then(ThreadManager::tick_count_cell);
        let mut last_tick = std::time::Instant::now();
        // Advances both guest clocks by wall time: the KeTickCount cell
        // (milliseconds) and the virtual TSC that KeQuerySystemTime derives
        // from (100 ns units — frozen otherwise under WHP, where the
        // interpreter that normally advances it only runs for MMIO steps;
        // DirectSound's time-based startup waits forever on a frozen clock).
        fn advance_clock(
            memory: &SoftwareAddressSpace,
            cpu: &mut CpuState,
            tick_cell: Option<GuestVa>,
            last_tick: &mut std::time::Instant,
        ) {
            let elapsed = last_tick.elapsed();
            let ticks = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
            if ticks > 0 {
                if let Some(cell) = tick_cell
                    && let Ok(current) = memory.read_u32(cell)
                {
                    let _ = memory.write_u32(cell, current.wrapping_add(ticks));
                }
                cpu.tsc = cpu.tsc.wrapping_add(u64::from(ticks) * 10_000);
                *last_tick += std::time::Duration::from_millis(u64::from(ticks));
            }
        }

        let mut relaunches = 0_u32;
        let mut serviced = 0_usize;
        let mut cancels = 0_u64;
        // How many identified DSP mailbox pages are out of the partition.
        fn unmap_mailboxes(
            devices: &crate::mmio::DeviceSpace,
            machine: &mut exbawks_whp::Machine,
            applied: &mut usize,
            all: bool,
        ) -> Result<(), CoreError> {
            let pages = devices.mailbox_pages();
            let start = if all { 0 } else { *applied };
            for page in &pages[start.min(pages.len())..] {
                machine
                    .unmap_gpa(u64::from(*page), 0x1000)
                    .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
            }
            *applied = pages.len();
            Ok(())
        }
        let mut mailboxes_applied = 0_usize;
        unmap_mailboxes(&self.devices, &mut machine, &mut mailboxes_applied, true)?;
        // Alias the claimed GPU instance region at the PRAMIN window so the
        // guest's instance-memory traffic is plain RAM, not exits (ADR 0013
        // exit economics). Applied once the claim exists.
        let mut pramin_mapped = false;
        let map_pramin = |emulator_memory: &std::sync::Arc<SoftwareAddressSpace>,
                          devices: &crate::mmio::DeviceSpace,
                          threads: &Option<ThreadManager>,
                          machine: &mut exbawks_whp::Machine,
                          mapped: &mut bool|
         -> Result<(), CoreError> {
            if *mapped {
                return Ok(());
            }
            if let Some((base, size)) = threads.as_ref().and_then(ThreadManager::gpu_instance) {
                let physical = u64::from(base.0 & 0x1FFF_FFFF);
                let bytes = u64::from(size).div_ceil(4096) * 4096;
                devices.set_pramin(base.0, size);
                machine
                    .map_address_space(
                        emulator_memory,
                        physical,
                        0xFD70_0000,
                        bytes,
                        exbawks_whp::MapFlags::READ_WRITE,
                    )
                    .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
                tracing::debug!(
                    base = format_args!("{:#010x}", base.0),
                    size,
                    "PRAMIN aliased to the GPU instance claim"
                );
                *mapped = true;
            }
            Ok(())
        };
        map_pramin(&self.memory, &self.devices, &self.threads, &mut machine, &mut pramin_mapped)?;
        while serviced < max_exits {
            let exit = machine.run().map_err(|error| CoreError::Hypervisor(error.to_string()))?;
            let exit_rip = machine.exit_context().rip;
            match exit {
                exbawks_whp::WhpExit::Canceled => {
                    // The pump doubles as a sampling profiler: the exit RIP
                    // is wherever the guest was interrupted.
                    if tracing::enabled!(tracing::Level::TRACE) {
                        let sampled = machine
                            .get_registers(&[
                                exbawks_whp::Register::Rax,
                                exbawks_whp::Register::Rbx,
                                exbawks_whp::Register::Fs,
                            ])
                            .unwrap_or_default();
                        let rax = sampled.first().map(|value| value.low).unwrap_or(0);
                        let rbx = sampled.get(1).map(|value| value.low).unwrap_or(0);
                        let fs_base = sampled.get(2).map(|value| value.low).unwrap_or(0);
                        tracing::trace!(
                            rip = format_args!("{exit_rip:#010x}"),
                            rax = format_args!("{rax:#010x}"),
                            rbx = format_args!("{rbx:#010x}"),
                            fs = format_args!("{fs_base:#010x}"),
                            "whp cancel sample"
                        );
                    }
                    advance_clock(&self.memory, &mut self.cpu, tick_cell, &mut last_tick);
                    // Every few cancellations is a time slice (ADR 0017):
                    // rotate ready threads so a compute-only loop cannot
                    // starve the rest of the guest.
                    cancels += 1;
                    if cancels.is_multiple_of(4) {
                        self.whp_read_cpu(&mut machine)?;
                        let rotated = self
                            .threads
                            .as_mut()
                            .is_some_and(|threads| threads.rotate_active(&mut self.cpu));
                        if rotated {
                            self.whp_write_cpu(&mut machine)?;
                        }
                    }
                    continue;
                }
                exbawks_whp::WhpExit::MemoryAccess(access) => {
                    serviced += 1;
                    if serviced.is_multiple_of(1 << 16) {
                        tracing::info!(
                            serviced,
                            eip = format_args!("{exit_rip:#010x}"),
                            devices = %self.devices.summary(),
                            "whp heartbeat"
                        );
                    }
                    let gpa = u32::try_from(access.gpa).unwrap_or(u32::MAX);
                    // An execute fault's GPA is page-aligned, losing the
                    // slot offset; the exit RIP carries the exact gate
                    // address the call jumped to.
                    let fault_va = if access.access_type == 2 {
                        u32::try_from(exit_rip).unwrap_or(u32::MAX)
                    } else {
                        gpa
                    };
                    // A thread's start routine returned (ADR 0011): created
                    // threads return to the exit sentinel; the boot thread
                    // returns to null. Either exits the thread and switches.
                    if access.access_type == 2 {
                        let null_exit = fault_va == 0
                            && self
                                .threads
                                .as_ref()
                                .is_some_and(ThreadManager::active_exits_on_null_return);
                        if fault_va == THREAD_EXIT_SENTINEL.0 || null_exit {
                            self.whp_read_cpu(&mut machine)?;
                            // The exiting thread's stack residue: the return
                            // addresses just popped name the unwind path.
                            if tracing::enabled!(tracing::Level::DEBUG) {
                                let esp = self.cpu.gpr[4];
                                let mut stack = String::new();
                                // Below the pointer: frames the return path
                                // already popped (most recent last).
                                for slot in (1..=24_u32).rev() {
                                    let at = esp.wrapping_sub(slot * 4);
                                    let value = self.memory.read_u32(GuestVa(at)).unwrap_or(0);
                                    stack.push_str(&format!("{value:#010x} "));
                                }
                                tracing::debug!(
                                    esp = format_args!("{esp:#010x}"),
                                    eax = format_args!("{:#010x}", self.cpu.gpr[0]),
                                    %stack,
                                    "thread exit"
                                );
                            }
                            if let Some(stop) = self.exit_current_thread(self.cpu.gpr[0]) {
                                return Ok(stop);
                            }
                            self.whp_write_cpu(&mut machine)?;
                            continue;
                        }
                    }

                    // A data access to a hardware register block (or the
                    // software DSP mailbox page): execute exactly one
                    // instruction on the interpreter over the device view,
                    // so the access's own semantics come from the oracle
                    // rather than a hand decoder (WHP-M2).
                    if access.access_type != 2 && self.devices.routes(gpa) {
                        self.whp_read_cpu(&mut machine)?;
                        let memory = self.memory.clone();
                        let view = crate::mmio::MmioView::new(&memory, &self.devices);
                        match exbawks_cpu::step(&mut self.cpu, &view) {
                            Ok(()) => {
                                // A device write may have just identified a
                                // DSP mailbox page; take it out of the
                                // partition so its traffic routes here.
                                unmap_mailboxes(
                                    &self.devices,
                                    &mut machine,
                                    &mut mailboxes_applied,
                                    false,
                                )?;
                                // A DMA_PUT write queued a pushbuffer range:
                                // walk it now, with the vCPU parked, so the
                                // GET readback truthfully reports the work
                                // consumed (GPU-M0).
                                self.consume_gpu_submissions();
                                self.whp_write_cpu(&mut machine)?;
                                continue;
                            }
                            Err(
                                ExecError::Unsupported { address }
                                | ExecError::InvalidInstruction { address },
                            ) => {
                                return Ok(StopReason::UnsupportedInstruction { address });
                            }
                            Err(ExecError::Divide { address }) => {
                                return Ok(StopReason::GuestFault { address });
                            }
                            Err(ExecError::Memory(error)) => {
                                tracing::debug!(
                                    eip = format_args!("{:#010x}", self.cpu.eip),
                                    gpa = format_args!("{gpa:#010x}"),
                                    %error,
                                    "the device step faulted"
                                );
                                let address =
                                    memory_fault_address(&error).unwrap_or(GuestVa(self.cpu.eip));
                                return Ok(StopReason::GuestFault { address });
                            }
                        }
                    }

                    let gate = gate_ordinal(GuestVa(fault_va)).filter(|_| access.access_type == 2);
                    let Some(ordinal) = gate else {
                        self.whp_read_cpu(&mut machine)?;
                        // The faulting frame's stack top: return addresses and
                        // spilled arguments identify the caller without a
                        // debugger attached.
                        let esp = self.cpu.gpr[4];
                        let mut stack = String::new();
                        for slot in 0..10_u32 {
                            let value = self.memory.read_u32(GuestVa(esp + slot * 4)).unwrap_or(0);
                            stack.push_str(&format!("{value:#010x} "));
                        }
                        let fs_base = self.cpu.segments[exbawks_cpu::Segment::Fs as usize].base;
                        tracing::debug!(
                            esp = format_args!("{esp:#010x}"),
                            fs = format_args!("{fs_base:#010x}"),
                            ebx = format_args!("{:#010x}", self.cpu.gpr[3]),
                            ebp = format_args!("{:#010x}", self.cpu.gpr[5]),
                            %stack,
                            "fault stack"
                        );
                        tracing::debug!(
                            gpa = format_args!("{:#010x}", access.gpa),
                            gva = format_args!("{:#010x}", access.gva),
                            access_type = access.access_type,
                            gpa_unmapped = access.gpa_unmapped,
                            gva_valid = access.gva_valid,
                            "unhandled memory-access exit"
                        );
                        if let Some(threads) = &self.threads {
                            tracing::debug!(
                                threads = %threads.describe_threads(),
                                "thread states at fault"
                            );
                        }
                        return Ok(StopReason::GuestFault { address: GuestVa(fault_va) });
                    };

                    // The call already pushed its return address and jumped;
                    // the fetch at the gate faulted, exactly the gate-by-EIP
                    // shape (CORE-004).
                    self.whp_read_cpu(&mut machine)?;
                    self.cpu.eip = fault_va;
                    advance_clock(&self.memory, &mut self.cpu, tick_cell, &mut last_tick);
                    if let Some(stop) = self.dispatch_gate_by_eip(ordinal, GuestVa(fault_va))? {
                        if let StopReason::Reboot { .. } = stop
                            && relaunches < MAX_RELAUNCHES
                            && self.relaunch_title()?
                        {
                            relaunches += 1;
                            tracing::info!(relaunches, "whp: soft reboot, relaunching title");
                            machine = self.whp_build_machine()?;
                            mapped_epoch = self.memory.mapping_epoch();
                            unmap_mailboxes(
                                &self.devices,
                                &mut machine,
                                &mut mailboxes_applied,
                                true,
                            )?;
                            continue;
                        }
                        return Ok(stop);
                    }
                    if self.memory.mapping_epoch() != mapped_epoch {
                        self.whp_sync_mappings(&mut machine)?;
                        mapped_epoch = self.memory.mapping_epoch();
                        // A resync remaps everything; every mailbox page
                        // must come back out.
                        unmap_mailboxes(&self.devices, &mut machine, &mut mailboxes_applied, true)?;
                    }
                    map_pramin(
                        &self.memory,
                        &self.devices,
                        &self.threads,
                        &mut machine,
                        &mut pramin_mapped,
                    )?;
                    self.whp_write_cpu(&mut machine)?;
                }
                exbawks_whp::WhpExit::IoPort(io) => {
                    serviced += 1;
                    if io.string_op {
                        return Err(CoreError::Hypervisor(format!(
                            "unhandled string port I/O at rip {exit_rip:#x}"
                        )));
                    }
                    let width_mask: u64 = match io.access_size {
                        1 => 0xFF,
                        2 => 0xFFFF,
                        _ => 0xFFFF_FFFF,
                    };
                    let length = u64::from(machine.exit_context().instruction_length());
                    let next_rip = (
                        exbawks_whp::Register::Rip,
                        exbawks_whp::RegisterValue::scalar(exit_rip.wrapping_add(length)),
                    );
                    let writes = if io.is_write {
                        self.devices.port_write(io.port, (io.rax & width_mask) as u32);
                        vec![next_rip]
                    } else {
                        let value = u64::from(self.devices.port_read(io.port)) & width_mask;
                        let rax = (io.rax & !width_mask) | value;
                        vec![
                            next_rip,
                            (exbawks_whp::Register::Rax, exbawks_whp::RegisterValue::scalar(rax)),
                        ]
                    };
                    machine
                        .set_registers(&writes)
                        .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
                    continue;
                }
                exbawks_whp::WhpExit::Exception(exception) => {
                    self.whp_read_cpu(&mut machine)?;
                    let rip = u32::try_from(exit_rip).unwrap_or(u32::MAX);
                    tracing::debug!(
                        vector = exception.vector,
                        parameter = format_args!("{:#x}", exception.parameter),
                        rip = format_args!("{rip:#010x}"),
                        "guest exception exit"
                    );
                    return Ok(match exception.vector {
                        // #UD: the instruction itself is the story.
                        6 => StopReason::UnsupportedInstruction { address: GuestVa(rip) },
                        // #PF: the faulting address is in the parameter.
                        14 => StopReason::GuestFault {
                            address: GuestVa(
                                u32::try_from(exception.parameter).unwrap_or(u32::MAX),
                            ),
                        },
                        _ => StopReason::GuestFault { address: GuestVa(rip) },
                    });
                }
                exbawks_whp::WhpExit::Halt => {
                    self.whp_read_cpu(&mut machine)?;
                    return Ok(StopReason::GuestExit { code: self.cpu.gpr[0] });
                }
                exbawks_whp::WhpExit::InvalidRegisterValue => {
                    return Err(CoreError::Hypervisor(format!(
                        "the processor rejected the register state at rip {exit_rip:#x}"
                    )));
                }
                exbawks_whp::WhpExit::UnrecoverableException => {
                    self.whp_read_cpu(&mut machine)?;
                    tracing::debug!(
                        rip = format_args!("{:#010x}", self.cpu.eip),
                        esp = format_args!("{:#010x}", self.cpu.gpr[4]),
                        "unrecoverable guest exception"
                    );
                    return Ok(StopReason::GuestFault {
                        address: GuestVa(u32::try_from(exit_rip).unwrap_or(u32::MAX)),
                    });
                }
                exbawks_whp::WhpExit::Other(reason) => {
                    return Err(CoreError::Hypervisor(format!(
                        "unhandled exit reason {reason:#x} at rip {exit_rip:#x}"
                    )));
                }
            }
        }
        Ok(StopReason::BudgetExhausted)
    }

    /// Creates a partition mirroring the current guest state.
    fn whp_build_machine(&mut self) -> Result<exbawks_whp::Machine, CoreError> {
        let mut machine = exbawks_whp::Machine::new()
            .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
        self.whp_sync_mappings(&mut machine)?;
        machine
            .set_boot_state_32(self.cpu.eip, self.cpu.gpr[4])
            .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
        self.write_descriptor_table();
        machine
            .set_gdt(GDT_WINDOW_VA, GDT_BYTES - 1)
            .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
        self.whp_write_cpu(&mut machine)?;
        Ok(machine)
    }

    /// Writes the guest's global descriptor table.
    ///
    /// Four entries: the null descriptor, the flat code and data segments
    /// the boot state loads, and the `fs` descriptor carrying the running
    /// thread's KPCR base — so a guest that reloads `fs` from its selector
    /// keeps reaching its own processor block.
    fn write_descriptor_table(&self) {
        /// A present, 32-bit, page-granular descriptor's high dword for a
        /// base of zero: limit 0xFFFFF with the type in bits 8..12.
        fn descriptor(base: u32, limit: u32, access: u32) -> (u32, u32) {
            let low = (limit & 0xFFFF) | (base << 16);
            let high = ((base >> 16) & 0xFF)
                | (access << 8)
                | (limit & 0x000F_0000)
                | 0x00C0_0000
                | (base & 0xFF00_0000);
            (low, high)
        }
        /// Present, ring 0, code segment: execute/read.
        const CODE_ACCESS: u32 = 0x9B;
        /// Present, ring 0, data segment: read/write.
        const DATA_ACCESS: u32 = 0x93;

        let fs_base = self.cpu.segments[exbawks_cpu::Segment::Fs as usize].base;
        let entries = [
            (0, 0),
            descriptor(0, 0xF_FFFF, CODE_ACCESS),
            descriptor(0, 0xF_FFFF, DATA_ACCESS),
            descriptor(fs_base, 0xF_FFFF, DATA_ACCESS),
        ];
        for (index, (low, high)) in entries.iter().enumerate() {
            let at = GDT_WINDOW_VA + index as u32 * 8;
            let _ = self.memory.write_u32(GuestVa(at), *low);
            let _ = self.memory.write_u32(GuestVa(at + 4), *high);
        }
    }

    /// Mirrors the software page table into the partition's GPA space.
    ///
    /// Guest-linear equals guest-physical in the partition (unpaged flat
    /// mode), so each mapped virtual page's GPA maps to its backing
    /// physical page's host bytes; contiguous same-permission runs merge
    /// into one platform call. Gate and unmapped pages stay unmapped, which
    /// is what makes them exit.
    fn whp_sync_mappings(&self, machine: &mut exbawks_whp::Machine) -> Result<(), CoreError> {
        use exbawks_types::{GUEST_PAGE_COUNT, GuestPage};

        let table = self.memory.page_table();
        let page_size = u64::from(GUEST_PAGE_SIZE);
        let mut page = 0_usize;
        while page < GUEST_PAGE_COUNT {
            let descriptor = table.get(GuestPage(page as u32));
            if descriptor.kind() != PageKind::Ram {
                page += 1;
                continue;
            }
            let run_va = page as u64;
            let run_pa = u64::from(descriptor.physical_page().0);
            let permissions = descriptor.permissions();
            let mut length = 1_u64;
            while page + (length as usize) < GUEST_PAGE_COUNT {
                let next = table.get(GuestPage((run_va + length) as u32));
                if next.kind() != PageKind::Ram
                    || u64::from(next.physical_page().0) != run_pa + length
                    || next.permissions() != permissions
                {
                    break;
                }
                length += 1;
            }

            let mut flags = 1_u32; // Mapped pages are always readable.
            if permissions.contains(MemoryPermissions::WRITE) {
                flags |= 2;
            }
            if permissions.contains(MemoryPermissions::EXECUTE) {
                flags |= 4;
            }
            tracing::trace!(
                gpa = format_args!("{:#010x}", run_va * page_size),
                pa = format_args!("{:#010x}", run_pa * page_size),
                bytes = length * page_size,
                flags,
                "partition map"
            );
            machine
                .map_address_space(
                    &self.memory,
                    run_pa * page_size,
                    run_va * page_size,
                    length * page_size,
                    exbawks_whp::MapFlags(flags),
                )
                .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
            page += length as usize;
        }
        Ok(())
    }

    /// Writes the guest CPU state into the virtual processor.
    fn whp_write_cpu(&self, machine: &mut exbawks_whp::Machine) -> Result<(), CoreError> {
        use exbawks_whp::{Register, RegisterValue};

        let fs_base = u64::from(self.cpu.segments[exbawks_cpu::Segment::Fs as usize].base);
        // The descriptor the `fs` selector names must agree with the base
        // loaded here, or a guest segment reload would lose the KPCR.
        self.write_descriptor_table();
        machine
            .set_registers(&[
                (Register::Rax, RegisterValue::scalar(u64::from(self.cpu.gpr[0]))),
                (Register::Rcx, RegisterValue::scalar(u64::from(self.cpu.gpr[1]))),
                (Register::Rdx, RegisterValue::scalar(u64::from(self.cpu.gpr[2]))),
                (Register::Rbx, RegisterValue::scalar(u64::from(self.cpu.gpr[3]))),
                (Register::Rsp, RegisterValue::scalar(u64::from(self.cpu.gpr[4]))),
                (Register::Rbp, RegisterValue::scalar(u64::from(self.cpu.gpr[5]))),
                (Register::Rsi, RegisterValue::scalar(u64::from(self.cpu.gpr[6]))),
                (Register::Rdi, RegisterValue::scalar(u64::from(self.cpu.gpr[7]))),
                (Register::Rip, RegisterValue::scalar(u64::from(self.cpu.eip))),
                (Register::Rflags, RegisterValue::scalar(u64::from(self.cpu.eflags | 0x2))),
                // The flat data segment with the thread's KPCR base.
                (
                    Register::Fs,
                    RegisterValue::segment(fs_base, 0xFFFF_FFFF, GDT_FS_SELECTOR, 0xC093),
                ),
            ])
            .map_err(|error| CoreError::Hypervisor(error.to_string()))
    }

    /// Reads the virtual processor's state back into the guest CPU state.
    fn whp_read_cpu(&mut self, machine: &mut exbawks_whp::Machine) -> Result<(), CoreError> {
        use exbawks_whp::Register;

        const NAMES: [Register; 10] = [
            Register::Rax,
            Register::Rcx,
            Register::Rdx,
            Register::Rbx,
            Register::Rsp,
            Register::Rbp,
            Register::Rsi,
            Register::Rdi,
            Register::Rip,
            Register::Rflags,
        ];
        let values = machine
            .get_registers(&NAMES)
            .map_err(|error| CoreError::Hypervisor(error.to_string()))?;
        for (index, value) in values.iter().take(8).enumerate() {
            self.cpu.gpr[index] = value.low as u32;
        }
        self.cpu.eip = values[8].low as u32;
        self.cpu.eflags = values[9].low as u32;
        Ok(())
    }
}

impl Emulator {
    /// Walks any pushbuffer ranges submitted since the last call (GPU-M0).
    #[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
    fn consume_gpu_submissions(&mut self) {
        /// Guest physical memory through the cached window (ADR 0010).
        struct WindowMemory<'a>(&'a SoftwareAddressSpace);
        impl exbawks_gpu::Nv2aMemory for WindowMemory<'_> {
            fn read_dword(&self, physical: u32) -> Option<u32> {
                if physical >= 0x2000_0000 {
                    return None;
                }
                self.0.read_u32(GuestVa(0x8000_0000 | physical)).ok()
            }

            fn write_dword(&self, physical: u32, value: u32) -> bool {
                physical < 0x2000_0000
                    && self.0.write_u32(GuestVa(0x8000_0000 | physical), value).is_ok()
            }
        }

        /// `NV_PFIFO_RAMHT`: the object hash table's location register.
        const PFIFO_RAMHT: u32 = 0xFD00_2210;

        let submissions = self.devices.take_submissions();
        if submissions.is_empty() {
            return;
        }
        let ramht_raw = self.devices.register_value(PFIFO_RAMHT).unwrap_or(0);
        let pramin =
            self.devices.pramin_region().map(|(base_va, _)| base_va & 0x1FFF_FFFF).unwrap_or(0);
        let memory = self.memory.clone();
        let view = WindowMemory(&memory);
        for (channel, get, put) in submissions {
            let end = self.gpu_pusher.submit(&view, channel, pramin, ramht_raw, get, put);
            let stats = self.gpu_pusher.stats();
            tracing::debug!(
                channel = format_args!("{channel:#010x}"),
                end = format_args!("{end:#010x}"),
                methods = stats.method_dwords,
                releases = stats.semaphore_releases,
                aborted = stats.aborted,
                "pushbuffer walked"
            );
        }
    }

    fn reset_inner(&mut self) -> Result<(), CoreError> {
        self.memory = Arc::new(SoftwareAddressSpace::new(self.config.physical_memory_bytes)?);
        self.cpu = CpuState::default();
        self.loaded = None;
        self.threads = None;
        self.code_cache.clear();
        self.address_space_epoch = self.address_space_epoch.wrapping_add(1);
        // The soft-reboot loop detector is per image; a fresh load starts
        // clean. `relaunch_title` re-sets it after its own reset (ADR 0015).
        self.last_relaunch_data = None;
        Ok(())
    }
}

/// Rounds a byte count up to a whole number of guest pages (at least one).
fn round_up_page(size: u32) -> u32 {
    size.max(1).div_ceil(GUEST_PAGE_SIZE).saturating_mul(GUEST_PAGE_SIZE)
}

fn validate_config(config: &EmulatorConfig) -> Result<(), CoreError> {
    if !config.physical_memory_is_aligned() {
        return Err(CoreError::InvalidConfiguration(
            "physical_memory_bytes must use nonzero 4 KiB pages",
        ));
    }
    if config.max_block_instructions == 0 {
        return Err(CoreError::InvalidConfiguration("max_block_instructions must not be zero"));
    }
    if config.max_block_bytes == 0 {
        return Err(CoreError::InvalidConfiguration("max_block_bytes must not be zero"));
    }
    if config.max_kernel_thunks == 0 {
        return Err(CoreError::InvalidConfiguration("max_kernel_thunks must not be zero"));
    }
    Ok(())
}

fn make_backend(kind: BackendKind) -> Box<dyn CodegenBackend> {
    match kind {
        BackendKind::DirectRewrite => Box::new(DirectRewriteBackend::default()),
        BackendKind::Cranelift => Box::new(CraneliftBackend),
    }
}

/// One byte extent to copy into guest RAM, with its permission contribution.
///
/// Headers are a read-only pseudo-section; every real section becomes one
/// extent. See ADR 0007.
struct ImageExtent<'a> {
    start: u32,
    virtual_size: u32,
    data: &'a [u8],
    permissions: MemoryPermissions,
    head_page_read_only: bool,
    tail_page_read_only: bool,
    /// The section index, or `None` for the header pseudo-section.
    section_index: Option<u32>,
}

impl ImageExtent<'_> {
    /// Returns the inclusive first and last covered guest pages.
    ///
    /// The last byte is computed in 64 bits so a malformed extent can never
    /// overflow here; `image_extents` has already rejected any extent whose
    /// range leaves the guest space.
    fn page_bounds(&self) -> (u32, u32) {
        let last_byte = u64::from(self.start) + u64::from(self.virtual_size) - 1;
        (self.start >> GUEST_PAGE_SHIFT, (last_byte >> GUEST_PAGE_SHIFT) as u32)
    }

    /// Returns this extent's write contribution to one guest page.
    fn writes_page(&self, page: u32) -> bool {
        if !self.permissions.contains(MemoryPermissions::WRITE) {
            return false;
        }
        let (head, tail) = self.page_bounds();
        if page == head && self.head_page_read_only {
            return false;
        }
        if page == tail && self.tail_page_read_only {
            return false;
        }
        true
    }
}

fn map_xbe(memory: &SoftwareAddressSpace, image: &XbeImage, bytes: &[u8]) -> Result<(), CoreError> {
    let extents = image_extents(image, bytes)?;
    if extents.is_empty() {
        return Ok(());
    }

    map_covered_pages(memory, &extents)?;

    // Copy header and section bytes to their exact guest addresses while the
    // pages are writable.
    for extent in &extents {
        if extent.data.is_empty() {
            continue;
        }
        memory.write(GuestVa(extent.start), extent.data)?;
    }

    apply_merged_permissions(memory, &extents)
}

/// Builds the header pseudo-section and every non-empty section as extents.
///
/// Every extent is validated to lie inside the declared image window
/// `[base, base + size_of_image)`, which must itself fit the 32-bit guest
/// space. That single bound makes all later page and overlap arithmetic
/// overflow-free (ADR 0007).
fn image_extents<'a>(image: &XbeImage, bytes: &'a [u8]) -> Result<Vec<ImageExtent<'a>>, CoreError> {
    let base = u64::from(image.header.base_address.0);
    let image_end = base + u64::from(image.header.size_of_image);
    if image_end > u64::from(u32::MAX) + 1 {
        return Err(CoreError::ImageLeavesGuestSpace { size_of_image: image.header.size_of_image });
    }

    let mut extents = Vec::with_capacity(image.sections.len() + 1);

    if image.header.size_of_headers != 0 {
        require_within_image(
            image.header.base_address,
            image.header.size_of_headers,
            base,
            image_end,
            None,
        )?;
        let header_size = usize::try_from(image.header.size_of_headers)
            .map_err(|_| CoreError::InvalidConfiguration("XBE header size does not fit usize"))?;
        let data = bytes
            .get(..header_size)
            .ok_or(CoreError::InvalidConfiguration("XBE header size exceeds the file"))?;
        extents.push(ImageExtent {
            start: image.header.base_address.0,
            virtual_size: image.header.size_of_headers,
            data,
            permissions: MemoryPermissions::READ,
            head_page_read_only: false,
            tail_page_read_only: false,
            section_index: None,
        });
    }

    for section in &image.sections {
        if section.virtual_size == 0 {
            continue;
        }
        if section.raw_size > section.virtual_size {
            return Err(CoreError::SectionRawExceedsVirtual {
                section_index: section.index,
                raw_size: section.raw_size,
                virtual_size: section.virtual_size,
            });
        }
        require_within_image(
            section.virtual_address,
            section.virtual_size,
            base,
            image_end,
            Some(section.index),
        )?;

        let mut permissions = MemoryPermissions::READ;
        if section.flags.contains(XbeSectionFlags::WRITABLE) {
            permissions |= MemoryPermissions::WRITE;
        }
        if section.flags.contains(XbeSectionFlags::EXECUTABLE) {
            permissions |= MemoryPermissions::EXECUTE;
        }

        extents.push(ImageExtent {
            start: section.virtual_address.0,
            virtual_size: section.virtual_size,
            data: image.section_data(bytes, section)?,
            permissions,
            head_page_read_only: section.flags.contains(XbeSectionFlags::HEAD_PAGE_READ_ONLY),
            tail_page_read_only: section.flags.contains(XbeSectionFlags::TAIL_PAGE_READ_ONLY),
            section_index: Some(section.index),
        });
    }

    reject_byte_overlap(&extents)?;
    Ok(extents)
}

/// Rejects an extent whose byte range leaves the declared image window.
///
/// `section_index` is `None` for the header pseudo-section.
fn require_within_image(
    start: GuestVa,
    virtual_size: u32,
    base: u64,
    image_end: u64,
    section_index: Option<u32>,
) -> Result<(), CoreError> {
    let start_u64 = u64::from(start.0);
    let end = start_u64 + u64::from(virtual_size);
    if start_u64 < base || end > image_end {
        return Err(CoreError::RangeOutsideImage { section_index, address: start });
    }
    Ok(())
}

/// Rejects extents whose byte ranges overlap beyond a shared page boundary.
///
/// Real sections share the boundary page but never the same bytes. The end
/// is computed in 64 bits; extents are already known to fit the guest space.
fn reject_byte_overlap(extents: &[ImageExtent<'_>]) -> Result<(), CoreError> {
    let mut ordered: Vec<&ImageExtent<'_>> = extents.iter().collect();
    ordered.sort_by_key(|extent| extent.start);
    for pair in ordered.windows(2) {
        let end = u64::from(pair[0].start) + u64::from(pair[0].virtual_size);
        if u64::from(pair[1].start) < end {
            return Err(CoreError::SectionByteOverlap {
                section_index: pair[1].section_index.unwrap_or(u32::MAX),
                address: GuestVa(pair[1].start),
            });
        }
    }
    Ok(())
}

/// Maps every maximal run of contiguous covered pages as anonymous RAM.
fn map_covered_pages(
    memory: &SoftwareAddressSpace,
    extents: &[ImageExtent<'_>],
) -> Result<(), CoreError> {
    // Collect the covered page set by walking each extent's page span.
    let mut covered: Vec<u32> = Vec::new();
    for extent in extents {
        let (first, last) = extent.page_bounds();
        for page in first..=last {
            covered.push(page);
        }
    }
    covered.sort_unstable();
    covered.dedup();

    let page_size = u64::from(GUEST_PAGE_SIZE);
    let mut index = 0;
    while index < covered.len() {
        let run_start = covered[index];
        let mut run_end = run_start;
        while index + 1 < covered.len() && covered[index + 1] == run_end + 1 {
            index += 1;
            run_end = covered[index];
        }
        index += 1;

        let start = GuestVa(run_start << GUEST_PAGE_SHIFT);
        let len = (u64::from(run_end - run_start) + 1) * page_size;
        let range =
            GuestRange::page_aligned(start, len).map_err(exbawks_memory::MemoryError::from)?;
        memory.map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)?;
    }

    Ok(())
}

/// Sets each covered page to the union permission of its contributors.
fn apply_merged_permissions(
    memory: &SoftwareAddressSpace,
    extents: &[ImageExtent<'_>],
) -> Result<(), CoreError> {
    use std::collections::BTreeMap;

    let mut page_perms: BTreeMap<u32, MemoryPermissions> = BTreeMap::new();
    for extent in extents {
        let (first, last) = extent.page_bounds();
        for page in first..=last {
            let mut perm = MemoryPermissions::READ;
            if extent.permissions.contains(MemoryPermissions::EXECUTE) {
                perm |= MemoryPermissions::EXECUTE;
            }
            if extent.writes_page(page) {
                perm |= MemoryPermissions::WRITE;
            }
            let entry = page_perms.entry(page).or_insert(MemoryPermissions::empty());
            *entry |= perm;
        }
    }

    // Apply in maximal runs of equal permission.
    let page_size = u64::from(GUEST_PAGE_SIZE);
    let mut iter = page_perms.into_iter().peekable();
    while let Some((run_start, perm)) = iter.next() {
        let mut run_end = run_start;
        while let Some((next_page, next_perm)) = iter.peek() {
            if *next_page == run_end + 1 && *next_perm == perm {
                run_end = *next_page;
                iter.next();
            } else {
                break;
            }
        }

        let start = GuestVa(run_start << GUEST_PAGE_SHIFT);
        let len = (u64::from(run_end - run_start) + 1) * page_size;
        let range =
            GuestRange::page_aligned(start, len).map_err(exbawks_memory::MemoryError::from)?;
        memory.protect(range, perm)?;
    }

    Ok(())
}

/// Replaces every parsed thunk slot with its kernel gate address.
///
/// Slot pages regain their original permissions after each patch.
fn patch_kernel_thunks(
    memory: &SoftwareAddressSpace,
    table: &KernelThunkTable,
    data_variables: &BTreeMap<u16, GuestVa>,
) -> Result<(), CoreError> {
    for thunk in &table.entries {
        // A data export's slot points at its live kernel variable; a
        // function export's slot points at its dispatch gate (ADR 0010).
        let target = match data_variables.get(&thunk.ordinal) {
            Some(address) => address.0,
            None => gate_address(thunk.ordinal).0,
        };

        let span = GuestRange::new(thunk.slot, 4).map_err(MemoryError::from)?;
        let mut originals = Vec::new();
        for page in span.pages() {
            let original = memory.page_table().get(page).permissions();
            let range = GuestRange::page_aligned(page.start_va(), u64::from(GUEST_PAGE_SIZE))
                .map_err(MemoryError::from)?;
            memory.protect(range, original | MemoryPermissions::WRITE)?;
            originals.push((range, original));
        }

        memory.write_u32(thunk.slot, target)?;

        for (range, original) in originals {
            memory.protect(range, original)?;
        }
    }

    Ok(())
}

/// The guest virtual base of the Xbox kernel image in the cached window.
const KERNEL_IMAGE_BASE: u32 = 0x8001_0000;

/// Maps a minimal synthetic kernel PE image at the kernel's fixed base.
///
/// Retail titles read the kernel image directly at `0x8001_0000` — for
/// example parsing its PE section table (e.g. to discard the `INIT`
/// section). This provides just enough of a valid DOS + PE header that such
/// a parse reads sane fields and lands inside the mapped page; the single
/// section is left unnamed, so the `INIT`-section check takes its clean
/// no-op return. A fuller kernel image is future work (and unnecessary under
/// the WHP tier, where the real kernel is present).
fn map_synthetic_kernel_image(memory: &SoftwareAddressSpace) -> Result<(), CoreError> {
    const E_LFANEW: u32 = 0x40;
    let base = KERNEL_IMAGE_BASE;
    // The image lives in kernel-owned low physical memory reserved at load
    // (physical `0x10000`), already reachable through the window alias; the
    // writes below go through the window, then the page tightens to R+X.
    let range = GuestRange::page_aligned(GuestVa(base), u64::from(GUEST_PAGE_SIZE))
        .map_err(MemoryError::from)?;

    // DOS header: the `MZ` magic and the `e_lfanew` offset to the PE header.
    memory.write(GuestVa(base), b"MZ")?;
    memory.write_u32(GuestVa(base + 0x3C), E_LFANEW)?;

    // PE header: signature, then an `IMAGE_FILE_HEADER` with one section and
    // a standard optional-header size. The guest reconstructs the section
    // table from `NumberOfSections` (+6) and `SizeOfOptionalHeader` (+0x14).
    let nt = base + E_LFANEW;
    memory.write(GuestVa(nt), b"PE\0\0")?;
    memory.write(GuestVa(nt + 4), &0x014C_u16.to_le_bytes())?; // Machine: i386
    memory.write(GuestVa(nt + 6), &1_u16.to_le_bytes())?; // NumberOfSections
    memory.write(GuestVa(nt + 0x14), &0xE0_u16.to_le_bytes())?; // SizeOfOptionalHeader
    memory.write(GuestVa(nt + 0x18), &0x010B_u16.to_le_bytes())?; // PE32 magic

    // The lone section header (at nt + 0x18 + 0xE0) stays zeroed, so its
    // name is not `INIT` and the kernel-image section check returns cleanly.
    memory.protect(range, MemoryPermissions::READ | MemoryPermissions::EXECUTE)?;
    Ok(())
}

/// Derives the guest device path of the loaded image for `XeImageFileName`.
fn image_device_name(_image: &XbeImage) -> String {
    // The retail boot medium is the DVD; XAPI derives the `D:` mount from
    // this path. The leaf name is conventional across retail titles.
    "\\Device\\CdRom0\\default.xbe".to_owned()
}

fn physical_dependencies(
    table: &exbawks_memory::PageTable,
    decoded: &DecodedBlock,
) -> Result<Vec<PhysicalPageDependency>, CoreError> {
    let byte_len = u64::try_from(decoded.byte_len)
        .map_err(|_| CoreError::InvalidConfiguration("decoded block size does not fit u64"))?;
    let range =
        GuestRange::new(decoded.start, byte_len).map_err(exbawks_memory::MemoryError::from)?;
    let mut pages = BTreeMap::new();

    for virtual_page in range.pages() {
        let descriptor = table.get(virtual_page);
        if descriptor.kind() != PageKind::Ram {
            return Err(exbawks_memory::MemoryError::Unmapped {
                address: virtual_page.start_va(),
                access: exbawks_types::AccessKind::Execute,
            }
            .into());
        }
        // The baseline comes from the authoritative per-physical-page
        // generation array (ADR 0005), which cache revalidation also reads;
        // the descriptor's embedded stamp is not kept in sync (a per-write
        // full-table sync walk made every guest write O(page table)).
        let physical_page = descriptor.physical_page();
        let generation = table.physical_generation(physical_page).unwrap_or_default();
        pages.entry(physical_page).or_insert(generation);
    }

    Ok(pages
        .into_iter()
        .map(|(page, generation)| PhysicalPageDependency { page, generation })
        .collect())
}

#[cfg(test)]
mod tests {
    use exbawks_types::BuildFlavor;

    use super::*;

    const ENTRY_RETAIL_XOR: u32 = 0xA8FC_57AB;
    const KERNEL_RETAIL_XOR: u32 = 0x5B6D_40B6;

    #[test]
    fn emulator_loads_and_plans_a_synthetic_image() {
        let mut emulator = Emulator::new().expect("emulator must initialize");
        let image = emulator.load_xbe(synthetic_xbe()).expect("synthetic XBE must load");
        let plan = emulator.plan_entry_block().expect("entry block must plan");

        assert_eq!(image.image().header.build_flavor, BuildFlavor::Retail);
        assert_eq!(plan.decoded.instructions.len(), 2);
        assert_eq!(plan.compiled.plan.actions.len(), 2);

        // Windows hosts emit executable code; portable hosts keep the plan.
        #[cfg(windows)]
        {
            use exbawks_jit::CompilationState;
            assert_eq!(plan.compiled.state, CompilationState::Executable);
            assert!(!plan.compiled.machine_code.is_empty());
        }
        #[cfg(not(windows))]
        assert!(plan.compiled.machine_code.is_empty());
    }

    #[test]
    fn plan_report_reveals_zero_coverage_for_memory_first_blocks() {
        // Entry starts with `mov ecx, ds:[0x10118]`, which the register-only
        // subset rejects, so the artifact translates zero instructions even
        // though emission succeeds.
        let memory_first = [0x8B, 0x0D, 0x18, 0x01, 0x01, 0x00, 0xC3];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&memory_first, &[])).expect("image must load");
        let report = emulator.plan_entry_block().expect("entry block must plan").report();

        #[cfg(windows)]
        {
            assert_eq!(report.compilation_state, "Executable");
            assert_eq!(report.translated_instructions, Some(0));
            assert_eq!(report.static_exit.as_deref(), Some("UnsupportedInstruction"));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(report.compilation_state, "Planned");
            assert_eq!(report.translated_instructions, None);
            assert_eq!(report.static_exit, None);
        }
    }

    #[test]
    fn plan_report_counts_the_translated_prefix() {
        // `nop` translates; the terminating `ret` does not, so coverage is
        // one of two decoded instructions.
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");
        let report = emulator.plan_entry_block().expect("entry block must plan").report();

        assert_eq!(report.decoded_instructions, 2);
        #[cfg(windows)]
        {
            assert_eq!(report.translated_instructions, Some(1));
            assert_eq!(report.static_exit.as_deref(), Some("UnsupportedInstruction"));
        }
        #[cfg(not(windows))]
        assert_eq!(report.translated_instructions, None);
    }

    /// The interpreter tier alone can carry a boot from memory-operand code
    /// through a kernel gate to a controlled exit — on any host.
    #[test]
    fn interpreter_fallback_reaches_a_gate_exit() {
        let code = [
            0x8B, 0x0D, 0x04, 0x01, 0x01, 0x00, // mov ecx, [0x10104] (header base field)
            0x6A, 0x00, // push 0
            0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, // call [0x11200] -> HalReturnToFirmware
        ];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&code, &[0x8000_0000 | 49])).expect("image must load");
        let stop = emulator.run(64).expect("run must complete");

        assert_eq!(stop, StopReason::GuestExit { code: 0 });
        assert_eq!(emulator.cpu().gpr[1], 0x0001_0000, "the interpreted load must land");
        assert!(emulator.cpu().tsc > 0, "interpreted instructions must advance the counter");
    }

    /// The synthetic kernel image parses as a PE whose last section is not
    /// `INIT`, so the guest's kernel-image section check returns cleanly.
    #[test]
    fn synthetic_kernel_image_parses_without_an_init_section() {
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");
        let memory = emulator.memory();

        assert_eq!(memory.read_u32(GuestVa(0x8001_003C)).expect("read"), 0x40, "e_lfanew");
        let nt = 0x8001_0000 + 0x40;
        assert_eq!(memory.read_u32(GuestVa(nt)).expect("read"), 0x0000_4550, "PE signature");
        // The address the guest computes for the last section header's name.
        let name = memory.read_u32(GuestVa(0x8001_0138)).expect("read");
        assert_ne!(name, 0x5449_4E49, "the last section must not be named INIT");
    }

    /// A boot thread that returns off its stack (to a null return address)
    /// exits cleanly instead of faulting at address zero.
    ///
    /// The entry is a bare `ret`, which the interpreter tier executes on any
    /// host (a register-only entry would classify as translatable and reach
    /// no executable off Windows). The boot thread starts with zeroed
    /// registers, so it exits with code 0 through its unset return slot.
    #[test]
    fn boot_thread_returning_to_null_exits() {
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&[0xC3], &[])).expect("image must load");
        let stop = emulator.run(64).expect("run must complete");
        assert_eq!(stop, StopReason::GuestExit { code: 0 });
    }

    /// A title that self-relaunches (ADR 0015) sets its `LaunchDataPage`,
    /// persists it, and reboots; the second boot must find the launch data
    /// preserved and take a different path. Here the second boot exits
    /// cleanly instead of rebooting again, proving the relaunch preserved the
    /// launch data across the machine reset.
    ///
    /// ```text
    /// mov ecx, [0x11200]   ; ecx = &LaunchDataPage (data-export cell)
    /// mov eax, [ecx]       ; eax = LaunchDataPage
    /// test eax, eax
    /// jnz already          ; boot 2: launch data present
    /// push 0x1000          ; boot 1: allocate, publish, persist, reboot
    /// call [0x11204]       ; MmAllocateContiguousMemory -> eax
    /// mov [ecx], eax       ; LaunchDataPage = buffer
    /// push 1; push 0x1000; push eax
    /// call [0x11208]       ; MmPersistContiguousMemory
    /// push 2
    /// call [0x1120C]       ; HalReturnToFirmware(quick reboot)
    /// already: push 0
    /// call [0x1120C]       ; HalReturnToFirmware(halt) -> GuestExit { 0 }
    /// ```
    #[test]
    fn self_relaunch_preserves_launch_data() {
        const RELAUNCH_CODE: [u8; 55] = [
            0x8B, 0x0D, 0x00, 0x12, 0x01, 0x00, // mov ecx, [0x00011200]
            0x8B, 0x01, // mov eax, [ecx]
            0x85, 0xC0, // test eax, eax
            0x75, 0x23, // jnz already (+0x23)
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0xFF, 0x15, 0x04, 0x12, 0x01, 0x00, // call [0x00011204]
            0x89, 0x01, // mov [ecx], eax
            0x6A, 0x01, // push 1
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0x50, // push eax
            0xFF, 0x15, 0x08, 0x12, 0x01, 0x00, // call [0x00011208]
            0x6A, 0x02, // push 2
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
            0x6A, 0x00, // already: push 0
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
        ];
        // Imports: LaunchDataPage (164, data), MmAllocateContiguousMemory
        // (165), MmPersistContiguousMemory (178), HalReturnToFirmware (49).
        let thunks = [0x8000_0000 | 164, 0x8000_0000 | 165, 0x8000_0000 | 178, 0x8000_0000 | 49];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&RELAUNCH_CODE, &thunks)).expect("image must load");

        let stop = emulator.run(4096).expect("run must complete");

        // The only path to GuestExit { 0 } is the second boot's
        // already-launched branch, reached only when LaunchDataPage survived
        // the reboot.
        assert_eq!(stop, StopReason::GuestExit { code: 0 });
    }

    /// Persisted launch-data BYTES survive the relaunch, not just the
    /// pointer: boot one writes a magic word into the persisted page and
    /// reboots; boot two exits 0 only when the magic reads back intact.
    /// (The window alias regression silently skipped the byte restore while
    /// the pointer test still passed — this pins the content path.)
    ///
    /// ```text
    /// mov ecx, [0x11200]; mov eax, [ecx]; test eax, eax; jnz already
    /// push 0x1000; call [0x11204]; mov [ecx], eax
    /// mov dword [eax+0x400], 0xC0DEC0DE
    /// push 1; push 0x1000; push eax; call [0x11208]
    /// push 2; call [0x1120C]
    /// already: cmp dword [eax+0x400], 0xC0DEC0DE; jne bad
    /// push 0; call [0x1120C]
    /// bad: push 7; call [0x1120C]
    /// ```
    #[test]
    fn relaunch_restores_persisted_content() {
        const CONTENT_CODE: [u8; 85] = [
            0x8B, 0x0D, 0x00, 0x12, 0x01, 0x00, // mov ecx, [0x00011200]
            0x8B, 0x01, // mov eax, [ecx]
            0x85, 0xC0, // test eax, eax
            0x75, 0x2D, // jnz already (+0x2D)
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0xFF, 0x15, 0x04, 0x12, 0x01, 0x00, // call [0x00011204]
            0x89, 0x01, // mov [ecx], eax
            0xC7, 0x80, 0x00, 0x04, 0x00, 0x00, 0xDE, 0xC0, 0xDE,
            0xC0, // mov dword [eax+0x400], 0xC0DEC0DE
            0x6A, 0x01, // push 1
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0x50, // push eax
            0xFF, 0x15, 0x08, 0x12, 0x01, 0x00, // call [0x00011208]
            0x6A, 0x02, // push 2
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
            0x81, 0xB8, 0x00, 0x04, 0x00, 0x00, 0xDE, 0xC0, 0xDE,
            0xC0, // already: cmp dword [eax+0x400], 0xC0DEC0DE
            0x75, 0x08, // jne bad (+8)
            0x6A, 0x00, // push 0
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
            0x6A, 0x07, // bad: push 7
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
        ];
        let thunks = [0x8000_0000 | 164, 0x8000_0000 | 165, 0x8000_0000 | 178, 0x8000_0000 | 49];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&CONTENT_CODE, &thunks)).expect("image must load");

        let stop = emulator.run(4096).expect("run must complete");

        assert_eq!(stop, StopReason::GuestExit { code: 0 }, "the magic word must survive");
    }

    /// A title stuck rebooting with identical launch data is a reboot loop,
    /// not progress; the run detects the repeated launch data and stops with
    /// `Reboot` instead of relaunching to the backstop limit. The guest here
    /// reuses its `LaunchDataPage` and reboots every boot.
    ///
    /// ```text
    /// mov ecx, [0x11200]; mov eax, [ecx]; test eax, eax
    /// jnz have_buffer      ; reuse an existing page
    /// push 0x1000; call [0x11204]; mov [ecx], eax
    /// have_buffer: push 1; push 0x1000; push eax
    /// call [0x11208]       ; persist
    /// push 2; call [0x1120C] ; reboot — always
    /// ```
    #[test]
    fn identical_relaunch_data_stops_the_loop() {
        const LOOP_CODE: [u8; 47] = [
            0x8B, 0x0D, 0x00, 0x12, 0x01, 0x00, // mov ecx, [0x00011200]
            0x8B, 0x01, // mov eax, [ecx]
            0x85, 0xC0, // test eax, eax
            0x75, 0x0D, // jnz have_buffer (+0x0D)
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0xFF, 0x15, 0x04, 0x12, 0x01, 0x00, // call [0x00011204]
            0x89, 0x01, // mov [ecx], eax
            0x6A, 0x01, // have_buffer: push 1
            0x68, 0x00, 0x10, 0x00, 0x00, // push 0x1000
            0x50, // push eax
            0xFF, 0x15, 0x08, 0x12, 0x01, 0x00, // call [0x00011208]
            0x6A, 0x02, // push 2
            0xFF, 0x15, 0x0C, 0x12, 0x01, 0x00, // call [0x0001120C]
        ];
        let thunks = [0x8000_0000 | 164, 0x8000_0000 | 165, 0x8000_0000 | 178, 0x8000_0000 | 49];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&LOOP_CODE, &thunks)).expect("image must load");

        let stop = emulator.run(4096).expect("run must complete");

        // The first reboot relaunches; the second persists identical data, so
        // the loop is detected and the run stops rather than spinning.
        assert_eq!(stop, StopReason::Reboot { routine: 2 });
    }

    /// A created thread reaching a null address is a genuine fault, not an
    /// intended exit — only the boot thread returns to null on purpose.
    #[test]
    fn created_thread_reaching_null_faults() {
        use exbawks_kernel::{KernelServices, ThreadCreateRequest};

        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");
        emulator
            .threads
            .as_mut()
            .unwrap()
            .create_thread(ThreadCreateRequest {
                thread_extension_size: 0,
                kernel_stack_size: 0,
                tls_data_size: 0,
                start_routine: GuestVa(0x0002_0000),
                start_context1: 0,
                start_context2: 0,
                create_suspended: false,
            })
            .expect("thread creates");

        // The boot thread exits, making the created thread active.
        assert_eq!(emulator.exit_current_thread(0), None, "the child keeps execution alive");
        // Force the created thread to a null address: it must fault.
        emulator.cpu.eip = 0;
        let stop = emulator.run(4).expect("run must complete");
        assert_eq!(stop, StopReason::GuestFault { address: GuestVa(0) });
    }

    /// Loading an image builds a KPCR the guest can read through fs, and the
    /// boot stack honors the XBE-declared size.
    #[test]
    fn load_builds_the_boot_thread_environment() {
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");

        let fs_base = emulator.cpu().segment(exbawks_cpu::Segment::Fs).base;
        assert!(fs_base >= 0x8000_0000, "the KPCR must live in kernel space");
        // fs:[0x1C] is the self pointer; fs:[0x20] is the Prcb pointer, whose
        // first field (fs:[0x28]) is the current KTHREAD.
        let self_pointer = emulator.memory().read_u32(GuestVa(fs_base + 0x1C)).expect("read");
        assert_eq!(self_pointer, fs_base);
        let prcb = emulator.memory().read_u32(GuestVa(fs_base + 0x20)).expect("read");
        assert_eq!(prcb, fs_base + 0x28, "the Prcb points at the embedded KPRCB");
        let kthread = emulator.memory().read_u32(GuestVa(prcb)).expect("read");
        assert_eq!(kthread, fs_base + 0x200, "Prcb.CurrentThread is the KTHREAD");
    }

    /// PsCreateSystemThreadEx succeeds through the service, and a thread that
    /// returns to the exit sentinel switches to the next ready thread.
    #[test]
    fn thread_creation_and_exit_drive_the_scheduler() {
        use exbawks_kernel::{KernelServices, ThreadCreateRequest};

        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");
        let threads = emulator.threads.as_mut().expect("threads exist");
        assert_eq!(threads.live_threads(), 1);

        let created = threads
            .create_thread(ThreadCreateRequest {
                thread_extension_size: 0,
                kernel_stack_size: 0,
                tls_data_size: 0,
                start_routine: GuestVa(0x0002_0000),
                start_context1: 0,
                start_context2: 0,
                create_suspended: false,
            })
            .expect("thread creates");
        assert!(created.handle >= 0xE000);
        assert_eq!(emulator.threads.as_ref().unwrap().live_threads(), 2);

        // The boot thread reaching the sentinel exits and resumes the child.
        emulator.cpu.eip = THREAD_EXIT_SENTINEL.0;
        emulator.cpu.gpr[0] = 0;
        let stop = emulator.exit_current_thread(0);
        assert_eq!(stop, None, "a ready child keeps execution alive");
        assert_eq!(emulator.cpu().eip, 0x0002_0000, "the child's context is now active");

        // The child exiting with no remaining threads stops the run.
        let stop = emulator.exit_current_thread(7);
        assert_eq!(stop, Some(StopReason::GuestExit { code: 7 }));
    }

    /// A new thread's stack has an unmapped guard page below its limit, and
    /// the guest time-stamp counter never moves backward across a switch.
    #[test]
    fn thread_switch_guards_stacks_and_preserves_the_counter() {
        use exbawks_kernel::{KernelServices, ThreadCreateRequest};

        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("image must load");
        let created = emulator
            .threads
            .as_mut()
            .unwrap()
            .create_thread(ThreadCreateRequest {
                thread_extension_size: 0,
                kernel_stack_size: 16 * 1024,
                tls_data_size: 0,
                start_routine: GuestVa(0x0002_0000),
                start_context1: 0,
                start_context2: 0,
                create_suspended: false,
            })
            .expect("thread creates");

        // The KTHREAD records the child's stack limit. Under the cached
        // physical window (ADR 0010) every physical page is mapped, so the
        // page below the limit is burned spacing rather than a faulting
        // guard; the limit itself must still be a valid window address.
        let stack_limit =
            emulator.memory().read_u32(GuestVa(created.kthread.0 + 0x20)).expect("read");
        assert!(stack_limit >= 0x8000_0000, "the stack lives in the kernel window");

        // A boot thread that has run instructions exits into the child; the
        // counter carries across rather than resetting to the child default.
        emulator.cpu.tsc = 5_000;
        emulator.exit_current_thread(0);
        assert_eq!(emulator.cpu().tsc, 5_000, "the counter must not move backward");
    }

    /// Register-only blocks run through the JIT while everything between
    /// them runs through the interpreter, sharing one CpuState.
    #[cfg(windows)]
    #[test]
    fn tiers_interleave_within_one_boot() {
        let code = [
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5 (translated block)
            0x8B, 0x0D, 0x04, 0x01, 0x01, 0x00, // mov ecx, [0x10104] (interpreted)
            0x01, 0xC1, // add ecx, eax (translated block)
            0x6A, 0x00, // push 0 (interpreted)
            0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, // call [0x11200]
        ];
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe_with(&code, &[0x8000_0000 | 49])).expect("image must load");
        let stop = emulator.run(64).expect("run must complete");

        assert_eq!(stop, StopReason::GuestExit { code: 0 });
        assert_eq!(emulator.cpu().gpr[1], 0x0001_0005, "both tiers must contribute");
    }

    #[test]
    fn reset_allows_another_image() {
        let mut emulator = Emulator::new().expect("emulator must initialize");
        emulator.load_xbe(synthetic_xbe()).expect("first image must load");
        emulator.reset().expect("reset must succeed");
        emulator.load_xbe(synthetic_xbe()).expect("second image must load");
    }

    fn synthetic_xbe() -> Vec<u8> {
        synthetic_xbe_with(&[0x90, 0xC3], &[])
    }

    /// Builds one retail image with entry code and raw thunk table values.
    ///
    /// Code loads at base + 0x1000 and the thunk table at base + 0x1200.
    fn synthetic_xbe_with(code: &[u8], thunk_values: &[u32]) -> Vec<u8> {
        assert!(code.len() <= 0x200, "entry code must fit before the thunk table");
        let mut bytes = vec![0_u8; 0x580];
        bytes[..4].copy_from_slice(b"XBEH");
        let base = 0x0001_0000_u32;
        write_u32(&mut bytes, 0x104, base);
        write_u32(&mut bytes, 0x108, 0x280);
        write_u32(&mut bytes, 0x10C, 0x4000);
        write_u32(&mut bytes, 0x110, 0x178);
        write_u32(&mut bytes, 0x118, base + 0x178);
        write_u32(&mut bytes, 0x11C, 1);
        write_u32(&mut bytes, 0x120, base + 0x200);
        write_u32(&mut bytes, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR);
        write_u32(&mut bytes, 0x130, 0x10000);
        write_u32(&mut bytes, 0x134, 0x100000);
        write_u32(&mut bytes, 0x138, 0x1000);
        write_u32(&mut bytes, 0x158, (base + 0x1200) ^ KERNEL_RETAIL_XOR);

        write_u32(&mut bytes, 0x200, XbeSectionFlags::EXECUTABLE.bits());
        write_u32(&mut bytes, 0x204, base + 0x1000);
        write_u32(&mut bytes, 0x208, 0x300);
        write_u32(&mut bytes, 0x20C, 0x280);
        write_u32(&mut bytes, 0x210, 0x300);
        write_u32(&mut bytes, 0x214, base + 0x238);
        bytes[0x238..0x23E].copy_from_slice(b".text\0");

        bytes[0x280..0x280 + code.len()].copy_from_slice(code);
        for (index, value) in thunk_values.iter().enumerate() {
            write_u32(&mut bytes, 0x480 + index * 4, *value);
        }
        bytes
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    mod gates {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        use exbawks_kernel::KernelExport;

        use super::*;

        /// Guest code: `call dword ptr [0x11200]; nop; ret`.
        const GATE_CALL_CODE: [u8; 8] = [0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, 0x90, 0xC3];

        struct CountingExport {
            calls: Arc<AtomicU32>,
        }

        impl KernelExport for CountingExport {
            fn ordinal(&self) -> u16 {
                7
            }

            fn name(&self) -> &'static str {
                "CountingExport"
            }

            fn call(&self, context: &mut KernelCallContext<'_>) -> KernelStatus {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let _ = context;
                KernelStatus(0xFEED_F00D)
            }
        }

        #[test]
        fn load_patches_thunks_into_gate_addresses() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            let image = emulator
                .load_xbe(synthetic_xbe_with(&[0x90, 0xC3], &[0x8000_0007, 0x8000_0170]))
                .expect("synthetic XBE must load");

            let memory = emulator.memory();
            assert_eq!(
                memory.read_u32(GuestVa(0x0001_1200)).expect("slot must read"),
                gate_address(0x7).0
            );
            assert_eq!(
                memory.read_u32(GuestVa(0x0001_1204)).expect("slot must read"),
                gate_address(0x170).0
            );

            // The slot page keeps its original permissions.
            let descriptor = memory.page_table().get(GuestVa(0x0001_1200).page());
            assert!(!descriptor.permissions().contains(MemoryPermissions::WRITE));

            // The loaded image records the table as parsed before patching.
            let thunks = image.kernel_thunks();
            assert_eq!(thunks.entries.len(), 2);
            assert_eq!(thunks.entries[0].ordinal, 7);
            assert_eq!(thunks.entries[1].ordinal, 0x170);
        }

        #[test]
        fn synthetic_thunk_calls_one_registered_export() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            let calls = Arc::new(AtomicU32::new(0));
            emulator
                .kernel()
                .register(CountingExport { calls: calls.clone() })
                .expect("registration must succeed");
            emulator
                .load_xbe(synthetic_xbe_with(&GATE_CALL_CODE, &[0x8000_0007]))
                .expect("synthetic XBE must load");

            let stack_before = emulator.cpu().gpr[4];
            let assist = emulator.try_kernel_gate_call().expect("assist must succeed");
            assert_eq!(
                assist,
                GateAssist::Dispatched {
                    ordinal: 7,
                    resume: GuestVa(0x0001_1006),
                    status: KernelStatus(0xFEED_F00D),
                    stop: None
                }
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(emulator.cpu().eip, 0x0001_1006);
            // The runtime places the returned status in guest EAX.
            assert_eq!(emulator.cpu().gpr[0], 0xFEED_F00D);
            assert_eq!(emulator.cpu().gpr[4], stack_before, "the call and return balance");
        }

        #[cfg(windows)]
        #[test]
        fn gate_reached_by_register_call_dispatches() {
            // `mov eax, [0x11200]; call eax; ret` — the guest loads the
            // patched gate address into a register and calls it, so EIP lands
            // in the gate region with the return address already pushed.
            // CORE-004 dispatches the ordinal from the EIP rather than from a
            // decoded `call [slot]` at the caller.
            let code = [0x8B, 0x05, 0x00, 0x12, 0x01, 0x00, 0xFF, 0xD0, 0xC3];
            let mut emulator = Emulator::new().expect("emulator must initialize");
            let calls = Arc::new(AtomicU32::new(0));
            emulator
                .kernel()
                .register(CountingExport { calls: calls.clone() })
                .expect("registration must succeed");
            emulator
                .load_xbe(synthetic_xbe_with(&code, &[0x8000_0007]))
                .expect("synthetic XBE must load");

            let stop = emulator.run(16).expect("the run must complete");

            assert_eq!(
                calls.load(Ordering::Relaxed),
                1,
                "the register-indirect gate call dispatched the export"
            );
            assert_eq!(emulator.cpu().gpr[0], 0xFEED_F00D, "the export status reached guest eax");
            assert_eq!(stop, StopReason::GuestExit { code: 0xFEED_F00D });
        }

        #[test]
        fn unknown_ordinal_reports_the_missing_export() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&GATE_CALL_CODE, &[0x8000_0007]))
                .expect("synthetic XBE must load");

            let assist = emulator.try_kernel_gate_call().expect("assist must succeed");
            assert_eq!(assist, GateAssist::MissingExport { ordinal: 7 });
            assert_eq!(emulator.cpu().eip, 0x0001_1000, "a missing export must not advance EIP");
        }

        #[test]
        fn non_gate_calls_are_not_dispatched() {
            // The called slot at 0x11300 holds zero, which is not a gate.
            let code = [0xFF, 0x15, 0x00, 0x13, 0x01, 0x00, 0x90, 0xC3];
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator.load_xbe(synthetic_xbe_with(&code, &[])).expect("synthetic XBE must load");

            let assist = emulator.try_kernel_gate_call().expect("assist must succeed");
            assert_eq!(assist, GateAssist::NotAGateCall);
        }

        #[test]
        fn malformed_thunks_stay_typed_load_errors() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            let error = emulator
                .load_xbe(synthetic_xbe_with(&[0x90, 0xC3], &[0x0000_1234]))
                .expect_err("a malformed thunk must fail the load");
            assert!(matches!(error, CoreError::InvalidKernelThunk { .. }));
        }
    }

    mod milestone {
        use super::*;

        /// The synthetic boot title for the first execution milestone.
        ///
        /// ```text
        /// 0x11000  mov eax, 5           ; translated arithmetic
        /// 0x11005  add eax, 0x25        ; eax = 42
        /// 0x11008  mov esi, eax         ; esi = 42, a register preserved across the call
        /// 0x1100A  call [0x11200]       ; DbgPrint through the first gate
        /// 0x11010  mov edi, eax         ; edi = the returned status (translated work after the return)
        /// 0x11012  call [0x11204]       ; HalReturnToFirmware requests a guest exit
        /// 0x11018  ret
        /// ```
        ///
        /// The register-only guest cannot push a format pointer, so DbgPrint
        /// sees a null argument and returns INVALID_PARAMETER; the milestone
        /// verifies that this status reaches guest EAX and that ESI survives
        /// the kernel call unchanged.
        const BOOT_CODE: [u8; 25] = [
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
            0x83, 0xC0, 0x25, // add eax, 0x25
            0x89, 0xC6, // mov esi, eax
            0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, // call [0x11200]
            0x89, 0xC7, // mov edi, eax
            0xFF, 0x15, 0x04, 0x12, 0x01, 0x00, // call [0x11204]
            0xC3, // ret
        ];

        const BOOT_THUNKS: [u32; 2] = [0x8000_0008, 0x8000_0031];

        /// The NTSTATUS DbgPrint returns for a null format pointer.
        const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;

        /// The first-milestone boot title runs natively on the WHP tier:
        /// guest code executes on the virtual processor, both kernel calls
        /// dispatch through gate-fetch exits, and the exit status lands in
        /// EAX — end to end through real hardware virtualization.
        #[cfg(all(windows, target_arch = "x86_64"))]
        #[test]
        fn synthetic_title_boots_on_the_whp_tier() {
            let _hardware = exbawks_whp::hardware_serial_lock();
            if !exbawks_whp::probe_whp().usable() {
                eprintln!("skipping: WHP is not usable on this host");
                return;
            }
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&BOOT_CODE, &BOOT_THUNKS))
                .expect("synthetic XBE must load");

            let stop = emulator.run_whp(64).expect("the boot flow must run");

            assert_eq!(stop, StopReason::GuestExit { code: 0 });
            assert_eq!(emulator.cpu().gpr[6], 42, "esi survived the kernel call unchanged");
            assert_eq!(
                emulator.cpu().gpr[7],
                STATUS_INVALID_PARAMETER,
                "the export status reached guest eax across the gate exit"
            );
        }

        /// A guest MMIO access on the WHP tier is absorbed by the device
        /// stub and execution continues: the write is ignored, the read
        /// returns zero, and the title still reaches its clean exit.
        ///
        /// ```text
        /// mov eax, 0x1234
        /// mov [0xFE800200], eax   ; APU register write -> device stub
        /// mov esi, [0xFE800204]   ; APU register read  -> zero
        /// call [0x11200]          ; HalReturnToFirmware(halt)
        /// ```
        #[cfg(all(windows, target_arch = "x86_64"))]
        #[test]
        fn mmio_accesses_are_absorbed_on_the_whp_tier() {
            const MMIO_CODE: [u8; 24] = [
                0xB8, 0x34, 0x12, 0x00, 0x00, // mov eax, 0x1234
                0xA3, 0x00, 0x02, 0x80, 0xFE, // mov [0xFE800200], eax
                0x8B, 0x35, 0x04, 0x02, 0x80, 0xFE, // mov esi, [0xFE800204]
                0x6A, 0x00, // push 0
                0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, // call [0x11200]
            ];
            let _hardware = exbawks_whp::hardware_serial_lock();
            if !exbawks_whp::probe_whp().usable() {
                eprintln!("skipping: WHP is not usable on this host");
                return;
            }
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&MMIO_CODE, &[0x8000_0031]))
                .expect("synthetic XBE must load");

            let stop = emulator.run_whp(64).expect("the run must complete");

            assert_eq!(stop, StopReason::GuestExit { code: 0 });
            assert_eq!(emulator.cpu().gpr[6], 0, "the device read returned zero");
        }

        #[cfg(windows)]
        #[test]
        fn synthetic_title_boots_calls_the_kernel_and_exits() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&BOOT_CODE, &BOOT_THUNKS))
                .expect("synthetic XBE must load");
            assert_eq!(emulator.cpu().eip, 0x0001_1000, "the image reaches its entry point");

            let stop = emulator.run(16).expect("the boot flow must run");

            assert_eq!(stop, StopReason::GuestExit { code: 0 });
            assert_eq!(emulator.cpu().gpr[6], 42, "esi survived the kernel call unchanged");
            assert_eq!(
                emulator.cpu().gpr[7],
                STATUS_INVALID_PARAMETER,
                "the export status reached guest eax and translated code ran after the return"
            );
        }

        #[cfg(windows)]
        #[test]
        fn unknown_ordinals_stop_with_the_missing_export_name() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            let code = [0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, 0xC3];
            emulator
                .load_xbe(synthetic_xbe_with(&code, &[0x8000_012C]))
                .expect("synthetic XBE must load");

            let stop = emulator.run(16).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::MissingKernelExport { ordinal: 0x12C });
        }

        #[cfg(windows)]
        #[test]
        fn unimplemented_stub_halts_the_run() {
            // Ordinal 99 (KeDelayExecutionThread) is a registered but
            // unimplemented stub.
            let code = [0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, 0xC3];
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&code, &[0x8000_0063]))
                .expect("synthetic XBE must load");

            let stop = emulator.run(16).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::UnimplementedKernelExport { ordinal: 99 });
        }

        #[cfg(windows)]
        #[test]
        fn zero_budget_reports_exhaustion() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&BOOT_CODE, &BOOT_THUNKS))
                .expect("synthetic XBE must load");

            let stop = emulator.run(0).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::BudgetExhausted);
        }

        #[cfg(windows)]
        #[test]
        fn instructions_straddling_a_page_boundary_still_decode() {
            // A two-page executable image with `mov edi, 0x11223344` placed so
            // its five bytes straddle the 0x12000 page boundary, followed by
            // HalReturnToFirmware. Both pages are mapped, so the fetch must
            // span them instead of truncating at the boundary.
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator.load_xbe(two_page_straddle_image()).expect("synthetic XBE must load");
            emulator.cpu_mut().eip = 0x0001_1FFE;

            let stop = emulator.run(16).expect("the straddling block must run");
            assert_eq!(stop, StopReason::GuestExit { code: 0 });
            assert_eq!(emulator.cpu().gpr[7], 0x1122_3344, "the straddling mov executed");
        }

        /// Builds a two-page executable image for the straddle test.
        fn two_page_straddle_image() -> Vec<u8> {
            let base = 0x0001_0000_u32;
            // File: headers to 0x280, then a 0x1400-byte section body.
            let mut bytes = vec![0_u8; 0x280 + 0x1400];
            bytes[..4].copy_from_slice(b"XBEH");
            write_u32(&mut bytes, 0x104, base);
            write_u32(&mut bytes, 0x108, 0x280);
            write_u32(&mut bytes, 0x10C, 0x4000);
            write_u32(&mut bytes, 0x110, 0x178);
            write_u32(&mut bytes, 0x118, base + 0x178);
            write_u32(&mut bytes, 0x11C, 1);
            write_u32(&mut bytes, 0x120, base + 0x200);
            write_u32(&mut bytes, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR);
            write_u32(&mut bytes, 0x130, 0x10000);
            write_u32(&mut bytes, 0x134, 0x100000);
            write_u32(&mut bytes, 0x138, 0x1000);
            write_u32(&mut bytes, 0x158, (base + 0x2200) ^ KERNEL_RETAIL_XOR);

            write_u32(&mut bytes, 0x200, XbeSectionFlags::EXECUTABLE.bits());
            write_u32(&mut bytes, 0x204, base + 0x1000);
            write_u32(&mut bytes, 0x208, 0x1400);
            write_u32(&mut bytes, 0x20C, 0x280);
            write_u32(&mut bytes, 0x210, 0x1400);
            write_u32(&mut bytes, 0x214, base + 0x238);
            bytes[0x238..0x23E].copy_from_slice(b".text\0");

            // Section body starts at file 0x280 and guest 0x11000.
            let body = 0x280;
            // mov edi, 0x11223344 at guest 0x11FFE (section offset 0xFFE).
            bytes[body + 0xFFE..body + 0xFFE + 5].copy_from_slice(&[0xBF, 0x44, 0x33, 0x22, 0x11]);
            // call [0x12200]; ret at guest 0x12003.
            bytes[body + 0x1003..body + 0x1003 + 7]
                .copy_from_slice(&[0xFF, 0x15, 0x00, 0x22, 0x01, 0x00, 0xC3]);
            // Kernel thunk table at guest 0x12200 (section offset 0x1200): HalReturnToFirmware.
            write_u32(&mut bytes, body + 0x1200, 0x8000_0031);
            bytes
        }
    }

    mod loader {
        use exbawks_memory::GuestMemory;
        use exbawks_types::MemoryPermissions as Perm;

        use super::*;

        /// Builds a synthetic image with two byte-contiguous sections that
        /// share the page at their boundary.
        ///
        /// `.text` (R|X) spans pages 0x11 and 0x12; `.data` (writable) starts
        /// mid-page 0x12 and spans pages 0x12 and 0x13, so page 0x12 is shared.
        /// When `data_head_read_only` is set, `.data` marks its head page
        /// read-only, suppressing its write contribution to the shared page.
        fn contiguous_sections_image(
            data_head_read_only: bool,
            data_tail_read_only: bool,
        ) -> Vec<u8> {
            let base = 0x0001_0000_u32;
            let mut bytes = vec![0_u8; 0x440];
            bytes[..4].copy_from_slice(b"XBEH");
            write_u32(&mut bytes, 0x104, base);
            write_u32(&mut bytes, 0x108, 0x400); // size of headers
            write_u32(&mut bytes, 0x10C, 0x4000); // size of image
            write_u32(&mut bytes, 0x110, 0x178);
            write_u32(&mut bytes, 0x118, base + 0x178);
            write_u32(&mut bytes, 0x11C, 2); // section count
            write_u32(&mut bytes, 0x120, base + 0x200); // section header table
            write_u32(&mut bytes, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR);
            write_u32(&mut bytes, 0x130, 0x1000);
            write_u32(&mut bytes, 0x158, (base + 0x1000) ^ KERNEL_RETAIL_XOR);

            // Section 0: .text, R|X, VA 0x11000, size 0x1800 (pages 0x11, 0x12).
            let text = 0x200;
            write_u32(&mut bytes, text, XbeSectionFlags::EXECUTABLE.bits());
            write_u32(&mut bytes, text + 0x04, base + 0x1000);
            write_u32(&mut bytes, text + 0x08, 0x1800);
            write_u32(&mut bytes, text + 0x0C, 0x400);
            write_u32(&mut bytes, text + 0x10, 0x10);
            write_u32(&mut bytes, text + 0x14, base + 0x270);
            write_u32(&mut bytes, text + 0x18, 1);

            // Section 1: .data, writable, VA 0x12800 (shares page 0x12), size 0x900.
            let data = 0x238;
            let mut data_flags = XbeSectionFlags::WRITABLE;
            if data_head_read_only {
                data_flags |= XbeSectionFlags::HEAD_PAGE_READ_ONLY;
            }
            if data_tail_read_only {
                data_flags |= XbeSectionFlags::TAIL_PAGE_READ_ONLY;
            }
            write_u32(&mut bytes, data, data_flags.bits());
            write_u32(&mut bytes, data + 0x04, base + 0x2800);
            write_u32(&mut bytes, data + 0x08, 0x900);
            write_u32(&mut bytes, data + 0x0C, 0x420);
            write_u32(&mut bytes, data + 0x10, 0x10);
            write_u32(&mut bytes, data + 0x14, base + 0x278);
            write_u32(&mut bytes, data + 0x18, 1);

            bytes[0x270..0x276].copy_from_slice(b".text\0");
            bytes[0x278..0x27E].copy_from_slice(b".data\0");
            bytes[0x400..0x410].copy_from_slice(&[0xAA; 0x10]); // .text raw
            bytes[0x420..0x430].copy_from_slice(&[0xBB; 0x10]); // .data raw
            bytes
        }

        fn mapped(bytes: &[u8]) -> SoftwareAddressSpace {
            let image = XbeImage::parse(bytes).expect("synthetic image parses");
            let memory = SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory is valid");
            map_xbe(&memory, &image, bytes).expect("map_xbe succeeds");
            memory
        }

        fn page_perms(memory: &SoftwareAddressSpace, address: u32) -> Perm {
            memory.page_table().get(GuestVa(address).page()).permissions()
        }

        #[test]
        fn contiguous_sections_load_with_merged_shared_page() {
            let memory = mapped(&contiguous_sections_image(false, false));

            // Both sections' bytes sit at their exact guest addresses.
            let mut text = [0_u8; 4];
            memory.read(GuestVa(0x0001_1000), &mut text).expect("text reads");
            assert_eq!(text, [0xAA; 4]);
            let mut data = [0_u8; 4];
            memory.read(GuestVa(0x0001_2800), &mut data).expect("data reads");
            assert_eq!(data, [0xBB; 4]);
            // The shared page holds .text's (zero) tail below .data's bytes.
            let mut shared_tail = [0_u8; 4];
            memory.read(GuestVa(0x0001_27F0), &mut shared_tail).expect("shared tail reads");
            assert_eq!(shared_tail, [0x00; 4]);

            // Page 0x11: .text only, R|X.
            assert_eq!(page_perms(&memory, 0x0001_1000), Perm::READ | Perm::EXECUTE);
            // Page 0x12: shared; .data is writable with no read-only flag, so
            // the merge grants R|W|X.
            assert_eq!(page_perms(&memory, 0x0001_2000), Perm::READ | Perm::WRITE | Perm::EXECUTE);
            // Page 0x13: .data only, R|W.
            assert_eq!(page_perms(&memory, 0x0001_3000), Perm::READ | Perm::WRITE);
        }

        #[test]
        fn head_read_only_flag_suppresses_write_on_the_shared_page() {
            let memory = mapped(&contiguous_sections_image(true, false));

            // .data suppresses its write on its head page (the shared page),
            // so page 0x12 keeps only .text's R|X.
            assert_eq!(page_perms(&memory, 0x0001_2000), Perm::READ | Perm::EXECUTE);
            // .data's own tail page stays writable.
            assert_eq!(page_perms(&memory, 0x0001_3000), Perm::READ | Perm::WRITE);
            // The read-only shared page still holds the loaded .data bytes.
            let mut data = [0_u8; 4];
            memory.read(GuestVa(0x0001_2800), &mut data).expect("data reads");
            assert_eq!(data, [0xBB; 4]);
        }

        #[test]
        fn overlapping_section_bytes_are_rejected() {
            // Point .data's start inside .text's byte range.
            let mut bytes = contiguous_sections_image(false, false);
            write_u32(&mut bytes, 0x238 + 0x04, 0x0001_0000 + 0x1400);
            let image = XbeImage::parse(&bytes).expect("image parses");
            let memory = SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory is valid");
            let error = map_xbe(&memory, &image, &bytes).expect_err("overlap must fail");
            assert!(matches!(error, CoreError::SectionByteOverlap { .. }));
        }

        #[test]
        fn tail_read_only_flag_suppresses_write_on_the_tail_page() {
            let memory = mapped(&contiguous_sections_image(false, true));

            // .data suppresses write on its tail page 0x13, leaving it R only.
            assert_eq!(page_perms(&memory, 0x0001_3000), Perm::READ);
            // The shared head page 0x12 keeps .data's write (no head flag): R|W|X.
            assert_eq!(page_perms(&memory, 0x0001_2000), Perm::READ | Perm::WRITE | Perm::EXECUTE);
        }

        /// Builds a minimal image whose declared window leaves the guest space.
        ///
        /// `base_address = 0xFFFF_0000` with `size_of_image = 0x20000` gives an
        /// image end above 2^32. The image still parses.
        fn image_leaving_guest_space() -> Vec<u8> {
            let base = 0xFFFF_0000_u32;
            let mut bytes = vec![0_u8; 0x200];
            bytes[..4].copy_from_slice(b"XBEH");
            write_u32(&mut bytes, 0x104, base);
            write_u32(&mut bytes, 0x108, 0x200); // size of headers
            write_u32(&mut bytes, 0x10C, 0x20000); // size of image
            write_u32(&mut bytes, 0x110, 0x178);
            write_u32(&mut bytes, 0x118, base + 0x100);
            write_u32(&mut bytes, 0x11C, 0); // no sections
            write_u32(&mut bytes, 0x120, base + 0x100); // section header table (unused)
            write_u32(&mut bytes, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR);
            write_u32(&mut bytes, 0x130, 0x1000);
            write_u32(&mut bytes, 0x158, (base + 0x2000) ^ KERNEL_RETAIL_XOR);
            bytes
        }

        #[test]
        fn image_leaving_the_guest_space_is_a_typed_error() {
            // This crafted image previously overflowed page arithmetic; the
            // loader must reject it with a typed error and never panic.
            let bytes = image_leaving_guest_space();
            let image = XbeImage::parse(&bytes).expect("the crafted image still parses");
            let memory = SoftwareAddressSpace::new(1024 * 1024).expect("memory is valid");
            let error = map_xbe(&memory, &image, &bytes).expect_err("out-of-space image must fail");
            assert!(matches!(error, CoreError::ImageLeavesGuestSpace { .. }));
        }

        #[test]
        fn a_section_outside_the_image_window_is_rejected() {
            // Move .data's start past the declared image window (0x14000).
            let mut bytes = contiguous_sections_image(false, false);
            write_u32(&mut bytes, 0x238 + 0x04, 0x0001_0000 + 0x8000);
            let image = XbeImage::parse(&bytes).expect("image parses");
            let memory = SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory is valid");
            let error =
                map_xbe(&memory, &image, &bytes).expect_err("out-of-window section must fail");
            assert!(matches!(error, CoreError::RangeOutsideImage { .. }));
        }
    }
}
