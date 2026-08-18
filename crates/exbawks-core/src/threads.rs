//! The guest thread table and kernel service implementation (ADR 0011/0012).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use exbawks_cpu::{CpuState, Segment, SegmentState};
use exbawks_kernel::{
    DisplayMode, FileInfo, FileOpenRequest, FileOpened, KernelServiceError, KernelServices,
    ThreadCreateRequest, ThreadCreated, VirtualAllocRequest, VirtualAllocation, WaitOutcome,
};
use exbawks_memory::{GuestMemory, MemoryError, SoftwareAddressSpace};
use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions};

use crate::hostfs::HostFileSystem;

/// The reserved return address for guest thread start routines.
///
/// It sits in the reserved region above the kernel gate range, so
/// `gate_ordinal` never resolves it to a real export; the run loop treats
/// execution arriving here as an implicit thread exit whose status is EAX
/// (ADR 0011).
pub(crate) const THREAD_EXIT_SENTINEL: GuestVa = GuestVa(0xFFBF_FFF4);

/// The base of the cached physical window (ADR 0010): `VA = 0x8000_0000 |
/// PA`, aliasing all of physical RAM. Kernel blocks are physical
/// allocations whose virtual address is derived through this identity, so
/// `MmGetPhysicalAddress` and GPU-visible physical pointers are truthful.
const KERNEL_WINDOW_BASE: u32 = 0x8000_0000;

/// The first virtual address the user-space allocator hands out (ADR 0010).
///
/// `NtAllocateVirtualMemory` results live in the user range
/// (`0x0001_0000`–`0x7FFF_FFFF`). This base sits above the mapped image and
/// boot stack (which reach into the low 64 MiB) and well below the kernel
/// region, so kernel-chosen allocations never collide with either.
const USER_ALLOC_BASE: u32 = 0x1000_0000;

/// One past the last address the user allocator may hand out, leaving
/// headroom below the `0x8000_0000` kernel boundary.
const USER_ALLOC_END: u32 = 0x7F00_0000;

/// The Win32 `MEM_COMMIT` allocation-type flag: back the range with pages.
const MEM_COMMIT: u32 = 0x0000_1000;

/// The KTHREAD block offset inside each thread's KPCR page.
const KTHREAD_OFFSET: u32 = 0x200;
/// The largest TLS template or zero-fill the loader honors; the sizes are
/// guest-controlled header fields, so they are bounded.
const MAX_TLS_BYTES: u32 = 1024 * 1024;

/// The image's `IMAGE_TLS_DIRECTORY` contents (ADR 0010 thread layout).
///
/// On the Xbox, thread-local storage lives at the **top of each thread's
/// stack**: the XDK CRT computes `_tls_index = -(aligned_tls_size / 4)` and
/// reads its block through `[fs:[4] + _tls_index * 4]`, i.e. at a negative
/// offset below `NtTib.StackBase`. XAPI's own guest code claims and fills
/// that region; the emulator's job is to keep the initial stack pointer
/// *below* it so ordinary pushes never clobber it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TlsTemplate {
    /// The guest VA of the initialized TLS data's first byte.
    pub raw_start: u32,
    /// One past the initialized TLS data's last byte.
    pub raw_end: u32,
    /// Zero bytes appended after the initialized data.
    pub zero_fill: u32,
}
/// The embedded KPRCB offset inside the KPCR (Xbox layout); its first field
/// is `CurrentThread`, and `KPCR.Prcb` (`fs:[0x20]`) points here.
const PRCB_OFFSET: u32 = 0x28;
/// The minimum guest stack size in bytes.
const MINIMUM_STACK_BYTES: u32 = 16 * 1024;
/// Scratch bytes kept above a new thread's initial stack pointer.
const STACK_SCRATCH: u32 = 16;

/// One guest thread's schedulable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadState {
    /// Eligible to run.
    Ready,
    /// The active thread.
    Running,
    /// Created suspended; a resume makes it ready.
    Suspended,
    /// Parked until the named handle signals (ADR 0017).
    Waiting(u32),
    /// Finished; never scheduled again.
    Terminated,
}

/// One guest thread record.
#[derive(Debug)]
pub(crate) struct GuestThread {
    /// The saved CPU context while the thread is not running.
    pub cpu: CpuState,
    /// The schedulable state.
    pub state: ThreadState,
    /// The guest-visible thread identifier.
    ///
    /// Reserved for the full scheduler's thread lookups (M4); the minimal
    /// form does not read it back.
    #[allow(dead_code)]
    pub id: u32,
    /// The KPCR page address (diagnostics; the fs base mirrors it).
    pub kpcr: GuestVa,
    /// True when a return to a null address is this thread's intended exit.
    ///
    /// The boot thread runs the entry with no set-up return address, so its
    /// terminal `ret` reaches null on purpose; created threads return to the
    /// exit sentinel, so a null there is a genuine fault, not an exit.
    pub exits_on_null_return: bool,
}

/// A scheduling effect recorded during a kernel call (ADR 0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingAction {
    /// The calling thread exits with a status.
    Exit {
        /// The guest exit status.
        status: u32,
    },
    /// The calling thread parks until the handle signals (ADR 0017).
    Wait {
        /// The awaited guest handle.
        handle: u32,
    },
}

/// The guest thread table, kernel-object allocator, and service surface.
pub(crate) struct ThreadManager {
    memory: Arc<SoftwareAddressSpace>,
    threads: Vec<GuestThread>,
    current: usize,
    pending: Option<PendingAction>,
    /// The next free user-space address `NtAllocateVirtualMemory` hands out
    /// for a kernel-chosen (`BaseAddress == 0`) allocation. A forward bump
    /// until the real VA region map lands (MEM-007).
    user_cursor: u32,
    /// Open guest handles (minimal object table; the full object manager is
    /// HLE-005). Thread creation registers its handle here.
    handles: HashSet<u32>,
    /// The host-backed file device (ADR 0014).
    files: HostFileSystem,
    /// Deferred procedure calls an interrupt routine has queued, in the
    /// order they were queued.
    dpc_queue: Vec<u32>,
    /// The interrupt service routines the title has registered, and
    /// whether each is currently connected. A device that raises an
    /// interrupt calls the connected routine for its vector.
    interrupts: Vec<(exbawks_kernel::InterruptRoutine, bool)>,
    /// Regions the guest marked to survive a soft reboot (ADR 0015):
    /// `(base, size)` pairs from `MmPersistContiguousMemory`.
    persisted: Vec<(u32, u32)>,
    /// The guest address of the `LaunchDataPage` variable cell (data ordinal
    /// 164), captured when the kernel variables are built, or `None` when the
    /// image does not import it.
    launch_data_page_cell: Option<GuestVa>,
    /// The guest address of the `KeTickCount` variable cell (data ordinal
    /// 156), which the run loop advances as the virtual clock (KRN-003).
    tick_count_cell: Option<GuestVa>,
    /// The image's TLS directory, when it has one; each thread gets a block
    /// built from this template.
    tls_template: Option<TlsTemplate>,
    /// Byte sizes of contiguous/pool allocations (base → page-rounded size).
    pool_sizes: HashMap<u32, u32>,
    /// The claimed GPU instance-memory region (base, page-rounded size).
    gpu_instance: Option<(GuestVa, u32)>,
    /// Guest event objects (handle → (manual-reset, signaled)). Under the
    /// cooperative scheduler (ADR 0011) events never block; the state exists
    /// for the guest's own signal/query bookkeeping.
    events: HashMap<u32, (bool, bool)>,
    /// Guest mutant objects (handle → owner thread index and recursion
    /// count, `None` when unowned).
    mutants: HashMap<u32, Option<(usize, u32)>>,
    /// The most recent display mode the title programmed, if any.
    display_mode: Option<DisplayMode>,
}

