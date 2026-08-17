//! Device MMIO dispatch for the hypervisor tier (WHP-M2).
//!
//! Hardware register blocks stay unmapped in the partition, so a guest
//! access exits with the faulting GPA. The engine then executes exactly one
//! instruction on the interpreter over an [`MmioView`]: RAM addresses reach
//! the real address space, device addresses reach the device model, and the
//! instruction's own semantics (addressing forms, read-modify-write,
//! flags) come from the interpreter rather than a hand decoder.
//!
//! The M2 device model is a stub: reads return zero, writes are ignored,
//! and both are counted per region so a stalled boot names the device it is
//! waiting on. Real GPU and APU models replace it region by region.

use std::sync::Mutex;

use exbawks_memory::{GuestMemory, MemoryError, PageTable, SoftwareAddressSpace};
use exbawks_types::GuestVa;

/// The first device address: the NV2A GPU register block.
const DEVICE_SPACE_START: u32 = 0xFD00_0000;
/// One past the last device address (flash and the gate region are above).
const DEVICE_SPACE_END: u32 = 0xFF00_0000;

/// One device region, for attribution in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRegion {
    /// The NV2A graphics processor (`0xFD00_0000`).
    Gpu,
    /// The MCPX audio processor (`0xFE80_0000`).
    Apu,
    /// The AC'97 codec (`0xFEC0_0000`).
    Ac97,
    /// Any other address inside device space.
    Other,
}

impl DeviceRegion {
    /// Classifies one device-space address.
    #[must_use]
    pub fn of(address: u32) -> Self {
        match address {
            0xFD00_0000..=0xFDFF_FFFF => Self::Gpu,
            0xFE80_0000..=0xFE87_FFFF => Self::Apu,
            0xFEC0_0000..=0xFEC0_FFFF => Self::Ac97,
            _ => Self::Other,
        }
    }
}

/// Per-region access counters.
#[derive(Debug, Default, Clone, Copy)]
struct RegionStats {
    reads: u64,
    writes: u64,
}

/// The stub device model: latched registers, counted accesses.
///
/// Writes are stored per register cell and read back verbatim — device
/// init routinely programs base addresses and sizes and rereads them, so a
/// zero-read stub corrupts pointers. Unwritten registers read zero, except
/// targeted ready-bit overrides.
#[derive(Debug, Default)]
pub struct DeviceSpace {
    stats: Mutex<[RegionStats; 4]>,
    registers: Mutex<std::collections::HashMap<u32, u32>>,
    /// Guest VA pages holding APU DSP command mailboxes, one per comm
    /// region the guest programs. The engine unmaps them so accesses route
    /// here: writes land in RAM, and the most recently written cell reads
    /// as zero (an "infinitely fast DSP" consumed it); every other cell
    /// keeps its RAM contents, so setup data in the same page survives.
    mailbox_pages: Mutex<Vec<u32>>,
    /// The most recently written mailbox cell (the FIFO head being polled).
    mailbox_head: Mutex<Option<u32>>,
    /// The claimed GPU instance region backing the `PRAMIN` window:
    /// (kernel-window VA, size). Accesses through `PRAMIN` redirect here.
    pramin: Mutex<Option<(u32, u32)>>,
    /// Latched I/O port values (VGA CRTC and friends).
    ports: Mutex<std::collections::HashMap<u16, u32>>,
    /// Pushbuffer submissions observed and not yet consumed:
    /// (channel base, get at submission, put).
    submissions: Mutex<Vec<(u32, u32, u32)>>,
}

/// The NV2A `PRAMIN` window: the GPU instance-memory aperture.
const PRAMIN_BASE: u32 = 0xFD70_0000;

/// The NV2A `USER` region: per-channel pushbuffer control (`DMA_PUT` at
/// `+0x40`, `DMA_GET` at `+0x44` within each channel window).
const NV_USER_START: u32 = 0xFD80_0000;
const NV_USER_END: u32 = 0xFDA0_0000;

/// The APU register receiving the GP comm region's physical base address.
const APU_GP_COMM_BASE: u32 = 0xFE82_0808;
/// The mailbox cell's page offset within the GP comm region.
const GP_MAILBOX_PAGE_OFFSET: u32 = 0x1000;

impl DeviceSpace {
    /// True when a whole access lies inside device space.
    #[must_use]
    pub fn contains(address: u32, len: usize) -> bool {
        let end = u64::from(address) + len as u64;
        address >= DEVICE_SPACE_START && end <= u64::from(DEVICE_SPACE_END)
    }

    fn slot(region: DeviceRegion) -> usize {
        match region {
            DeviceRegion::Gpu => 0,
            DeviceRegion::Apu => 1,
            DeviceRegion::Ac97 => 2,
            DeviceRegion::Other => 3,
        }
    }

