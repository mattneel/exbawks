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
    BackendKind, GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions, StopReason,
};
use exbawks_xbe::{XbeImage, XbeSection, XbeSectionFlags};

use crate::{BootPlanReport, CoreError, EmulatorConfig, KernelThunkTable, LoadedImage};

/// The outcome of one kernel gate call attempt at the current guest EIP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAssist {
    /// One export ran; execution resumes at the address.
    Dispatched {
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
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - address.page_offset())
            .map_err(|_| CoreError::InvalidConfiguration("guest page size does not fit usize"))?;
        let fetch_len = page_remaining.min(15);
        let mut bytes = [0_u8; 15];
        self.memory.fetch(address, &mut bytes[..fetch_len])?;

        let decoder =
            BasicBlockDecoder::new(DecodeConfig { max_instructions: 1, max_bytes: fetch_len });
        let block = decoder.decode(address, &bytes[..fetch_len])?;
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

        Ok(GateAssist::Dispatched { resume: GuestVa(return_address), status, stop })
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
        let page_remaining = usize::try_from(GUEST_PAGE_SIZE - start.page_offset())
            .map_err(|_| CoreError::InvalidConfiguration("guest page size does not fit usize"))?;
        let fetch_len = self.config.max_block_bytes.min(page_remaining);
        let mut bytes = vec![0_u8; fetch_len];
        self.memory.fetch(start, &mut bytes)?;

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

fn map_xbe(memory: &SoftwareAddressSpace, image: &XbeImage, bytes: &[u8]) -> Result<(), CoreError> {
    let header_size = usize::try_from(image.header.size_of_headers)
        .map_err(|_| CoreError::InvalidConfiguration("XBE header size does not fit usize"))?;
    memory.load_region(
        image.header.base_address,
        image.header.size_of_headers,
        &bytes[..header_size],
        MemoryPermissions::READ,
    )?;

    for section in &image.sections {
        map_section(memory, image, bytes, section)?;
    }

    Ok(())
}

fn map_section(
    memory: &SoftwareAddressSpace,
    image: &XbeImage,
    bytes: &[u8],
    section: &XbeSection,
) -> Result<(), CoreError> {
    if section.virtual_size == 0 {
        return Ok(());
    }
    if section.virtual_address.page_offset() != 0 {
        return Err(CoreError::UnalignedSection {
            section_index: section.index,
            address: section.virtual_address,
        });
    }
    if section.raw_size > section.virtual_size {
        return Err(CoreError::SectionRawExceedsVirtual {
            section_index: section.index,
            raw_size: section.raw_size,
            virtual_size: section.virtual_size,
        });
    }

    let initial = image.section_data(bytes, section)?;
    let mut permissions = MemoryPermissions::READ;
    if section.flags.contains(XbeSectionFlags::WRITABLE) {
        permissions |= MemoryPermissions::WRITE;
    }
    if section.flags.contains(XbeSectionFlags::EXECUTABLE) {
        permissions |= MemoryPermissions::EXECUTE;
    }

    memory.load_region(section.virtual_address, section.virtual_size, initial, permissions)?;
    apply_section_page_permissions(memory, section, permissions)?;
    Ok(())
}

fn apply_section_page_permissions(
    memory: &SoftwareAddressSpace,
    section: &XbeSection,
    permissions: MemoryPermissions,
) -> Result<(), CoreError> {
    if !permissions.contains(MemoryPermissions::WRITE) {
        return Ok(());
    }

    let page_size = u64::from(GUEST_PAGE_SIZE);
    let rounded_size = (u64::from(section.virtual_size) + page_size - 1) & !(page_size - 1);
    let page_count = rounded_size / page_size;
    let read_only = permissions & !MemoryPermissions::WRITE;

    if section.flags.contains(XbeSectionFlags::HEAD_PAGE_READ_ONLY) {
        let range = GuestRange::page_aligned(section.virtual_address, page_size)
            .map_err(exbawks_memory::MemoryError::from)?;
        memory.protect(range, read_only)?;
    }

    if section.flags.contains(XbeSectionFlags::TAIL_PAGE_READ_ONLY) {
        let tail_offset = u32::try_from((page_count - 1) * page_size)
            .map_err(|_| CoreError::InvalidConfiguration("section tail offset overflow"))?;
        let tail = section
            .virtual_address
            .checked_add(tail_offset)
            .ok_or(CoreError::InvalidConfiguration("section tail address overflow"))?;
        let range =
            GuestRange::page_aligned(tail, page_size).map_err(exbawks_memory::MemoryError::from)?;
        memory.protect(range, read_only)?;
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
                context.cpu.gpr[0] = 0xFEED_F00D;
                KernelStatus::SUCCESS
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
                    resume: GuestVa(0x0001_1006),
                    status: KernelStatus::SUCCESS,
                    stop: None
                }
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(emulator.cpu().eip, 0x0001_1006);
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
        /// 0x11000  mov eax, 5
        /// 0x11005  add eax, 0x25        ; translated work before the call
        /// 0x11008  call [0x11200]       ; DbgPrint through the first gate
        /// 0x1100E  mov ebx, eax         ; translated work after the return
        /// 0x11010  call [0x11204]       ; HalReturnToFirmware exits
        /// 0x11016  ret
        /// ```
        const BOOT_CODE: [u8; 23] = [
            0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
            0x83, 0xC0, 0x25, // add eax, 0x25
            0xFF, 0x15, 0x00, 0x12, 0x01, 0x00, // call [0x11200]
            0x89, 0xC3, // mov ebx, eax
            0xFF, 0x15, 0x04, 0x12, 0x01, 0x00, // call [0x11204]
            0xC3, // ret
        ];

        const BOOT_THUNKS: [u32; 2] = [0x8000_0008, 0x8000_0031];

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
            assert_eq!(emulator.cpu().gpr[0], 42, "translated arithmetic ran before the call");
            assert_eq!(emulator.cpu().gpr[3], 42, "translated code ran after the HLE return");
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
        fn zero_budget_reports_exhaustion() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&BOOT_CODE, &BOOT_THUNKS))
                .expect("synthetic XBE must load");

            let stop = emulator.run(0).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::BudgetExhausted);
        }

        #[cfg(not(windows))]
        #[test]
        fn portable_hosts_stop_with_runtime_incomplete() {
            let mut emulator = Emulator::new().expect("emulator must initialize");
            emulator
                .load_xbe(synthetic_xbe_with(&BOOT_CODE, &BOOT_THUNKS))
                .expect("synthetic XBE must load");

            let stop = emulator.run(16).expect("the run must stop cleanly");
            assert_eq!(stop, StopReason::RuntimeIncomplete);
        }
    }
}