/// The first guest handle the event table hands out; disjoint from file
/// (`0x100`+) and thread (`0xE000`+) handles.
const EVENT_HANDLE_BASE: u32 = 0x0000_A000;
/// The first guest handle the mutant table hands out.
const MUTANT_HANDLE_BASE: u32 = 0x0000_B000;

/// The data-export ordinal of the kernel `LaunchDataPage` pointer variable.
const LAUNCH_DATA_PAGE_ORDINAL: u16 = 164;
/// The data-export ordinal of the kernel `KeTickCount` variable.
const KE_TICK_COUNT_ORDINAL: u16 = 156;

impl ThreadManager {
    pub(crate) fn new(
        memory: Arc<SoftwareAddressSpace>,
        disc_root: Option<PathBuf>,
        hdd_root: Option<PathBuf>,
    ) -> Self {
        Self {
            memory,
            threads: Vec::new(),
            current: 0,
            pending: None,
            user_cursor: USER_ALLOC_BASE,
            handles: HashSet::new(),
            files: HostFileSystem::new(disc_root, hdd_root),
            interrupts: Vec::new(),
            dpc_queue: Vec::new(),
            persisted: Vec::new(),
            launch_data_page_cell: None,
            tick_count_cell: None,
            tls_template: None,
            pool_sizes: HashMap::new(),
            gpu_instance: None,
            events: HashMap::new(),
            mutants: HashMap::new(),
            display_mode: None,
        }
    }

    /// The claimed GPU instance-memory region, when one exists.
    pub(crate) fn gpu_instance(&self) -> Option<(GuestVa, u32)> {
        self.gpu_instance
    }

    /// Rotates to the next ready thread, round-robin (ADR 0017).
    ///
    /// Saves the active thread's context from `cpu` and loads the next
    /// ready thread's context into it. Returns `false` (leaving `cpu`
    /// untouched) when no other thread is ready to run.
    pub(crate) fn rotate_active(&mut self, cpu: &mut CpuState) -> bool {
        let count = self.threads.len();
        if count < 2 {
            return false;
        }
        let next = (1..count)
            .map(|offset| (self.current + offset) % count)
            .find(|index| self.threads[*index].state == ThreadState::Ready);
        let Some(next) = next else {
            return false;
        };

        self.threads[self.current].cpu = cpu.clone();
        if self.threads[self.current].state == ThreadState::Running {
            self.threads[self.current].state = ThreadState::Ready;
        }
        self.threads[next].state = ThreadState::Running;
        *cpu = self.threads[next].cpu.clone();
        self.current = next;
        true
    }

    /// Returns the guest address of the `KeTickCount` cell, when imported.
    pub(crate) fn tick_count_cell(&self) -> Option<GuestVa> {
        self.tick_count_cell
    }

    /// Supplies the image's TLS directory before threads are created.
    pub(crate) fn set_tls_template(&mut self, template: TlsTemplate) {
        self.tls_template = Some(template);
    }

    /// Bytes to reserve for TLS at the top of each thread's stack.
    ///
    /// Mirrors the XDK CRT's size computation — `align16(data + zero_fill) +
    /// 4` — with slack, so the region the CRT claims below `StackBase` sits
    /// wholly above the initial stack pointer.
    pub(crate) fn tls_reserve_bytes(&self) -> u32 {
        let Some(template) = self.tls_template else {
            return 0;
        };
        let data = template.raw_end.saturating_sub(template.raw_start).min(MAX_TLS_BYTES);
        let zero = template.zero_fill.min(MAX_TLS_BYTES);
        let total = data.saturating_add(zero);
        (total.saturating_add(15) & !15).saturating_add(16)
    }

    /// Returns the guest address of the `LaunchDataPage` variable cell, when
    /// the image imports it (ADR 0015).
    pub(crate) fn launch_data_page_cell(&self) -> Option<GuestVa> {
        self.launch_data_page_cell
    }

    /// Returns the regions the guest marked to survive a soft reboot.
    pub(crate) fn persisted_regions(&self) -> &[(u32, u32)] {
        &self.persisted
    }

    /// Builds the boot thread's KPCR/KTHREAD pages and registers thread one.
    ///
    /// Returns the KPCR address the caller wires into the active `fs` base.
    pub(crate) fn create_boot_environment(
        &mut self,
        stack_base: GuestVa,
        stack_bytes: u32,
    ) -> Result<GuestVa, MemoryError> {
        let kpcr = self.build_thread_pages(stack_base, stack_bytes)?;
        self.threads.push(GuestThread {
            cpu: CpuState::default(),
            state: ThreadState::Running,
            id: 1,
            kpcr,
            exits_on_null_return: true,
        });
        self.current = 0;
        Ok(kpcr)
    }

    /// Allocates one page-rounded kernel block and returns its range.
    ///
    /// The block is a physical allocation; its virtual address is the
    /// window identity `0x8000_0000 | PA` (ADR 0010), which the window
    /// alias already maps, so `MmGetPhysicalAddress` and GPU-visible
    /// physical pointers hold trivially.
    fn allocate_kernel_block(&mut self, bytes: u32) -> Result<GuestRange, MemoryError> {
        let page = GUEST_PAGE_SIZE;
        let rounded = bytes.max(1).div_ceil(page).saturating_mul(page);
        let physical = self.memory.allocate_physical(rounded)?;
        let base = KERNEL_WINDOW_BASE | physical.0;
        let range = GuestRange::new(GuestVa(base), u64::from(rounded))?;
        // The guest can write any physical page through the window before
        // it is allocated, so a fresh block is scrubbed to keep the zeroed
        // invariant KPCR/TLS construction relies on.
        let zeros = vec![0_u8; rounded as usize];
        self.memory.write(GuestVa(base), &zeros)?;
        Ok(range)
    }