    /// The value last written to one device register, if any.
    ///
    /// The display controller's start address lives here, and it is the
    /// only record of which surface the console would actually be
    /// scanning out.
    #[must_use]
    pub fn latched(&self, address: u32) -> Option<u32> {
        self.registers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address)
            .copied()
    }

    /// Serves one device read: the latched value, a ready-bit override, or
    /// zero.
    fn read(&self, address: u32, output: &mut [u8]) {
        output.fill(0);
        // Registers a boot path polls for hardware-set bits: a zero read
        // spins forever, so these answer all-ones ("whatever you are
        // waiting on is ready"). Refined into real device models later.
        //
        // `0xFE82_0010`: an APU GP-DSP status register DirectSound's init
        // polls after programming the global setup (observed in the retail
        // image; never written by the guest).
        //
        // `0xFEC0_0130`: the AC'97 global status register (`GLOB_STA`);
        // DirectSound's init polls it for the codec-ready bits with a
        // bounded timeout, and a timeout fails `DirectSoundCreate` — which
        // the retail image never checks, crashing on the null device.
        const READY_OVERRIDES: [u32; 2] = [0xFE82_0010, 0xFEC0_0130];
        // Registers that read as a fixed idle-hardware value: the PFIFO
        // status family reports an empty FIFO (`LOW_MARK`, bit 4) and an
        // enabled, drained DMA pusher — zeros read as "busy forever" and
        // Direct3D's drain loop never exits.
        const VALUE_OVERRIDES: [(u32, u32); 4] = [
            (0xFD00_2080, 0x0000_0010), // NV_PFIFO_RUNOUT_STATUS: empty
            (0xFD00_2400, 0x0000_0010), // NV_PFIFO_CACHE0 status: empty
            (0xFD00_3214, 0x0000_0010), // NV_PFIFO_CACHE1_STATUS: low mark
            (0xFD00_3220, 0x0000_0101), // CACHE1_DMA_PUSH: enabled + empty
        ];
        // Self-clearing command registers: the guest writes a reset bit and
        // spins until it reads back clear; real hardware clears it
        // immediately, so latching the write would spin forever. The AC'97
        // busmaster channel control registers (`CR` at `0xFEC001n0 + 0xB`
        // for each channel, the SPDIF channel at `+0x70` included) are the
        // observed cases.
        let ac97_channel_control =
            (0xFEC0_0100..=0xFEC0_01FF).contains(&address) && address & 0xF == 0xB;
        // `0xFD10_0410`: an NV2A PFB (memory controller) register D3D
        // writes a trigger bit into and polls until it self-clears.
        //
        // Interrupt-status registers are write-1-to-clear: the guest ACKs
        // by writing all-ones, so a latch would report every interrupt
        // pending forever — Direct3D then loops through its PGRAPH
        // exception handler on phantom errors. With no interrupt sources
        // modeled, status reads back clear: PMC `0xFD00_0100`, PFIFO
        // `0xFD00_2100`, PTIMER `0xFD00_9100`, PGRAPH `0xFD40_0100`, and
        // PCRTC `0xFD60_0100`.
        const SELF_CLEAR: [u32; 6] =
            [0xFD10_0410, 0xFD00_0100, 0xFD00_2100, 0xFD00_9100, 0xFD40_0100, 0xFD60_0100];
        if READY_OVERRIDES.contains(&address) {
            output.fill(0xFF);
        } else if let Some((_, value)) = VALUE_OVERRIDES.iter().find(|(fixed, _)| *fixed == address)
        {
            let bytes = value.to_le_bytes();
            let take = output.len().min(4);
            output[..take].copy_from_slice(&bytes[..take]);
        } else if ac97_channel_control || SELF_CLEAR.contains(&address) {
            // Already zero-filled: the command completed instantly.
        } else {
            let registers =
                self.registers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(value) = registers.get(&address) {
                let bytes = value.to_le_bytes();
                let take = output.len().min(4);
                output[..take].copy_from_slice(&bytes[..take]);
            }
        }
        let region = DeviceRegion::of(address);
        let mut stats = self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stats[Self::slot(region)].reads += 1;
        tracing::trace!(?region, address = format_args!("{address:#010x}"), "MMIO read");
    }

    /// Accepts one device write, latching it for readback.
    fn write(&self, address: u32, input: &[u8]) {
        let mut bytes = [0_u8; 4];
        let take = input.len().min(4);
        bytes[..take].copy_from_slice(&input[..take]);
        let value = u32::from_le_bytes(bytes);
        // A pushbuffer submission: the guest writes DMA_PUT (offset 0x40
        // within a 64 KiB USER channel window) and polls DMA_GET until the
        // GPU catches up. The stub GPU is infinitely fast: GET snaps to PUT
        // the moment PUT is written. The submitted range's commands live in
        // guest RAM for the graphics frontend to consume once it exists.
        if (NV_USER_START..NV_USER_END).contains(&address) && address & 0xFFFF == 0x40 {
            let get_register = address + 4;
            let mut registers =
                self.registers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // The walk starts wherever GET last pointed (the guest programs
            // it at channel setup); the engine consumes [get, put) and the
            // readback then reports the infinitely fast GPU caught up.
            let get_before = registers.get(&get_register).copied().unwrap_or(value);
            registers.insert(get_register, value);
            drop(registers);
            self.submissions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((
                address & !0xFFFF,
                get_before,
                value,
            ));
            tracing::debug!(
                put = format_args!("{value:#010x}"),
                get = format_args!("{get_before:#010x}"),
                channel = format_args!("{:#010x}", address & !0xFFFF),
                "pushbuffer submitted"
            );
        }
        if address == APU_GP_COMM_BASE && value != 0 {
            // A comm region's physical base: its mailbox page (in the
            // cached kernel window) becomes an instant-consumer mailbox.
            let page = (0x8000_0000 | value).wrapping_add(GP_MAILBOX_PAGE_OFFSET) & !0xFFF;
            let mut pages =
                self.mailbox_pages.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !pages.contains(&page) {
                pages.push(page);
                tracing::debug!(
                    page = format_args!("{page:#010x}"),
                    "APU DSP mailbox page identified"
                );
            }
        }
        self.registers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(address, value);
        let region = DeviceRegion::of(address);
        let mut stats = self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stats[Self::slot(region)].writes += 1;
        tracing::trace!(
            ?region,
            address = format_args!("{address:#010x}"),
            value = format_args!("{value:#x}"),
            "MMIO write"
        );
    }

    /// Serves one port read: the latched value, or zero when unwritten.
    pub fn port_read(&self, port: u16) -> u32 {
        let value = self
            .ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&port)
            .copied()
            .unwrap_or(0);
        tracing::trace!(port = format_args!("{port:#06x}"), value, "port read");
        value
    }

    /// Accepts one port write, latching it for readback.
    pub fn port_write(&self, port: u16, value: u32) {
        tracing::trace!(port = format_args!("{port:#06x}"), value, "port write");
        self.ports.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(port, value);
    }

    /// Backs the `PRAMIN` window with the claimed instance region.
    pub fn set_pramin(&self, base_va: u32, size: u32) {
        *self.pramin.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((base_va, size));
    }

    /// Translates a `PRAMIN`-window address to its backing RAM VA.
    fn pramin_target(&self, address: u32, len: usize) -> Option<u32> {
        let (base_va, size) =
            (*self.pramin.lock().unwrap_or_else(std::sync::PoisonError::into_inner))?;
        let offset = address.checked_sub(PRAMIN_BASE)?;
        if u64::from(offset) + len as u64 <= u64::from(size) {
            Some(base_va + offset)
        } else {
            None
        }
    }

    /// Drains the pushbuffer submissions observed since the last call.
    pub fn take_submissions(&self) -> Vec<(u32, u32, u32)> {
        std::mem::take(
            &mut self.submissions.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The latched value of one device register, when the guest wrote it.
    pub fn register_value(&self, address: u32) -> Option<u32> {
        self.registers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address)
            .copied()
    }

    /// The GPU instance region backing `PRAMIN`: (window VA, size).
    pub fn pramin_region(&self) -> Option<(u32, u32)> {
        *self.pramin.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The identified DSP mailbox pages.
    #[must_use]
    pub fn mailbox_pages(&self) -> Vec<u32> {
        self.mailbox_pages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// True when a data fault at this address routes to the device model.
    pub fn routes(&self, address: u32) -> bool {
        Self::contains(address, 1) || self.in_mailbox(address, 1)
    }

    /// Records the most recent mailbox write and reports consumption.
    fn consume_mailbox_write(&self, address: u32) {
        *self.mailbox_head.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(address);
    }

    /// True when this cell is the consumed FIFO head.
    fn mailbox_consumed(&self, address: u32) -> bool {
        *self.mailbox_head.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            == Some(address)
    }

    /// True when an access falls inside an identified mailbox page.
    fn in_mailbox(&self, address: u32, len: usize) -> bool {
        let end = u64::from(address) + len as u64;
        self.mailbox_pages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|page| address >= *page && end <= u64::from(*page) + 0x1000)
    }

    /// Renders the access counters for diagnostics.
    #[must_use]
    pub fn summary(&self) -> String {
        let stats = self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        format!(
            "gpu {}r/{}w, apu {}r/{}w, ac97 {}r/{}w, other {}r/{}w",
            stats[0].reads,
            stats[0].writes,
            stats[1].reads,
            stats[1].writes,
            stats[2].reads,
            stats[2].writes,
            stats[3].reads,
            stats[3].writes,
        )
    }
}

/// A guest-memory view that overlays device space onto RAM.
pub struct MmioView<'a> {
    ram: &'a SoftwareAddressSpace,
    devices: &'a DeviceSpace,
}

