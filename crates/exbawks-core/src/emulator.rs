use std::collections::BTreeMap;
use std::sync::Arc;

use exbawks_cpu::{BasicBlockDecoder, CpuState, DecodeConfig, DecodedBlock};
use exbawks_debug::{NoopTrace, TraceEvent, TraceSink};
use exbawks_gpu::{GraphicsFrontend, NullGraphicsBackend};
use exbawks_jit::{
    BlockKey, CodeCache, CodegenBackend, CompiledBlock, CraneliftBackend, DirectRewriteBackend,
    PhysicalPageDependency,
};
use exbawks_kernel::KernelRegistry;
use exbawks_memory::{GuestMemory, PageKind, SoftwareAddressSpace};
use exbawks_types::{BackendKind, GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions};
use exbawks_xbe::{XbeImage, XbeSection, XbeSectionFlags};

use crate::{BootPlanReport, CoreError, EmulatorConfig, LoadedImage};

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

        Ok(Emulator {
            config: self.config,
            memory,
            cpu: CpuState::default(),
            kernel: KernelRegistry::new(),
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

        self.cpu = CpuState { eip: image.header.entry_point.0, ..CpuState::default() };
        self.memory = memory;
        self.code_cache.clear();
        self.address_space_epoch = self.address_space_epoch.wrapping_add(1);

        let loaded = Arc::new(LoadedImage::new(image, bytes));
        self.loaded = Some(loaded.clone());
        Ok(loaded)
    }

    /// Decodes and plans the current entry block.
    pub fn plan_entry_block(&self) -> Result<EntryBlockPlan, CoreError> {
        let image = self.loaded.clone().ok_or(CoreError::NoImageLoaded)?;
        let start = GuestVa(self.cpu.eip);
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
            return Ok(EntryBlockPlan { image, decoded, compiled });
        }

        let dependencies = physical_dependencies(self.memory.page_table(), &decoded)?;
        let compiled = Arc::new(self.backend.compile(&decoded)?);
        self.code_cache.insert(key, compiled.clone(), dependencies);

        Ok(EntryBlockPlan { image, decoded, compiled })
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
        let mut bytes = vec![0_u8; 0x282];
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
        write_u32(&mut bytes, 0x208, 2);
        write_u32(&mut bytes, 0x20C, 0x280);
        write_u32(&mut bytes, 0x210, 2);
        write_u32(&mut bytes, 0x214, base + 0x238);
        bytes[0x238..0x23E].copy_from_slice(b".text\0");
        bytes[0x280..0x282].copy_from_slice(&[0x90, 0xC3]);
        bytes
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