    /// Burns one physical page of spacing between kernel blocks.
    ///
    /// Under the window alias every physical page is mapped, so this no
    /// longer faults an overflow (the ADR 0010 guard property); it keeps
    /// the neighboring block out of the first overflowing write's way.
    fn reserve_guard_page(&mut self) {
        let _ = self.memory.allocate_physical(GUEST_PAGE_SIZE);
    }

    /// Builds the kernel-variables region backing the DATA exports (ADR 0010).
    ///
    /// Each imported data ordinal gets a readable cell so the guest can
    /// dereference the patched slot without faulting; ordinals with defined
    /// semantics are initialized and the rest stay zero. Returns the guest
    /// address of each variable for the loader's thunk patching.
    pub(crate) fn build_kernel_variables(
        &mut self,
        data_ordinals: &[u16],
        image_name: &str,
    ) -> Result<BTreeMap<u16, GuestVa>, MemoryError> {
        let mut variables = BTreeMap::new();
        if data_ordinals.is_empty() {
            return Ok(variables);
        }

        // One 64-byte cell per ordinal (room for the largest struct), then
        // the image-name string the XeImageFileName ANSI_STRING points at.
        const CELL: u32 = 64;
        let cells = u32::try_from(data_ordinals.len()).unwrap_or(u32::MAX);
        let name_len = u32::try_from(image_name.len()).unwrap_or(0).min(0xFF);
        let region_bytes = cells.saturating_mul(CELL).saturating_add(name_len).saturating_add(CELL);
        let region = self.allocate_kernel_block(region_bytes)?;
        let base = region.start().0;
        let name_addr = base.wrapping_add(cells.saturating_mul(CELL));
        self.memory.write(GuestVa(name_addr), &image_name.as_bytes()[..name_len as usize])?;

        for (index, &ordinal) in data_ordinals.iter().enumerate() {
            let cell = GuestVa(base.wrapping_add(index as u32 * CELL));
            self.initialize_kernel_variable(ordinal, cell, name_addr, name_len as u16)?;
            if ordinal == LAUNCH_DATA_PAGE_ORDINAL {
                self.launch_data_page_cell = Some(cell);
            }
            if ordinal == KE_TICK_COUNT_ORDINAL {
                self.tick_count_cell = Some(cell);
            }
            variables.insert(ordinal, cell);
        }
        Ok(variables)
    }

    /// Writes the defined initial value of one kernel variable; cells are
    /// zero from the fresh anonymous mapping, so unlisted ordinals need none.
    fn initialize_kernel_variable(
        &self,
        ordinal: u16,
        cell: GuestVa,
        name_addr: u32,
        name_len: u16,
    ) -> Result<(), MemoryError> {
        // Ordinals from the verified export table (docs/whp-notes.md and the
        // ADR 0010 data-export set).
        match ordinal {
            // KeTimeIncrement: clock interrupt interval in 100 ns units (1 ms).
            157 => self.memory.write_u32(cell, 10_000)?,
            // XboxKrnlVersion: {Major, Minor, Build, Qfe}.
            324 => {
                for (offset, value) in [(0_u32, 1_u16), (2, 0), (4, 5838), (6, 1)] {
                    self.memory.write(GuestVa(cell.0 + offset), &value.to_le_bytes())?;
                }
            }
            // XeImageFileName: ANSI_STRING {Length, MaximumLength, Buffer}.
            326 => {
                self.memory.write(cell, &name_len.to_le_bytes())?;
                self.memory.write(GuestVa(cell.0 + 2), &name_len.to_le_bytes())?;
                self.memory.write_u32(GuestVa(cell.0 + 4), name_addr)?;
            }
            // KeTickCount, LaunchDataPage (NULL), object types, keys, and the
            // rest read as zero, which is the correct cold-boot value.
            _ => {}
        }
        Ok(())
    }

    /// Builds one KPCR page (KTHREAD embedded) describing a stack and TLS.
    fn build_thread_pages(
        &mut self,
        stack_base: GuestVa,
        stack_bytes: u32,
    ) -> Result<GuestVa, MemoryError> {
        let kpcr_range = self.allocate_kernel_block(GUEST_PAGE_SIZE)?;
        let kpcr = kpcr_range.start();
        let kthread = GuestVa(kpcr.0 + KTHREAD_OFFSET);
        let stack_top = stack_base.0.wrapping_add(stack_bytes);

        // KPCR / NT_TIB / KPRCB fields (Xbox layout, ADR 0010). `fs:[4]` is
        // the true StackBase: the CRT claims its TLS region at negative
        // offsets below it, so the initial ESP must sit below the reserve
        // (`tls_reserve_bytes`), which the thread-creation paths honor.
        self.memory.write_u32(kpcr, 0xFFFF_FFFF)?; // fs:[0x00] NtTib.ExceptionList
        self.memory.write_u32(GuestVa(kpcr.0 + 0x04), stack_top)?; // NtTib.StackBase
        self.memory.write_u32(GuestVa(kpcr.0 + 0x08), stack_base.0)?; // NtTib.StackLimit
        self.memory.write_u32(GuestVa(kpcr.0 + 0x18), kpcr.0)?; // NtTib.Self
        self.memory.write_u32(GuestVa(kpcr.0 + 0x1C), kpcr.0)?; // KPCR.SelfPcr
        self.memory.write_u32(GuestVa(kpcr.0 + 0x20), kpcr.0 + PRCB_OFFSET)?; // KPCR.Prcb
        self.memory.write_u32(GuestVa(kpcr.0 + PRCB_OFFSET), kthread.0)?; // Prcb.CurrentThread

        // Synthetic KTHREAD fields XAPI consumes; TlsData points at the
        // thread's TLS area below StackBase when the image has TLS.
        self.memory.write_u32(GuestVa(kthread.0 + 0x1C), stack_top)?;
        self.memory.write_u32(GuestVa(kthread.0 + 0x20), stack_base.0)?;
        let tls_data = self.initialize_stack_tls(stack_top)?;
        self.memory.write_u32(GuestVa(kthread.0 + 0x28), tls_data)?;
        Ok(kpcr)
    }