impl<'a> MmioView<'a> {
    /// Builds a view over one address space and device model.
    #[must_use]
    pub fn new(ram: &'a SoftwareAddressSpace, devices: &'a DeviceSpace) -> Self {
        Self { ram, devices }
    }
}

impl GuestMemory for MmioView<'_> {
    fn read(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError> {
        if let Some(target) = self.devices.pramin_target(address.0, output.len()) {
            return self.ram.read(GuestVa(target), output);
        }
        if self.devices.in_mailbox(address.0, output.len()) {
            if self.devices.mailbox_consumed(address.0) {
                // The infinitely fast DSP consumed the FIFO head.
                output.fill(0);
                return Ok(());
            }
            return self.ram.read(address, output);
        }
        if DeviceSpace::contains(address.0, output.len()) {
            self.devices.read(address.0, output);
            return Ok(());
        }
        self.ram.read(address, output)
    }

    fn fetch(&self, address: GuestVa, output: &mut [u8]) -> Result<(), MemoryError> {
        // Code never executes out of device registers.
        self.ram.fetch(address, output)
    }

    fn write(&self, address: GuestVa, input: &[u8]) -> Result<(), MemoryError> {
        if let Some(target) = self.devices.pramin_target(address.0, input.len()) {
            return self.ram.write(GuestVa(target), input);
        }
        if self.devices.in_mailbox(address.0, input.len()) {
            // The write lands, and the cell becomes the consumed FIFO head.
            self.devices.consume_mailbox_write(address.0);
            return self.ram.write(address, input);
        }
        if DeviceSpace::contains(address.0, input.len()) {
            self.devices.write(address.0, input);
            return Ok(());
        }
        self.ram.write(address, input)
    }

    fn page_table(&self) -> &PageTable {
        self.ram.page_table()
    }
}

