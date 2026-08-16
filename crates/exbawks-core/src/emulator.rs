use std::collections::BTreeMap;
use std::sync::Arc;

use exbawks_cpu::{BasicBlockDecoder, CpuState, DecodeConfig, DecodedBlock, indirect_call_slot};
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

/// The guest stack range for the first synthetic thread.
const GUEST_STACK_BASE: GuestVa = GuestVa(0x03FF_0000);
/// The guest stack size in bytes.
const GUEST_STACK_BYTES: u32 = 64 * 1024;
/// The scratch bytes kept above the initial stack pointer.
const GUEST_STACK_SCRATCH: u32 = 16;

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
        map_xbe(&memory, &image, &bytes)?;

        let thunks = KernelThunkTable::read(
            memory.as_ref(),
            image.header.kernel_thunk_address,
            self.config.max_kernel_thunks,
        )?;
        patch_kernel_thunks(&memory, &thunks)?;

        // The first synthetic guest thread receives one mapped stack.
        let stack = GuestRange::page_aligned(GUEST_STACK_BASE, u64::from(GUEST_STACK_BYTES))
            .map_err(MemoryError::from)?;
        memory.map_anonymous(stack, MemoryPermissions::READ | MemoryPermissions::WRITE)?;
        let stack_top = GUEST_STACK_BASE.0 + GUEST_STACK_BYTES - GUEST_STACK_SCRATCH;

        self.cpu = CpuState { eip: image.header.entry_point.0, ..CpuState::default() };
        self.cpu.gpr[4] = stack_top;
        self.memory = memory;
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
        tracing::trace!(ordinal, name = export.name(), "dispatching kernel gate call");
        self.trace.record(TraceEvent::KernelCall { ordinal, caller: address });
        let memory = self.memory.clone();

        // Perform the call: push the return address so exports see the real
        // stdcall frame layout with arguments above the return slot.
        let pushed_esp = self.cpu.gpr[4].wrapping_sub(4);
        memory.write_u32(GuestVa(pushed_esp), resume.0)?;
        self.cpu.gpr[4] = pushed_esp;

        let mut context =
            KernelCallContext { cpu: &mut self.cpu, memory: memory.as_ref(), stop_request: None };
        let status = export.call(&mut context);
        let stop = context.stop_request;

        // Perform the return: pop the return address and the stdcall
        // argument bytes the export declares.
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
    /// block budget expires.
    pub fn run(&mut self, max_blocks: usize) -> Result<StopReason, CoreError> {
        let stop = self.run_blocks(max_blocks)?;
        self.trace.record(TraceEvent::Stop { reason: format!("{stop:?}") });
        Ok(stop)
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
        if self.loaded.is_none() {
            return Err(CoreError::NoImageLoaded);
        }

        for _ in 0..max_blocks {
            let start = GuestVa(self.cpu.eip);
            let (_, compiled) = self.compiled_block_at(start)?;
            let Some(emitted) = compiled.executable.as_ref() else {
                return Ok(StopReason::RuntimeIncomplete);
            };

            match Dispatcher.run(emitted, &mut self.cpu)? {
                BlockExit::DirectSuccessor => {}
                BlockExit::UnsupportedInstruction => match self.try_kernel_gate_call()? {
                    GateAssist::Dispatched { stop: Some(stop), .. } => return Ok(stop),
                    // An unimplemented stub cannot correctly clean the stdcall
                    // stack or return meaningful values, so halt rather than
                    // continue past it with corrupted guest state.
                    GateAssist::Dispatched { ordinal, status, .. }
                        if status == KernelStatus::NOT_IMPLEMENTED =>
                    {
                        return Ok(StopReason::UnimplementedKernelExport { ordinal });
                    }
                    GateAssist::Dispatched { .. } => {}
                    GateAssist::MissingExport { ordinal } => {
                        return Ok(StopReason::MissingKernelExport { ordinal });
                    }
                    GateAssist::NotAGateCall => {
                        return Ok(StopReason::UnsupportedInstruction {
                            address: GuestVa(self.cpu.eip),
                        });
                    }
                },
                _ => {
                    return Ok(StopReason::UnsupportedInstruction {
                        address: GuestVa(self.cpu.eip),
                    });
                }
            }
        }

        Ok(StopReason::BudgetExhausted)
    }

    /// Decodes and plans the current entry block.
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
        self.memory = Arc::new(SoftwareAddressSpace::new(self.config.physical_memory_bytes)?);
        self.cpu = CpuState::default();
        self.loaded = None;
        self.code_cache.clear();
        self.address_space_epoch = self.address_space_epoch.wrapping_add(1);
        Ok(())
    }
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
) -> Result<(), CoreError> {
    for thunk in &table.entries {
        let span = GuestRange::new(thunk.slot, 4).map_err(MemoryError::from)?;
        let mut originals = Vec::new();
        for page in span.pages() {
            let original = memory.page_table().get(page).permissions();
            let range = GuestRange::page_aligned(page.start_va(), u64::from(GUEST_PAGE_SIZE))
                .map_err(MemoryError::from)?;
            memory.protect(range, original | MemoryPermissions::WRITE)?;
            originals.push((range, original));
        }

        memory.write_u32(thunk.slot, gate_address(thunk.ordinal).0)?;

        for (range, original) in originals {
            memory.protect(range, original)?;
        }
    }

    Ok(())
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
        pages.entry(descriptor.physical_page()).or_insert(descriptor.generation());
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
            // Ordinal 187 (NtClose) is a registered but unimplemented stub.
            let code = [0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, 0xC3];
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&code, &[0x8000_00BB]))
                .expect("synthetic XBE must load");

            let stop = emulator.run(16).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::UnimplementedKernelExport { ordinal: 187 });
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