    /// Lays out the thread's TLS area at the top of its stack.
    ///
    /// Mirrors the XDK CRT contract exactly: with `size = align16(data +
    /// zero_fill) + 4` and `_tls_index = -size/4`, the accessor reads a block
    /// pointer from `[StackBase + _tls_index*4] = [StackBase - size]`, so
    /// that slot holds a self-pointer to the data at `StackBase - size + 4`,
    /// which is filled from the image's TLS template (zero-fill is already
    /// zero from the fresh stack mapping). Returns the TLS data address, or
    /// zero when the image has no TLS.
    fn initialize_stack_tls(&mut self, stack_top: u32) -> Result<u32, MemoryError> {
        let Some(template) = self.tls_template else {
            return Ok(0);
        };
        let data_bytes = template.raw_end.saturating_sub(template.raw_start).min(MAX_TLS_BYTES);
        let zero_bytes = template.zero_fill.min(MAX_TLS_BYTES);
        let size =
            (data_bytes.saturating_add(zero_bytes).saturating_add(15) & !15).saturating_add(4);
        let slot = stack_top.saturating_sub(size);
        let data = slot.saturating_add(4);
        self.memory.write_u32(GuestVa(slot), data)?;
        if data_bytes > 0 {
            let mut bytes = vec![0_u8; data_bytes as usize];
            // A short template read leaves zeros, which still functions.
            if self.memory.read(GuestVa(template.raw_start), &mut bytes).is_ok() {
                self.memory.write(GuestVa(data), &bytes)?;
            }
        }
        tracing::debug!(
            slot = format_args!("{slot:#x}"),
            data = format_args!("{data:#x}"),
            size,
            "thread TLS laid out at stack top"
        );
        Ok(data)
    }

    /// Takes the scheduling action the last kernel call recorded.
    pub(crate) fn take_pending(&mut self) -> Option<PendingAction> {
        self.pending.take()
    }

    /// True when the active thread's return to null is an intended exit
    /// rather than a fault (only the boot thread, ADR 0011).
    pub(crate) fn active_exits_on_null_return(&self) -> bool {
        self.threads.get(self.current).is_some_and(|thread| thread.exits_on_null_return)
    }

    /// Terminates the active thread and switches to the next ready one.
    ///
    /// Returns `true` when a thread was resumed (execution continues) and
    /// `false` when no runnable thread remains (the caller stops).
    pub(crate) fn exit_active(&mut self, active_cpu: &mut CpuState) -> bool {
        if let Some(thread) = self.threads.get_mut(self.current) {
            thread.state = ThreadState::Terminated;
        }
        // Joiners of this thread's handle wake (ADR 0017).
        let exited_handle = 0x0000_E000 + (self.current as u32) * 4;
        for thread in &mut self.threads {
            if thread.state == ThreadState::Waiting(exited_handle) {
                thread.state = ThreadState::Ready;
            }
        }
        // FIFO by creation order (ADR 0011).
        let next = match self.threads.iter().position(|thread| thread.state == ThreadState::Ready) {
            Some(next) => next,
            // Every remaining thread is parked on an object nothing can
            // signal now that this one is gone. The emulated devices raise
            // no interrupts and finish their work synchronously, so the
            // waits are already satisfied in fact: release them rather than
            // reporting the guest exited (the stance `WaitOutcome::TimedOut`
            // takes when a wait begins with nothing else runnable).
            None if self.release_parked_waits() => {
                match self.threads.iter().position(|thread| thread.state == ThreadState::Ready) {
                    Some(next) => next,
                    None => return false,
                }
            }
            None => return false,
        };
        self.threads[next].state = ThreadState::Running;
        // The time-stamp counter is per-core, not per-thread, so it carries
        // across the switch instead of resetting to the child's default.
        let tsc = active_cpu.tsc;
        *active_cpu = self.threads[next].cpu.clone();
        active_cpu.tsc = tsc;
        self.current = next;
        true
    }

    /// The display mode the title last programmed, if any.
    pub(crate) fn display_mode(&self) -> Option<DisplayMode> {
        self.display_mode
    }

    /// Resolves a thread handle to its index in the table.
    fn thread_index(&self, handle: u32) -> Option<usize> {
        if handle < 0x0000_E000 || !self.handles.contains(&handle) {
            return None;
        }
        let index = ((handle - 0x0000_E000) / 4) as usize;
        (index < self.threads.len()).then_some(index)
    }

    /// Makes every parked thread ready, reporting whether any was parked.
    fn release_parked_waits(&mut self) -> bool {
        let mut released = false;
        for thread in &mut self.threads {
            if let ThreadState::Waiting(handle) = thread.state {
                tracing::debug!(
                    handle = format_args!("{handle:#010x}"),
                    "releasing a parked wait: no other thread can signal it"
                );
                thread.state = ThreadState::Ready;
                released = true;
            }
        }
        released
    }

    /// Parks the active thread on a handle and resumes the next ready one.
    ///
    /// Returns `false` when no other thread is runnable (the wait service
    /// reports a timeout instead of parking in that case, so this is a
    /// should-not-happen guard).
    pub(crate) fn park_active(&mut self, handle: u32, active_cpu: &mut CpuState) -> bool {
        let Some(next) = self.threads.iter().enumerate().position(|(index, thread)| {
            index != self.current && thread.state == ThreadState::Ready
        }) else {
            return false;
        };
        if let Some(thread) = self.threads.get_mut(self.current) {
            thread.cpu = active_cpu.clone();
            thread.state = ThreadState::Waiting(handle);
        }
        self.threads[next].state = ThreadState::Running;
        let tsc = active_cpu.tsc;
        *active_cpu = self.threads[next].cpu.clone();
        active_cpu.tsc = tsc;
        self.current = next;
        true
    }

    /// Renders every thread's scheduling state for stop diagnostics.
    pub(crate) fn describe_threads(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (index, thread) in self.threads.iter().enumerate() {
            let _ = write!(
                out,
                "[{index}: {:?} eip={:#010x} kpcr={}] ",
                thread.state, thread.cpu.eip, thread.kpcr
            );
        }
        out
    }

    /// Returns the number of live (non-terminated) threads.
    #[cfg(test)]
    pub(crate) fn live_threads(&self) -> usize {
        self.threads.iter().filter(|thread| thread.state != ThreadState::Terminated).count()
    }
}

impl ThreadManager {
    /// The connected service routine for one interrupt vector.
    pub(crate) fn interrupt_routine(
        &self,
        vector: u32,
    ) -> Option<exbawks_kernel::InterruptRoutine> {
        self.interrupts
            .iter()
            .find(|(known, connected)| *connected && known.vector == vector)
            .map(|(known, _)| *known)
    }