#[cfg(test)]
mod tests {
    use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, MemoryPermissions};

    use super::*;

    #[test]
    fn device_space_bounds_are_exact() {
        assert!(DeviceSpace::contains(0xFD00_0000, 4));
        assert!(DeviceSpace::contains(0xFE80_0200, 4));
        assert!(DeviceSpace::contains(0xFEFF_FFFC, 4));
        assert!(!DeviceSpace::contains(0xFCFF_FFFC, 4), "below the GPU block");
        assert!(!DeviceSpace::contains(0xFEFF_FFFE, 4), "straddles the top");
        assert!(!DeviceSpace::contains(0xFF80_0000, 4), "the gate region is not a device");
    }

    #[test]
    fn regions_classify_and_count() {
        assert_eq!(DeviceRegion::of(0xFD00_1234), DeviceRegion::Gpu);
        assert_eq!(DeviceRegion::of(0xFE80_0200), DeviceRegion::Apu);
        assert_eq!(DeviceRegion::of(0xFEC0_0010), DeviceRegion::Ac97);
        assert_eq!(DeviceRegion::of(0xFE00_0000), DeviceRegion::Other);
    }

    #[test]
    fn the_view_routes_devices_and_ram() {
        let ram = SoftwareAddressSpace::new(64 * 1024).expect("memory is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), u64::from(GUEST_PAGE_SIZE))
            .expect("range is valid");
        ram.map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)
            .expect("mapping succeeds");
        let devices = DeviceSpace::default();
        let view = MmioView::new(&ram, &devices);

        // RAM passes through and persists.
        view.write_u32(GuestVa(0x1000), 0x1234_5678).expect("RAM write");
        assert_eq!(view.read_u32(GuestVa(0x1000)).unwrap(), 0x1234_5678);

        // Device writes latch for readback; unwritten registers read zero.
        view.write_u32(GuestVa(0xFE80_0200), 0xDEAD_BEEF).expect("device write");
        assert_eq!(view.read_u32(GuestVa(0xFE80_0200)).unwrap(), 0xDEAD_BEEF, "latched");
        assert_eq!(view.read_u32(GuestVa(0xFE80_0300)).unwrap(), 0, "unwritten reads zero");
        assert!(devices.summary().contains("apu 2r/1w"), "{}", devices.summary());
    }
}
