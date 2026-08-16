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
    /// here: writes are consumed instantly (an "infinitely fast DSP"),
    /// reads report the consumed (zero) state.
    mailbox_pages: Mutex<Vec<u32>>,
}

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
        // Self-clearing command registers: the guest writes a reset bit and
        // spins until it reads back clear; real hardware clears it
        // immediately, so latching the write would spin forever. The AC'97
        // busmaster channel control registers (`CR` at `0xFEC001n0 + 0xB`
        // for each channel, the SPDIF channel at `+0x70` included) are the
        // observed cases.
        let ac97_channel_control =
            (0xFEC0_0100..=0xFEC0_01FF).contains(&address) && address & 0xF == 0xB;
        if READY_OVERRIDES.contains(&address) {
            output.fill(0xFF);
        } else if ac97_channel_control {
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

    /// The identified DSP mailbox pages.
    #[must_use]
    pub fn mailbox_pages(&self) -> Vec<u32> {
        self.mailbox_pages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// True when a data fault at this address routes to the device model.
    pub fn routes(&self, address: u32) -> bool {
        Self::contains(address, 1) || self.in_mailbox(address, 1)
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
        if self.devices.in_mailbox(address.0, output.len()) {
            // The infinitely fast DSP already consumed everything.
            output.fill(0);
            return Ok(());
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
        if self.devices.in_mailbox(address.0, input.len()) {
            // Commands are consumed the instant they are written.
            return Ok(());
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