    /// Takes the next queued deferred procedure call.
    pub(crate) fn take_dpc(&mut self) -> Option<u32> {
        if self.dpc_queue.is_empty() {
            return None;
        }
        Some(self.dpc_queue.remove(0))
    }

    /// Whether any deferred procedure is waiting to run.
    pub(crate) fn has_queued_dpc(&self) -> bool {
        !self.dpc_queue.is_empty()
    }
}

impl KernelServices for ThreadManager {
    fn create_thread(
        &mut self,
        request: ThreadCreateRequest,
    ) -> Result<ThreadCreated, KernelServiceError> {
        let stack_bytes = request.kernel_stack_size.max(MINIMUM_STACK_BYTES);
        // Leave an unmapped guard page below the stack limit.
        self.reserve_guard_page();
        let stack = self
            .allocate_kernel_block(stack_bytes)
            .map_err(|_| KernelServiceError::ResourceExhausted)?;
        // The requested TLS size derives from the same image TLS directory
        // the template models, so each thread's block comes from the
        // template inside `build_thread_pages`.
        let kpcr = self
            .build_thread_pages(stack.start(), stack.len() as u32)
            .map_err(|_| KernelServiceError::ResourceExhausted)?;
        let kthread = GuestVa(kpcr.0 + KTHREAD_OFFSET);

        // The initial frame: the start routine returns to the exit sentinel
        // and receives its two context arguments. ESP starts below the TLS
        // reserve at the stack top (the CRT claims that region). The cursor
        // bound keeps stack_top well below 2^32, so this cannot underflow.
        let stack_top = stack.start().0.wrapping_add(stack.len() as u32);
        let esp = stack_top
            .saturating_sub(self.tls_reserve_bytes())
            .saturating_sub(STACK_SCRATCH)
            .saturating_sub(12);
        let frame_ok = self.memory.write_u32(GuestVa(esp), THREAD_EXIT_SENTINEL.0).is_ok()
            && self.memory.write_u32(GuestVa(esp + 4), request.start_context1).is_ok()
            && self.memory.write_u32(GuestVa(esp + 8), request.start_context2).is_ok();
        if !frame_ok {
            return Err(KernelServiceError::ResourceExhausted);
        }

        let mut cpu = CpuState { eip: request.start_routine.0, ..CpuState::default() };
        cpu.gpr[4] = esp;
        cpu.set_segment(Segment::Fs, SegmentState { base: kpcr.0, ..SegmentState::default() });

        let index = self.threads.len();
        let id = index as u32 + 1;
        let handle = 0x0000_E000 + (index as u32) * 4;
        self.handles.insert(handle);
        self.threads.push(GuestThread {
            cpu,
            state: if request.create_suspended {
                ThreadState::Suspended
            } else {
                ThreadState::Ready
            },
            id,
            kpcr,
            exits_on_null_return: false,
        });

        Ok(ThreadCreated { handle, thread_id: id, kthread })
    }

    fn exit_current_thread(&mut self, status: u32) {
        self.pending = Some(PendingAction::Exit { status });
    }

    fn close_handle(&mut self, handle: u32) -> bool {
        // Thread, file, and event handles share one namespace but disjoint
        // ranges.
        self.handles.remove(&handle)
            || self.files.close(handle)
            || self.events.remove(&handle).is_some()
    }

    fn create_event(
        &mut self,
        manual_reset: bool,
        initially_signaled: bool,
    ) -> Result<u32, KernelServiceError> {
        let handle = EVENT_HANDLE_BASE + self.events.len() as u32 * 4;
        self.events.insert(handle, (manual_reset, initially_signaled));
        Ok(handle)
    }

    fn set_event(&mut self, handle: u32) -> Result<bool, KernelServiceError> {
        tracing::trace!(handle = format_args!("{handle:#x}"), "set_event");
        let Some((manual_reset, signaled)) = self.events.get_mut(&handle) else {
            return Err(KernelServiceError::InvalidHandle);
        };
        let previous = *signaled;
        *signaled = true;
        let manual = *manual_reset;
        // Wake the parked waiters: all of them for a manual-reset event,
        // one (consuming the signal) for an auto-reset event.
        let mut woke = false;
        for thread in &mut self.threads {
            if thread.state == ThreadState::Waiting(handle) {
                thread.state = ThreadState::Ready;
                woke = true;
                if !manual {
                    break;
                }
            }
        }
        if woke
            && !manual
            && let Some((_, signaled)) = self.events.get_mut(&handle)
        {
            *signaled = false;
        }
        Ok(previous)
    }

    fn set_interrupt_routine(&mut self, interrupt: exbawks_kernel::InterruptRoutine) {
        // A title re-initialises the same object rather than allocating a
        // new one, so the newest description of an object replaces the
        // older one instead of accumulating beside it.
        if let Some(existing) =
            self.interrupts.iter_mut().find(|(known, _)| known.object == interrupt.object)
        {
            existing.0 = interrupt;
            return;
        }
        tracing::debug!(
            object = format_args!("{:#010x}", interrupt.object),
            routine = format_args!("{:#010x}", interrupt.routine),
            vector = interrupt.vector,
            "guest registered an interrupt routine"
        );
        self.interrupts.push((interrupt, false));
    }

    fn queue_dpc(&mut self, dpc: u32) -> bool {
        if self.dpc_queue.contains(&dpc) {
            return false;
        }
        self.dpc_queue.push(dpc);
        true
    }

    fn connect_interrupt(&mut self, object: u32, connected: bool) {
        if let Some(entry) = self.interrupts.iter_mut().find(|(known, _)| known.object == object) {
            entry.1 = connected;
            tracing::debug!(
                object = format_args!("{object:#010x}"),
                vector = entry.0.vector,
                connected,
                "guest connected an interrupt"
            );
        }
    }

    fn set_display_mode(&mut self, mode: DisplayMode) {
        self.display_mode = Some(mode);
    }

    fn resume_thread(&mut self, handle: u32) -> Result<u32, KernelServiceError> {
        let index = self.thread_index(handle).ok_or(KernelServiceError::InvalidHandle)?;
        let thread = &mut self.threads[index];
        // Suspension is a single level here: threads are created suspended
        // and resumed once, which is the pattern titles use.
        if thread.state == ThreadState::Suspended {
            thread.state = ThreadState::Ready;
            return Ok(1);
        }
        Ok(0)
    }

    fn suspend_thread(&mut self, handle: u32) -> Result<u32, KernelServiceError> {
        let index = self.thread_index(handle).ok_or(KernelServiceError::InvalidHandle)?;
        if index == self.current {
            // Suspending the running thread would need a reschedule the
            // export cannot perform; no title on this path does it.
            return Err(KernelServiceError::AccessDenied);
        }
        let thread = &mut self.threads[index];
        if thread.state == ThreadState::Ready {
            thread.state = ThreadState::Suspended;
            return Ok(0);
        }
        Ok(1)
    }

    fn create_mutant(&mut self, initially_owned: bool) -> Result<u32, KernelServiceError> {
        let handle = MUTANT_HANDLE_BASE + self.mutants.len() as u32 * 4;
        self.mutants.insert(handle, initially_owned.then_some((self.current, 1)));
        self.handles.insert(handle);
        Ok(handle)
    }

    fn release_mutant(&mut self, handle: u32) -> Result<u32, KernelServiceError> {
        let current = self.current;
        let Some(owner) = self.mutants.get_mut(&handle) else {
            return Err(KernelServiceError::InvalidHandle);
        };
        let Some((holder, recursion)) = *owner else {
            return Err(KernelServiceError::AccessDenied);
        };
        if holder != current {
            return Err(KernelServiceError::AccessDenied);
        }
        *owner = (recursion > 1).then_some((holder, recursion - 1));
        if owner.is_none() {
            // The mutant is free: every thread queued on it may retry.
            for thread in &mut self.threads {
                if thread.state == ThreadState::Waiting(handle) {
                    thread.state = ThreadState::Ready;
                }
            }
        }
        Ok(recursion - 1)
    }

    fn wait_for_dispatcher_object(
        &mut self,
        address: u32,
    ) -> Result<WaitOutcome, KernelServiceError> {
        tracing::debug!(
            object = format_args!("{address:#010x}"),
            thread = self.current,
            "wait_for_dispatcher_object"
        );
        // The object's own address keys the park: guest addresses never
        // collide with the handle values this manager hands out.
        let another_runnable = self
            .threads
            .iter()
            .enumerate()
            .any(|(index, thread)| index != self.current && thread.state == ThreadState::Ready);
        if !another_runnable {
            return Ok(WaitOutcome::TimedOut);
        }
        self.pending = Some(PendingAction::Wait { handle: address });
        Ok(WaitOutcome::Pending)
    }

    fn signal_dispatcher_object(&mut self, address: u32) {
        for thread in &mut self.threads {
            if thread.state == ThreadState::Waiting(address) {
                thread.state = ThreadState::Ready;
            }
        }
    }

    fn wait_for_object(&mut self, handle: u32) -> Result<WaitOutcome, KernelServiceError> {
        tracing::debug!(
            handle = format_args!("{handle:#x}"),
            thread = self.current,
            "wait_for_object"
        );
        let another_runnable = self
            .threads
            .iter()
            .enumerate()
            .any(|(index, thread)| index != self.current && thread.state == ThreadState::Ready);
        // An event: signaled means no wait (auto-reset consumes the signal).
        if let Some((manual_reset, signaled)) = self.events.get_mut(&handle) {
            if *signaled {
                if !*manual_reset {
                    *signaled = false;
                }
                return Ok(WaitOutcome::Signaled);
            }
            if !another_runnable {
                // Nothing left can signal it. A title paces its frames on
                // events a device interrupt would set, and this model has
                // no interrupts and finishes device work synchronously, so
                // the wait is satisfied in fact; reporting a timeout only
                // makes the caller spin on it forever.
                tracing::debug!(
                    handle = format_args!("{handle:#x}"),
                    "completing an event wait no thread can signal"
                );
                return Ok(WaitOutcome::Signaled);
            }
            self.pending = Some(PendingAction::Wait { handle });
            return Ok(WaitOutcome::Pending);
        }
        // A mutant: acquiring it is the wait's whole effect. An unowned one
        // (or one this thread already holds) is taken recursively; another
        // thread's ownership parks this one until the release.
        if let Some(owner) = self.mutants.get_mut(&handle) {
            match *owner {
                None => {
                    *owner = Some((self.current, 1));
                    return Ok(WaitOutcome::Signaled);
                }
                Some((holder, recursion)) if holder == self.current => {
                    *owner = Some((holder, recursion + 1));
                    return Ok(WaitOutcome::Signaled);
                }
                Some(_) if !another_runnable => return Ok(WaitOutcome::TimedOut),
                Some(_) => {
                    self.pending = Some(PendingAction::Wait { handle });
                    return Ok(WaitOutcome::Pending);
                }
            }
        }
        // A thread handle: signaled once the thread terminates.
        if handle >= 0x0000_E000 && self.handles.contains(&handle) {
            let index = ((handle - 0x0000_E000) / 4) as usize;
            if self.threads.get(index).is_some_and(|t| t.state == ThreadState::Terminated) {
                return Ok(WaitOutcome::Signaled);
            }
            if !another_runnable {
                return Ok(WaitOutcome::TimedOut);
            }
            self.pending = Some(PendingAction::Wait { handle });
            return Ok(WaitOutcome::Pending);
        }
        Err(KernelServiceError::InvalidHandle)
    }

    fn open_file(&mut self, request: FileOpenRequest) -> Result<FileOpened, KernelServiceError> {
        self.files.open(&request)
    }

    fn read_file(
        &mut self,
        handle: u32,
        offset: Option<u64>,
        len: u32,
    ) -> Result<Vec<u8>, KernelServiceError> {
        self.files.read(handle, offset, len)
    }

    fn file_info(&mut self, handle: u32) -> Result<FileInfo, KernelServiceError> {
        self.files.info(handle)
    }

    fn write_file(
        &mut self,
        handle: u32,
        offset: Option<u64>,
        bytes: &[u8],
    ) -> Result<u32, KernelServiceError> {
        self.files.write(handle, offset, bytes)
    }

    fn create_symbolic_link(
        &mut self,
        name: String,
        target: String,
    ) -> Result<(), KernelServiceError> {
        self.files.create_link(&name, &target);
        Ok(())
    }

    fn delete_symbolic_link(&mut self, name: &str) -> bool {
        self.files.delete_link(name)
    }

    fn open_symbolic_link(&mut self, name: &str) -> Result<u32, KernelServiceError> {
        self.files.open_link_object(name)
    }

    fn query_symbolic_link(&mut self, handle: u32) -> Result<String, KernelServiceError> {
        self.files.link_target(handle)
    }

    fn persist_memory(&mut self, base: u32, size: u32) {
        // A title may persist the same page more than once; keep one entry per
        // base and grow it to the largest size seen so a later, larger persist
        // is not restored truncated.
        if let Some(entry) = self.persisted.iter_mut().find(|(existing, _)| *existing == base) {
            entry.1 = entry.1.max(size);
        } else {
            self.persisted.push((base, size));
        }
    }

    fn allocate_contiguous(&mut self, bytes: u32) -> Result<GuestVa, KernelServiceError> {
        // A fresh kernel block is contiguous in physical RAM (the bump
        // allocator hands out consecutive pages) and lives in the kernel
        // window, which is what the Mm contiguous family promises.
        let range =
            self.allocate_kernel_block(bytes).map_err(|_| KernelServiceError::ResourceExhausted)?;
        self.pool_sizes.insert(range.start().0, range.len() as u32);
        Ok(range.start())
    }

    fn set_file_position(&mut self, handle: u32, offset: u64) -> Result<(), KernelServiceError> {
        self.files.set_position(handle, offset)
    }

    fn set_file_length(&mut self, handle: u32, length: u64) -> Result<(), KernelServiceError> {
        self.files.set_length(handle, length)
    }

    fn pool_block_size(&mut self, address: u32) -> Result<u32, KernelServiceError> {
        self.pool_sizes.get(&address).copied().ok_or(KernelServiceError::NotFound)
    }

    fn claim_gpu_instance(&mut self, bytes: u32) -> Result<GuestVa, KernelServiceError> {
        let base = self.allocate_contiguous(bytes)?;
        let size = self.pool_sizes.get(&base.0).copied().unwrap_or(bytes);
        self.gpu_instance = Some((base, size));
        Ok(base)
    }

    fn allocate_virtual_memory(
        &mut self,
        request: VirtualAllocRequest,
    ) -> Result<VirtualAllocation, KernelServiceError> {
        let page = GUEST_PAGE_SIZE;
        // Round the base down to its page and the size up to cover the whole
        // requested span from that page, matching the kernel's own rounding.
        let offset = request.base % page;
        let span = request.size.saturating_add(offset);
        let rounded = span.max(1).div_ceil(page).saturating_mul(page);

        let base = if request.base == 0 {
            let start = self.user_cursor;
            let end = start
                .checked_add(rounded)
                .filter(|end| *end <= USER_ALLOC_END)
                .ok_or(KernelServiceError::ResourceExhausted)?;
            self.user_cursor = end;
            start
        } else {
            request.base - offset
        };

        // Commit maps physical pages now; a reserve-only request leaves the
        // range unbacked (the real reserve/commit region map is MEM-007), so
        // an access before a later commit faults as it should.
        if request.allocation_type & MEM_COMMIT != 0 {
            let range = GuestRange::new(GuestVa(base), u64::from(rounded))
                .map_err(|_| KernelServiceError::ResourceExhausted)?;
            let permissions = protect_to_permissions(request.protect);
            if self.memory.map_anonymous(range, permissions).is_err() {
                // A range already committed by an earlier call: re-apply the
                // protection rather than double-mapping.
                self.memory
                    .protect(range, permissions)
                    .map_err(|_| KernelServiceError::ResourceExhausted)?;
            }
        }

        Ok(VirtualAllocation { base: GuestVa(base), size: rounded })
    }
}

/// Maps Win32 `PAGE_*` protection flags to guest page permissions.
///
/// Only the low protection byte selects the access mode; the modifier bits
/// (`PAGE_GUARD`, `PAGE_NOCACHE`, `PAGE_WRITECOMBINE`) do not change guest
/// permissions here. An unrecognized value falls back to read/write.
fn protect_to_permissions(protect: u32) -> MemoryPermissions {
    match protect & 0xFF {
        0x01 => MemoryPermissions::empty(),     // PAGE_NOACCESS
        0x02 | 0x08 => MemoryPermissions::READ, // READONLY, WRITECOPY
        0x10 => MemoryPermissions::EXECUTE,     // PAGE_EXECUTE
        0x20 => MemoryPermissions::READ | MemoryPermissions::EXECUTE, // EXECUTE_READ
        0x40 | 0x80 => {
            MemoryPermissions::READ | MemoryPermissions::WRITE | MemoryPermissions::EXECUTE
        } // EXECUTE_READWRITE, EXECUTE_WRITECOPY
        _ => MemoryPermissions::READ | MemoryPermissions::WRITE, // READWRITE and the default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Win32 allocation-type and protection flags used by the tests.
    const MEM_RESERVE: u32 = 0x0000_2000;
    const PAGE_READWRITE: u32 = 0x0000_0004;

    fn manager() -> ThreadManager {
        let memory = Arc::new(SoftwareAddressSpace::new(4 * 1024 * 1024).expect("memory is valid"));
        // Kernel blocks live behind the cached physical window (ADR 0010).
        memory.map_physical_window().expect("the window maps");
        ThreadManager::new(memory, None, None)
    }

    /// Builds a request for a child thread starting at `entry`.
    fn thread_request(entry: u32) -> ThreadCreateRequest {
        ThreadCreateRequest {
            thread_extension_size: 0,
            kernel_stack_size: 0x4000,
            tls_data_size: 0,
            start_routine: GuestVa(entry),
            start_context1: 0,
            start_context2: 0,
            create_suspended: false,
        }
    }

    #[test]
    fn a_wait_parks_the_thread_and_a_signal_wakes_it() {
        let mut threads = manager();
        // The active thread is index 0; a second thread makes the wait park.
        threads.create_thread(thread_request(0x2000)).expect("worker A creates");
        threads.create_thread(thread_request(0x3000)).expect("worker B creates");
        let event = threads.create_event(false, false).expect("event creates");

        // Thread 0 waits on the unsignaled event; another thread is
        // runnable, so the wait parks.
        assert_eq!(threads.wait_for_object(event), Ok(WaitOutcome::Pending));
        let mut cpu = CpuState::default();
        assert!(threads.park_active(event, &mut cpu), "the wait parks and switches");
        assert_eq!(threads.current, 1, "a ready thread now runs");
        assert_eq!(threads.threads[0].state, ThreadState::Waiting(event));

        // Signalling the auto-reset event wakes exactly the parked waiter.
        assert_eq!(threads.set_event(event), Ok(false));
        assert_eq!(threads.threads[0].state, ThreadState::Ready);
    }

    #[test]
    fn a_wait_no_thread_can_signal_completes() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("the sole thread creates");
        let event = threads.create_event(false, false).expect("event creates");
        // No other thread is runnable, so nothing can ever signal it. The
        // wait completes rather than deadlocking or spinning the guest.
        assert_eq!(threads.wait_for_object(event), Ok(WaitOutcome::Signaled));
    }

    #[test]
    fn a_thread_join_no_thread_can_satisfy_times_out() {
        let mut threads = manager();
        let created = threads.create_thread(thread_request(0x2000)).expect("thread creates");
        // A join on a thread that will never run is a genuine timeout: no
        // device stands behind it, so completing it would be a fiction.
        assert_eq!(threads.wait_for_object(created.handle), Ok(WaitOutcome::TimedOut));
    }

    #[test]
    fn a_suspended_thread_resumes_once() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        let mut request = thread_request(0x2000);
        request.create_suspended = true;
        let created = threads.create_thread(request).expect("suspended thread creates");
        assert_eq!(threads.threads[1].state, ThreadState::Suspended);

        assert_eq!(threads.resume_thread(created.handle), Ok(1), "it was suspended once");
        assert_eq!(threads.threads[1].state, ThreadState::Ready);
        assert_eq!(threads.resume_thread(created.handle), Ok(0), "already running");
        assert_eq!(threads.resume_thread(0xDEAD), Err(KernelServiceError::InvalidHandle));
    }

    #[test]
    fn a_mutant_is_acquired_recursively_and_released_in_pairs() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        let mutant = threads.create_mutant(false).expect("mutant creates");

        // Thread 0 takes it twice; both takes are immediate.
        assert_eq!(threads.wait_for_object(mutant), Ok(WaitOutcome::Signaled));
        assert_eq!(threads.wait_for_object(mutant), Ok(WaitOutcome::Signaled));
        assert_eq!(threads.release_mutant(mutant), Ok(1), "one level remains held");

        // Thread 1 cannot take it while thread 0 still holds it.
        let mut cpu = CpuState::default();
        threads.rotate_active(&mut cpu);
        assert_eq!(threads.current, 1);
        assert_eq!(threads.wait_for_object(mutant), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(mutant, &mut cpu), "the contended wait parks");

        // Thread 0's last release frees it and wakes the queued thread.
        assert_eq!(threads.current, 0);
        assert_eq!(threads.release_mutant(mutant), Ok(0));
        assert_eq!(threads.threads[1].state, ThreadState::Ready);
        assert_eq!(threads.wait_for_object(mutant), Ok(WaitOutcome::Signaled), "free again");
    }

    #[test]
    fn releasing_a_mutant_another_thread_owns_is_denied() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        let mutant = threads.create_mutant(true).expect("mutant creates owned");
        let mut cpu = CpuState::default();
        threads.rotate_active(&mut cpu);

        assert_eq!(threads.current, 1);
        assert_eq!(threads.release_mutant(mutant), Err(KernelServiceError::AccessDenied));
        assert_eq!(threads.release_mutant(0xDEAD), Err(KernelServiceError::InvalidHandle));
    }

    #[test]
    fn the_last_ready_thread_exiting_releases_the_parked_ones() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        threads.create_thread(thread_request(0x3000)).expect("thread 2 creates");
        let event = threads.create_event(false, false).expect("event creates");
        let mut cpu = CpuState::default();

        // Threads 0 and 1 park on an event nothing will signal.
        assert_eq!(threads.wait_for_object(event), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(event, &mut cpu));
        assert_eq!(threads.wait_for_object(event), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(event, &mut cpu));
        assert_eq!(threads.current, 2, "the last ready thread runs");

        // It exits: the guest has not finished, the waits have — the parked
        // threads resume instead of the run reporting an exit.
        assert!(threads.exit_active(&mut cpu), "a released waiter resumes");
        assert_eq!(threads.threads[2].state, ThreadState::Terminated);
        assert_eq!(threads.threads[0].state, ThreadState::Running);
        assert_eq!(threads.threads[1].state, ThreadState::Ready);
    }

    #[test]
    fn the_last_thread_exiting_ends_the_run() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("the sole thread creates");
        let mut cpu = CpuState::default();
        assert!(!threads.exit_active(&mut cpu), "nothing remains to run");
    }

    #[test]
    fn rotate_round_robins_ready_threads() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        threads.create_thread(thread_request(0x3000)).expect("thread 2 creates");
        let mut cpu = CpuState { eip: 0x1000, ..CpuState::default() };

        assert_eq!(threads.current, 0);
        assert!(threads.rotate_active(&mut cpu));
        assert_eq!(threads.current, 1);
        assert!(threads.rotate_active(&mut cpu));
        assert_eq!(threads.current, 2);
        assert!(threads.rotate_active(&mut cpu), "wraps back to thread 0");
        assert_eq!(threads.current, 0);
        assert_eq!(cpu.eip, 0x1000, "thread 0's saved context returns intact");
    }

    #[test]
    fn commit_places_writable_user_memory() {
        let mut threads = manager();
        let request = VirtualAllocRequest {
            base: 0,
            size: 0x1234,
            allocation_type: MEM_COMMIT | MEM_RESERVE,
            protect: PAGE_READWRITE,
        };
        let allocation = threads.allocate_virtual_memory(request).expect("allocation succeeds");

        assert!(
            (USER_ALLOC_BASE..USER_ALLOC_END).contains(&allocation.base.0),
            "the base sits in the user range"
        );
        assert_eq!(allocation.size, 0x2000, "the size rounds up to whole pages");
        // The committed region is mapped read/write.
        threads
            .memory
            .write_u32(allocation.base, 0xDEAD_BEEF)
            .expect("committed memory is writable");
        assert_eq!(threads.memory.read_u32(allocation.base).unwrap(), 0xDEAD_BEEF);
    }

    #[test]
    fn reserve_only_leaves_the_range_unbacked() {
        let mut threads = manager();
        let request = VirtualAllocRequest {
            base: 0,
            size: 0x1000,
            allocation_type: MEM_RESERVE,
            protect: PAGE_READWRITE,
        };
        let allocation = threads.allocate_virtual_memory(request).expect("reservation succeeds");
        // A reserve-only request records the range but backs no pages, so an
        // access faults until a later commit.
        assert!(
            threads.memory.write_u32(allocation.base, 1).is_err(),
            "reserved-only memory is not accessible"
        );
    }

    #[test]
    fn kernel_chosen_bases_do_not_overlap() {
        let mut threads = manager();
        let request = VirtualAllocRequest {
            base: 0,
            size: 0x1000,
            allocation_type: MEM_COMMIT,
            protect: PAGE_READWRITE,
        };
        let first = threads.allocate_virtual_memory(request).expect("first allocation");
        let second = threads.allocate_virtual_memory(request).expect("second allocation");
        assert_eq!(second.base.0, first.base.0 + 0x1000, "the cursor advances past the first");
    }
}
