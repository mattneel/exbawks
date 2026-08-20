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

/// The alignment of a fresh reservation, as the console's kernel places
/// them: blocks reserved by `NtAllocateVirtualMemory` are 64 KiB aligned
/// with sizes rounded at page granularity.
const RESERVE_ALIGN: u32 = 0x1_0000;

/// The KTHREAD block offset inside each thread's KPCR page.
const KTHREAD_OFFSET: u32 = 0x200;
/// `DISPATCHER_HEADER.SignalState`, which a finished thread sets.
const DISPATCHER_SIGNAL_STATE: u32 = 0x04;
/// `DISPATCHER_HEADER.Type` naming a thread object.
const THREAD_OBJECT_TYPE: u8 = 6;
/// `DISPATCHER_HEADER.WaitListHead`, a list head that must point at itself.
const DISPATCHER_WAIT_LIST: u32 = 0x08;
/// Where a thread's exit status sits in its control block.
///
/// The title's `GetExitCodeThread` reads exactly this: it references the
/// thread by handle, and where the signal state is set it returns
/// `[thread + 0x120]` and otherwise reports the thread still running.
const KTHREAD_EXIT_STATUS: u32 = 0x120;
/// What `GetExitCodeThread` reports for a thread that has not finished.
const STILL_ACTIVE: u32 = 0x103;
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

/// Everything one parked thread waits for (ADR 0021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitBlock {
    /// The objects, as handles or guest addresses, in the order the guest
    /// named them — a wake reports the satisfied one's index.
    pub keys: Vec<u32>,
    /// Whether every key must signal, rather than any one of them.
    pub all: bool,
    /// The virtual millisecond the wait gives up at, when it has one.
    pub deadline: Option<u64>,
    /// What EAX reads when the deadline passes: `STATUS_TIMEOUT` for a
    /// wait, success for a sleep, which finishes rather than fails.
    pub timeout_status: u32,
}

/// One guest thread's schedulable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThreadState {
    /// Eligible to run.
    Ready,
    /// The active thread.
    Running,
    /// Created suspended; a resume makes it ready.
    Suspended,
    /// Parked on a wait block until a key signals or the deadline passes
    /// (ADR 0021).
    Waiting(WaitBlock),
    /// Finished; never scheduled again.
    Terminated,
}

/// `STATUS_TIMEOUT`, the status a timed-out wait reads.
const STATUS_TIMEOUT: u32 = 0x0000_0102;
/// `DISPATCHER_HEADER.Type` for an auto-reset (synchronization) event.
const SYNCHRONIZATION_EVENT_TYPE: u32 = 1;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingAction {
    /// The calling thread exits with a status.
    Exit {
        /// The guest exit status.
        status: u32,
    },
    /// The calling thread parks on a wait block (ADR 0021).
    Wait {
        /// Everything the thread waits for.
        block: WaitBlock,
    },
}

/// The guest thread table, kernel-object allocator, and service surface.
/// One armed timer.
#[derive(Debug, Clone, Copy)]
struct ArmedTimer {
    /// The guest `KTIMER` object.
    object: u32,
    /// The deferred procedure to queue when it comes due.
    dpc: u32,
    /// The millisecond of guest time it is due at.
    due_at: u64,
    /// Its repeat interval in milliseconds, zero for a single shot.
    period: u64,
}

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
    /// Armed timers: the object, the deferred procedure to queue when it
    /// is due, the millisecond it is due at, and its repeat interval.
    timers: Vec<ArmedTimer>,
    /// Milliseconds of guest time the runtime has counted.
    elapsed_ms: u64,
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
    /// The next event handle to hand out; a cursor, never the map's size,
    /// so a closed handle is not reissued while its neighbours live.
    next_event_handle: u32,
    /// Guest mutant objects (handle → owner thread index and recursion
    /// count, `None` when unowned).
    mutants: HashMap<u32, Option<(usize, u32)>>,
    /// The next mutant handle, a cursor like the event one.
    next_mutant_handle: u32,
    /// The most recent display mode the title programmed, if any.
    display_mode: Option<DisplayMode>,
}

/// The first guest handle the event table hands out; disjoint from file
/// (`0x100`+) and thread (`0xE000`+) handles.
const EVENT_HANDLE_BASE: u32 = 0x0000_A000;
/// The first guest handle the mutant table hands out.
const MUTANT_HANDLE_BASE: u32 = 0x0000_B000;
/// The handle a thread passes to mean itself, rather than looking its own
/// up. It is never in the handle table.
const CURRENT_THREAD_PSEUDO_HANDLE: u32 = 0xFFFF_FFFE;
/// The first thread handle; each thread's is four apart.
const THREAD_HANDLE_BASE: u32 = 0x0000_E000;

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
            timers: Vec::new(),
            elapsed_ms: 0,
            persisted: Vec::new(),
            launch_data_page_cell: None,
            tick_count_cell: None,
            tls_template: None,
            pool_sizes: HashMap::new(),
            gpu_instance: None,
            events: HashMap::new(),
            next_event_handle: EVENT_HANDLE_BASE,
            mutants: HashMap::new(),
            next_mutant_handle: MUTANT_HANDLE_BASE,
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

        // A thread is a dispatcher object, and a title reads its header
        // directly rather than asking the kernel: `GetExitCodeThread` takes
        // the signal state as "has it finished", and a wait by pointer
        // reads the same two fields. The type marks it a thread so a wait
        // does not consume the signal the way an auto-reset event's is
        // consumed — a finished thread stays finished for every joiner.
        self.memory.write_u32(kthread, u32::from(THREAD_OBJECT_TYPE))?;
        self.memory.write_u32(GuestVa(kthread.0 + DISPATCHER_SIGNAL_STATE), 0)?;
        self.memory.write_u32(GuestVa(kthread.0 + KTHREAD_EXIT_STATUS), STILL_ACTIVE)?;
        // An empty list head points at itself, and a zeroed one does not.
        // A waiter links its wait block into this list and unlinks it
        // afterwards by writing through the neighbours it finds here, so
        // zeros are not an empty list — they are a null dereference in
        // whichever of the two writes goes first.
        let wait_list = kthread.0 + DISPATCHER_WAIT_LIST;
        self.memory.write_u32(GuestVa(wait_list), wait_list)?;
        self.memory.write_u32(GuestVa(wait_list + 4), wait_list)?;

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

    /// Every thread, as its identifier, what it is doing, and where.
    ///
    /// A run that wedges is a run whose threads are all waiting on each
    /// other, and the instruction pointer alone cannot show that: it names
    /// one thread of several. This says what every one of them is doing.
    pub(crate) fn thread_report(&self, active_cpu: &CpuState) -> Vec<(u32, String, u32)> {
        self.threads
            .iter()
            .enumerate()
            .map(|(index, thread)| {
                let state = match &thread.state {
                    ThreadState::Ready => "ready".to_owned(),
                    ThreadState::Running => "running".to_owned(),
                    ThreadState::Suspended => "suspended".to_owned(),
                    ThreadState::Waiting(block) => {
                        let keys: Vec<String> =
                            block.keys.iter().map(|key| format!("{key:#x}")).collect();
                        match block.deadline {
                            Some(at) => {
                                format!("waiting on [{}] until {at}ms", keys.join(", "))
                            }
                            None => format!("waiting on [{}]", keys.join(", ")),
                        }
                    }
                    ThreadState::Terminated => "terminated".to_owned(),
                };
                // The active thread's context lives in the processor, not
                // in its saved copy, which is stale while it runs.
                let eip = if index == self.current { active_cpu.eip } else { thread.cpu.eip };
                (thread.id, state, eip)
            })
            .collect()
    }

    /// Records a finished thread's result in its own control block.
    ///
    /// `GetExitCodeThread` is the reason this exists. XAPI implements it
    /// without a kernel call beyond resolving the handle: it reads the
    /// dispatcher header's signal state and, where that is set, the status
    /// beside it — so marking a thread terminated in the emulator's own
    /// table is invisible to the guest. A title polling a worker sees
    /// `STILL_ACTIVE` for as long as these two fields say nothing, which is
    /// a wait no amount of scheduling can end.
    fn publish_thread_exit(&mut self, index: usize, status: u32) {
        let Some(thread) = self.threads.get(index) else {
            return;
        };
        let kthread = GuestVa(thread.kpcr.0 + KTHREAD_OFFSET);
        // A failure here is not fatal: the thread has already stopped
        // running, and reporting it is what is lost.
        if self.memory.write_u32(GuestVa(kthread.0 + KTHREAD_EXIT_STATUS), status).is_err()
            || self.memory.write_u32(GuestVa(kthread.0 + DISPATCHER_SIGNAL_STATE), 1).is_err()
        {
            tracing::warn!(
                kthread = format_args!("{:#010x}", kthread.0),
                "could not record a thread's exit where the title reads it"
            );
        }
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
    pub(crate) fn exit_active(&mut self, active_cpu: &mut CpuState, status: u32) -> bool {
        if let Some(thread) = self.threads.get_mut(self.current) {
            thread.state = ThreadState::Terminated;
        }
        // A joiner does not have to ask the kernel whether this thread
        // finished — the title reads the answer out of the thread's own
        // control block, so a thread that dies without writing it there is
        // one the title waits on forever.
        self.publish_thread_exit(self.current, status);
        // Joiners wake, whichever way they named this thread: by its
        // handle, or by its control block's address (ADR 0021). A
        // terminated thread stays signaled, so every joiner wakes.
        let exited_handle = THREAD_HANDLE_BASE + (self.current as u32) * 4;
        self.wake_by_key(exited_handle, true);
        if let Some(thread) = self.threads.get(self.current) {
            let kthread = thread.kpcr.0 + KTHREAD_OFFSET;
            self.wake_by_key(kthread, true);
        }
        // FIFO by creation order (ADR 0011). No thread ready means the
        // rest are parked or done; the run loop idles or stops — waits are
        // never fabricated complete (ADR 0021).
        let Some(next) = self.threads.iter().position(|thread| thread.state == ThreadState::Ready)
        else {
            return false;
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

    /// Readies one thread with the given wake status in its saved EAX.
    ///
    /// A parked thread's export already returned, so the only way a wake
    /// can tell it what happened — which object won a multi-wait, or that
    /// the deadline passed — is through the register it will resume with.
    fn ready_thread_with(&mut self, index: usize, eax: u32) {
        if let Some(thread) = self.threads.get_mut(index) {
            thread.cpu.gpr[0] = eax;
            thread.state = ThreadState::Ready;
        }
    }

    /// Whether one wait key reads as signaled for one thread, without
    /// consuming anything.
    fn key_signaled(&self, key: u32, for_thread: usize) -> bool {
        if let Some((_, signaled)) = self.events.get(&key) {
            return *signaled;
        }
        if let Some(owner) = self.mutants.get(&key) {
            return match owner {
                None => true,
                Some((holder, _)) => *holder == for_thread,
            };
        }
        if let Some(index) = self.thread_index(key) {
            return self.threads.get(index).is_some_and(|t| t.state == ThreadState::Terminated);
        }
        // A guest address: the dispatcher header's signal state says.
        self.memory
            .read_u32(GuestVa(key.wrapping_add(DISPATCHER_SIGNAL_STATE)))
            .is_ok_and(|state| state != 0)
    }

    /// Consumes one wait key on behalf of one thread: an auto-reset event
    /// resets, a free mutant becomes that thread's. Everything else is
    /// untouched by being waited on.
    fn consume_key(&mut self, key: u32, for_thread: usize) {
        if let Some((manual_reset, signaled)) = self.events.get_mut(&key) {
            if !*manual_reset {
                *signaled = false;
            }
            return;
        }
        if let Some(owner) = self.mutants.get_mut(&key)
            && owner.is_none()
        {
            *owner = Some((for_thread, 1));
        }
    }

    /// Wakes threads whose wait contains `key`, reporting how many woke.
    ///
    /// This is the one wake arbiter (ADR 0021). A wait-any block is
    /// satisfied outright and told which of its keys won; a wait-all block
    /// is re-checked whole and satisfied only when every key reads
    /// signaled, consuming what it consumes. `wake_all` is the signaled
    /// object's own semantics: false for objects one signal hands to one
    /// waiter, true for those that stay signaled for everyone.
    fn wake_by_key(&mut self, key: u32, wake_all: bool) -> usize {
        let candidates: Vec<usize> = self
            .threads
            .iter()
            .enumerate()
            .filter(|(_, thread)| {
                matches!(&thread.state, ThreadState::Waiting(block) if block.keys.contains(&key))
            })
            .map(|(index, _)| index)
            .collect();
        let mut woken = 0;
        for index in candidates {
            let ThreadState::Waiting(block) = self.threads[index].state.clone() else {
                continue;
            };
            if block.all {
                if block.keys.iter().all(|k| self.key_signaled(*k, index)) {
                    for k in &block.keys {
                        self.consume_key(*k, index);
                    }
                    self.ready_thread_with(index, 0);
                    woken += 1;
                }
            } else {
                let position = block.keys.iter().position(|k| *k == key).unwrap_or(0) as u32;
                // What the wake hands over is consumed by the thread it
                // wakes: a mutant's woken waiter owns it from this moment.
                self.consume_key(key, index);
                // STATUS_WAIT_0 + index names the object that satisfied it.
                self.ready_thread_with(index, position);
                woken += 1;
            }
            if !wake_all && woken > 0 {
                return woken;
            }
        }
        woken
    }

    /// Records the wait the run loop parks the caller on (ADR 0021).
    fn park_pending(
        &mut self,
        keys: Vec<u32>,
        all: bool,
        timeout_ms: Option<u64>,
        timeout_status: u32,
    ) {
        let deadline = timeout_ms.map(|ms| self.elapsed_ms.saturating_add(ms.max(1)));
        self.pending =
            Some(PendingAction::Wait { block: WaitBlock { keys, all, deadline, timeout_status } });
    }

    /// Parks the active thread on a wait block and resumes the next ready
    /// one.
    ///
    /// Returns `false` when no other thread is runnable. The caller stays
    /// parked either way — the run loop then idles, advancing the clocks
    /// and delivering interrupts until a wake makes some thread ready
    /// (ADR 0021), instead of the wait being fabricated complete.
    pub(crate) fn park_active(&mut self, block: WaitBlock, active_cpu: &mut CpuState) -> bool {
        if let Some(thread) = self.threads.get_mut(self.current) {
            thread.cpu = active_cpu.clone();
            thread.state = ThreadState::Waiting(block);
        }
        let Some(next) = self.threads.iter().enumerate().position(|(index, thread)| {
            index != self.current && thread.state == ThreadState::Ready
        }) else {
            return false;
        };
        self.threads[next].state = ThreadState::Running;
        let tsc = active_cpu.tsc;
        *active_cpu = self.threads[next].cpu.clone();
        active_cpu.tsc = tsc;
        self.current = next;
        true
    }

    /// Whether the active thread may execute guest code right now.
    pub(crate) fn active_runnable(&self) -> bool {
        self.threads.get(self.current).is_some_and(|t| t.state == ThreadState::Running)
    }

    /// Switches to any ready thread when the active one cannot run,
    /// reporting whether guest execution may continue.
    ///
    /// The parked thread's context was saved when it parked, so nothing is
    /// saved here — the live CPU state is stale by design.
    pub(crate) fn resume_any_ready(&mut self, active_cpu: &mut CpuState) -> bool {
        if self.active_runnable() {
            return true;
        }
        let Some(next) = self.threads.iter().position(|thread| thread.state == ThreadState::Ready)
        else {
            return false;
        };
        self.threads[next].state = ThreadState::Running;
        let tsc = active_cpu.tsc;
        *active_cpu = self.threads[next].cpu.clone();
        active_cpu.tsc = tsc;
        self.current = next;
        true
    }

    /// What could still wake a parked thread: the earliest virtual-time
    /// wake (timer or wait deadline), whether any interrupt routine is
    /// connected, and whether any thread is parked at all.
    pub(crate) fn wake_hint(&self) -> (Option<u64>, bool, bool) {
        let mut next: Option<u64> = None;
        let mut consider = |at: u64| {
            next = Some(next.map_or(at, |known: u64| known.min(at)));
        };
        for timer in &self.timers {
            consider(timer.due_at);
        }
        let mut any_waiting = false;
        for thread in &self.threads {
            if let ThreadState::Waiting(block) = &thread.state {
                any_waiting = true;
                if let Some(deadline) = block.deadline {
                    consider(deadline);
                }
            }
        }
        let interrupts = self.interrupts.iter().any(|(_, connected)| *connected);
        (next, interrupts, any_waiting)
    }

    /// The current virtual time in milliseconds.
    pub(crate) fn now_ms(&self) -> u64 {
        self.elapsed_ms
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

    /// Advances guest time and queues the deferred procedures of any
    /// timers that have come due.
    pub(crate) fn advance_timers(&mut self, milliseconds: u64) {
        self.elapsed_ms = self.elapsed_ms.saturating_add(milliseconds);
        if milliseconds == 0 {
            return;
        }
        // Waits whose deadline has passed wake with their timeout status —
        // `STATUS_TIMEOUT` for a wait, success for a sleep (ADR 0021).
        let now = self.elapsed_ms;
        let expired: Vec<(usize, u32)> = self
            .threads
            .iter()
            .enumerate()
            .filter_map(|(index, thread)| match &thread.state {
                ThreadState::Waiting(block) if block.deadline.is_some_and(|at| at <= now) => {
                    Some((index, block.timeout_status))
                }
                _ => None,
            })
            .collect();
        for (index, status) in expired {
            self.ready_thread_with(index, status);
        }
        if self.timers.is_empty() {
            return;
        }
        let now = self.elapsed_ms;
        let mut due = Vec::new();
        self.timers.retain_mut(|timer| {
            if timer.due_at > now {
                return true;
            }
            if timer.dpc != 0 {
                due.push(timer.dpc);
            }
            // A repeating timer re-arms itself; a one-shot is done.
            if timer.period > 0 {
                timer.due_at = now.saturating_add(timer.period);
                return true;
            }
            false
        });
        for dpc in due {
            self.queue_dpc(dpc);
        }
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
        let handle = THREAD_HANDLE_BASE + (index as u32) * 4;
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
        // A cursor, never the map's size: a closed handle's value must not
        // be reissued while its neighbours live (ADR 0021).
        let handle = self.next_event_handle;
        self.next_event_handle = self.next_event_handle.wrapping_add(4);
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
        // The arbiter wakes one waiter for an auto-reset event, all for a
        // notification event; an auto-reset signal a waiter took is
        // consumed by that wake (ADR 0021).
        let woke = self.wake_by_key(handle, manual);
        if woke > 0
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

    fn set_timer(&mut self, timer: u32, due: i64, period: u32, dpc: u32) -> bool {
        // A negative due time is an interval from now in hundred-nanosecond
        // units; a positive one is an absolute time this runtime has no
        // clock to compare against, so it is treated as immediate.
        let delay_ms =
            if due < 0 { (due.unsigned_abs() / 10_000).min(u64::from(u32::MAX)) } else { 0 };
        let due_at = self.elapsed_ms.saturating_add(delay_ms);
        let armed = ArmedTimer { object: timer, dpc, due_at, period: u64::from(period) };
        if let Some(existing) = self.timers.iter_mut().find(|known| known.object == timer) {
            *existing = armed;
            return true;
        }
        tracing::debug!(
            timer = format_args!("{timer:#010x}"),
            dpc = format_args!("{dpc:#010x}"),
            delay_ms,
            "guest armed a timer"
        );
        self.timers.push(armed);
        false
    }

    fn cancel_timer(&mut self, timer: u32) -> bool {
        let before = self.timers.len();
        self.timers.retain(|known| known.object != timer);
        before != self.timers.len()
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
        let handle = self.next_mutant_handle;
        self.next_mutant_handle = self.next_mutant_handle.wrapping_add(4);
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
            // Exactly one waiter wakes, owning the mutant from the moment
            // it wakes — releasing to everyone released it to no one, and
            // two "owners" is what a mutant exists to prevent (ADR 0021).
            self.wake_by_key(handle, false);
        }
        Ok(recursion - 1)
    }

    fn wait_for_dispatcher_object(
        &mut self,
        address: u32,
        timeout_ms: Option<u64>,
    ) -> Result<WaitOutcome, KernelServiceError> {
        tracing::debug!(
            object = format_args!("{address:#010x}"),
            thread = self.current,
            "wait_for_dispatcher_object"
        );
        if timeout_ms == Some(0) {
            return Ok(WaitOutcome::TimedOut);
        }
        // The object's own address keys the park: guest addresses never
        // collide with the handle values this manager hands out, because
        // every handle range sits below the image base at 0x10000 and no
        // guest object can live in unmapped memory below it.
        self.park_pending(vec![address], false, timeout_ms, STATUS_TIMEOUT);
        Ok(WaitOutcome::Pending)
    }

    fn signal_dispatcher_object(&mut self, address: u32) {
        // The header's type byte decides the wake's reach: an auto-reset
        // (synchronization) event hands its signal to exactly one waiter
        // and is consumed by that wake; everything else stays signaled for
        // every waiter (ADR 0021).
        let kind = self.memory.read_u32(GuestVa(address)).map(|word| word & 0xFF).unwrap_or(0);
        let auto = kind == SYNCHRONIZATION_EVENT_TYPE;
        let woke = self.wake_by_key(address, !auto);
        if auto && woke > 0 {
            let _ =
                self.memory.write_u32(GuestVa(address.wrapping_add(DISPATCHER_SIGNAL_STATE)), 0);
        }
    }

    fn transfer_section_wake(&mut self, address: u32) -> Option<u32> {
        let index = self.threads.iter().position(|thread| {
            matches!(&thread.state, ThreadState::Waiting(block) if block.keys.contains(&address))
        })?;
        self.ready_thread_with(index, 0);
        Some(self.threads[index].kpcr.0 + KTHREAD_OFFSET)
    }

    fn object_for_handle(&self, handle: u32) -> Option<u32> {
        // A thread handle names its own control block, which is the object
        // a caller expects to be handed and to read fields from. Handles
        // this runtime keeps no body for report nothing, so a caller is
        // told the handle is not one it can reference.
        //
        // The pseudo-handle for "the thread asking" never appears in the
        // table: it is a constant a caller passes instead of its own
        // handle, and it names whichever thread is running.
        let thread = if handle == CURRENT_THREAD_PSEUDO_HANDLE {
            self.threads.get(self.current)?
        } else {
            let index = handle.checked_sub(THREAD_HANDLE_BASE)? / 4;
            self.threads.get(index as usize)?
        };
        Some(thread.kpcr.0 + KTHREAD_OFFSET)
    }

    fn yield_thread(&mut self) -> bool {
        // Whether any thread other than the caller is ready to be given
        // the turn. The rotation itself happens on the run loop's own
        // slice boundary, where the processor state is the loop's to move.
        let count = self.threads.len();
        (1..count)
            .map(|offset| (self.current + offset) % count)
            .any(|index| self.threads[index].state == ThreadState::Ready)
    }

    fn wait_for_object(
        &mut self,
        handle: u32,
        timeout_ms: Option<u64>,
    ) -> Result<WaitOutcome, KernelServiceError> {
        tracing::debug!(
            handle = format_args!("{handle:#x}"),
            thread = self.current,
            "wait_for_object"
        );
        // An event: signaled means no wait (auto-reset consumes the
        // signal). A mutant: acquiring it is the wait's whole effect, and
        // one this thread already holds is taken recursively. A thread:
        // signaled once it terminates.
        if let Some(owner) = self.mutants.get_mut(&handle)
            && let Some((holder, recursion)) = *owner
            && holder == self.current
        {
            *owner = Some((holder, recursion + 1));
            return Ok(WaitOutcome::Signaled);
        }
        let known = self.events.contains_key(&handle)
            || self.mutants.contains_key(&handle)
            || self.thread_index(handle).is_some();
        if !known {
            return Err(KernelServiceError::InvalidHandle);
        }
        if self.key_signaled(handle, self.current) {
            self.consume_key(handle, self.current);
            return Ok(WaitOutcome::Signaled);
        }
        // Unsatisfied. A poll reports the truth; anything else parks, and
        // what wakes it is a signal, a deadline, or nothing — in which
        // case the run loop reports the deadlock instead of this code
        // fabricating a completion (ADR 0021).
        if timeout_ms == Some(0) {
            return Ok(WaitOutcome::TimedOut);
        }
        self.park_pending(vec![handle], false, timeout_ms, STATUS_TIMEOUT);
        Ok(WaitOutcome::Pending)
    }

    fn wait_for_objects(
        &mut self,
        keys: &[u32],
        wait_all: bool,
        timeout_ms: Option<u64>,
    ) -> Result<exbawks_kernel::MultiWaitOutcome, KernelServiceError> {
        use exbawks_kernel::MultiWaitOutcome;
        for key in keys {
            let known = self.events.contains_key(key)
                || self.mutants.contains_key(key)
                || self.thread_index(*key).is_some();
            if !known {
                return Err(KernelServiceError::InvalidHandle);
            }
        }
        if wait_all {
            if keys.iter().all(|key| self.key_signaled(*key, self.current)) {
                for key in keys {
                    self.consume_key(*key, self.current);
                }
                return Ok(MultiWaitOutcome::Satisfied(0));
            }
        } else if let Some(position) =
            keys.iter().position(|key| self.key_signaled(*key, self.current))
        {
            self.consume_key(keys[position], self.current);
            return Ok(MultiWaitOutcome::Satisfied(position as u32));
        }
        if timeout_ms == Some(0) {
            return Ok(MultiWaitOutcome::TimedOut);
        }
        self.park_pending(keys.to_vec(), wait_all, timeout_ms, STATUS_TIMEOUT);
        Ok(MultiWaitOutcome::Pending)
    }

    fn sleep_thread(&mut self, timeout_ms: u64) -> Result<WaitOutcome, KernelServiceError> {
        if timeout_ms == 0 {
            return Ok(WaitOutcome::Signaled);
        }
        // No keys: only the deadline wakes it, and finishing a delay is
        // success, not a timeout (ADR 0021).
        self.park_pending(Vec::new(), false, Some(timeout_ms), 0);
        Ok(WaitOutcome::Pending)
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

    fn free_contiguous(&mut self, address: u32) -> Result<(), KernelServiceError> {
        // The recorded size makes the free exact; an address this runtime
        // never handed out is refused rather than guessed at. Persisted
        // regions (ADR 0015) stay out of the pool: a title frees its
        // launch page only by rebooting.
        let size = self.pool_sizes.remove(&address).ok_or(KernelServiceError::NotFound)?;
        let physical = address & 0x1FFF_FFFF;
        if self
            .persisted
            .iter()
            .any(|(start, len)| *start <= address && address < start.saturating_add(*len))
        {
            return Ok(());
        }
        self.memory.free_physical(exbawks_types::GuestPa(physical), size);
        tracing::trace!(
            address = format_args!("{address:#010x}"),
            size = format_args!("{size:#x}"),
            "contiguous free returned pages to the pool"
        );
        Ok(())
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
            // A fresh reservation is 64 KiB aligned, exactly as the
            // console's kernel places one; only the size rounds at page
            // granularity. An allocator finds its arena header by masking
            // a block pointer down to that alignment, so a reservation
            // this runtime packs at page granularity sends every such
            // lookup into the wrong arena — whose memory reads as zeros.
            let start = self.user_cursor.next_multiple_of(RESERVE_ALIGN);
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
            let permissions = protect_to_permissions(request.protect);
            // Page by page, because a commit may overlap pages an earlier
            // commit already backed: the kernel commits the fresh ones and
            // leaves the rest committed, and a title extending its heap
            // relies on that. Refusing the mixed case fails the extend,
            // and an allocator's failure path is where its bookkeeping
            // and its lists part company.
            let mut cursor = base;
            let end = base.checked_add(rounded).ok_or(KernelServiceError::ResourceExhausted)?;
            while cursor < end {
                let range = GuestRange::new(GuestVa(cursor), u64::from(page))
                    .map_err(|_| KernelServiceError::ResourceExhausted)?;
                if self.memory.map_anonymous(range, permissions).is_err() {
                    // Already committed: re-apply the protection instead.
                    self.memory.protect(range, permissions).map_err(|_| {
                        tracing::warn!(
                            page = format_args!("{cursor:#010x}"),
                            "a commit page could neither map nor re-protect"
                        );
                        KernelServiceError::ResourceExhausted
                    })?;
                }
                cursor = cursor.saturating_add(page);
            }
        }

        tracing::debug!(
            requested_base = format_args!("{:#010x}", request.base),
            requested_size = format_args!("{:#x}", request.size),
            allocation_type = format_args!("{:#x}", request.allocation_type),
            base = format_args!("{base:#010x}"),
            size = format_args!("{rounded:#x}"),
            "NtAllocateVirtualMemory"
        );
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

    /// A single-key infinite wait block, as the wait services build.
    fn block(key: u32) -> WaitBlock {
        WaitBlock { keys: vec![key], all: false, deadline: None, timeout_status: STATUS_TIMEOUT }
    }

    /// Whether a thread waits on exactly this key.
    fn waits_on(state: &ThreadState, key: u32) -> bool {
        matches!(state, ThreadState::Waiting(block) if block.keys == vec![key])
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
        assert_eq!(threads.wait_for_object(event, None), Ok(WaitOutcome::Pending));
        let mut cpu = CpuState::default();
        assert!(threads.park_active(block(event), &mut cpu), "the wait parks and switches");
        assert_eq!(threads.current, 1, "a ready thread now runs");
        assert!(waits_on(&threads.threads[0].state, event));

        // Signalling the auto-reset event wakes exactly the parked waiter.
        assert_eq!(threads.set_event(event), Ok(false));
        assert_eq!(threads.threads[0].state, ThreadState::Ready);
    }

    #[test]
    fn a_wait_parks_even_when_nothing_else_is_runnable() {
        // The old model fabricated a completion here; ADR 0021 parks and
        // leaves the outcome to the run loop's idle machinery, which
        // reports a genuine deadlock rather than papering over one.
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("the sole thread creates");
        let event = threads.create_event(false, false).expect("event creates");
        assert_eq!(threads.wait_for_object(event, None), Ok(WaitOutcome::Pending));
    }

    #[test]
    fn a_zero_timeout_poll_reports_a_timeout_instead_of_parking() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("the sole thread creates");
        let event = threads.create_event(false, false).expect("event creates");
        // NT's standard non-blocking poll: WaitForSingleObject(h, 0).
        assert_eq!(threads.wait_for_object(event, Some(0)), Ok(WaitOutcome::TimedOut));
        assert!(threads.pending.is_none(), "a poll never parks");
    }

    #[test]
    fn a_wait_deadline_wakes_the_thread_with_a_timeout() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x3000)).expect("thread 1 creates");
        let event = threads.create_event(false, false).expect("event creates");

        assert_eq!(threads.wait_for_object(event, Some(50)), Ok(WaitOutcome::Pending));
        let mut cpu = CpuState::default();
        let Some(PendingAction::Wait { block }) = threads.take_pending() else {
            panic!("a wait was recorded");
        };
        assert!(threads.park_active(block, &mut cpu), "the wait parks");
        assert!(matches!(threads.threads[0].state, ThreadState::Waiting(_)));

        // Short of the deadline nothing wakes; past it the thread readies
        // with STATUS_TIMEOUT in its saved EAX.
        threads.advance_timers(49);
        assert!(matches!(threads.threads[0].state, ThreadState::Waiting(_)));
        threads.advance_timers(2);
        assert_eq!(threads.threads[0].state, ThreadState::Ready);
        assert_eq!(threads.threads[0].cpu.gpr[0], STATUS_TIMEOUT);
    }

    #[test]
    fn a_multi_wait_wake_names_the_winning_index() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x3000)).expect("thread 1 creates");
        let first = threads.create_event(false, false).expect("event A creates");
        let second = threads.create_event(false, false).expect("event B creates");

        use exbawks_kernel::MultiWaitOutcome;
        assert_eq!(
            threads.wait_for_objects(&[first, second], false, None),
            Ok(MultiWaitOutcome::Pending)
        );
        let mut cpu = CpuState::default();
        let Some(PendingAction::Wait { block }) = threads.take_pending() else {
            panic!("a wait was recorded");
        };
        assert_eq!(block.keys, vec![first, second], "the whole set is one block");
        assert!(threads.park_active(block, &mut cpu));

        // The SECOND object signals: the woken thread's EAX must read
        // STATUS_WAIT_0 + 1, not the success the export saved.
        assert_eq!(threads.set_event(second), Ok(false));
        assert_eq!(threads.threads[0].state, ThreadState::Ready);
        assert_eq!(threads.threads[0].cpu.gpr[0], 1, "the winner's index");
    }

    #[test]
    fn closed_event_handles_are_not_reissued() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("thread creates");
        let first = threads.create_event(false, false).expect("event A creates");
        let second = threads.create_event(true, true).expect("event B creates");
        assert!(threads.close_handle(first), "A closes");
        let third = threads.create_event(false, false).expect("event C creates");
        assert_ne!(third, second, "a live handle is never reissued");
        assert_ne!(third, first, "nor is a closed one reused");
        // B's state survived C's creation.
        assert_eq!(threads.wait_for_object(second, None), Ok(WaitOutcome::Signaled));
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
        assert_eq!(threads.wait_for_object(mutant, None), Ok(WaitOutcome::Signaled));
        assert_eq!(threads.wait_for_object(mutant, None), Ok(WaitOutcome::Signaled));
        assert_eq!(threads.release_mutant(mutant), Ok(1), "one level remains held");

        // Thread 1 cannot take it while thread 0 still holds it.
        let mut cpu = CpuState::default();
        threads.rotate_active(&mut cpu);
        assert_eq!(threads.current, 1);
        assert_eq!(threads.wait_for_object(mutant, None), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(block(mutant), &mut cpu), "the contended wait parks");

        // Thread 0's last release wakes the queued thread as the OWNER:
        // releasing to everyone released it to no one (ADR 0021).
        assert_eq!(threads.current, 0);
        assert_eq!(threads.release_mutant(mutant), Ok(0));
        assert_eq!(threads.threads[1].state, ThreadState::Ready);
        assert_eq!(
            threads.mutants.get(&mutant),
            Some(&Some((1, 1))),
            "ownership transferred with the wake"
        );
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
    fn the_last_ready_thread_exiting_leaves_the_parked_ones_parked() {
        // The old model woke every parked thread here, whatever it waited
        // on. ADR 0021 keeps them parked: whether anything can still wake
        // them is the run loop's question, and a wait on an event nothing
        // will signal is a deadlock to report, not to paper over.
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        threads.create_thread(thread_request(0x3000)).expect("thread 2 creates");
        let event = threads.create_event(false, false).expect("event creates");
        let mut cpu = CpuState::default();

        assert_eq!(threads.wait_for_object(event, None), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(block(event), &mut cpu));
        assert_eq!(threads.wait_for_object(event, None), Ok(WaitOutcome::Pending));
        assert!(threads.park_active(block(event), &mut cpu));
        assert_eq!(threads.current, 2, "the last ready thread runs");

        assert!(!threads.exit_active(&mut cpu, 0), "nothing is runnable");
        assert_eq!(threads.threads[2].state, ThreadState::Terminated);
        assert!(matches!(threads.threads[0].state, ThreadState::Waiting(_)));
        assert!(matches!(threads.threads[1].state, ThreadState::Waiting(_)));
        let (next_due, _interrupts, any_waiting) = threads.wake_hint();
        assert!(any_waiting, "the run loop sees the parked threads");
        assert_eq!(next_due, None, "and that nothing is due to wake them");
    }

    #[test]
    fn a_thread_exit_wakes_a_joiner_waiting_by_pointer() {
        // A title may join by the KTHREAD address instead of the handle;
        // the exit must wake both key forms (ADR 0021).
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        let created = threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        let mut cpu = CpuState::default();

        let kthread = created.kthread.0;
        assert_eq!(threads.wait_for_dispatcher_object(kthread, None), Ok(WaitOutcome::Pending));
        let Some(PendingAction::Wait { block }) = threads.take_pending() else {
            panic!("a wait was recorded");
        };
        assert!(threads.park_active(block, &mut cpu), "the join parks");
        assert_eq!(threads.current, 1, "the joined thread runs");

        assert!(!threads.exit_active(&mut cpu, 0) || threads.current == 0);
        assert_eq!(threads.threads[0].state, ThreadState::Running, "the joiner resumed");
    }

    #[test]
    fn a_finished_thread_says_so_where_the_title_reads_it() {
        // `GetExitCodeThread` asks the kernel only to resolve the handle;
        // it then reads the thread's own control block — the dispatcher
        // header's signal state, and the status beside it. A thread that
        // stops running without writing those is one a title polls
        // forever, so this pins both fields and the order they are
        // written in: a reader that sees the signal must already find the
        // status there.
        let mut threads = manager();
        threads.create_thread(thread_request(0x1000)).expect("thread 0 creates");
        threads.create_thread(thread_request(0x2000)).expect("thread 1 creates");
        let mut cpu = CpuState::default();
        threads.rotate_active(&mut cpu);
        assert_eq!(threads.current, 1);

        let kthread = threads.threads[1].kpcr.0 + KTHREAD_OFFSET;
        let read = |threads: &ThreadManager, offset: u32| {
            threads.memory.read_u32(GuestVa(kthread + offset)).expect("the block is mapped")
        };
        assert_eq!(read(&threads, DISPATCHER_SIGNAL_STATE), 0, "it has not finished");
        assert_eq!(read(&threads, KTHREAD_EXIT_STATUS), STILL_ACTIVE, "and says so");
        assert_eq!(read(&threads, 0) & 0xFF, u32::from(THREAD_OBJECT_TYPE), "a thread object");

        assert!(threads.exit_active(&mut cpu, 0x2A), "the other thread runs on");
        assert_eq!(read(&threads, DISPATCHER_SIGNAL_STATE), 1, "now it is signaled");
        assert_eq!(read(&threads, KTHREAD_EXIT_STATUS), 0x2A, "carrying its status");
    }

    #[test]
    fn the_last_thread_exiting_ends_the_run() {
        let mut threads = manager();
        threads.create_thread(thread_request(0x2000)).expect("the sole thread creates");
        let mut cpu = CpuState::default();
        assert!(!threads.exit_active(&mut cpu, 0), "nothing remains to run");
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
        assert!(second.base.0 > first.base.0, "the cursor advances past the first");
        assert_eq!(first.base.0 % 0x1_0000, 0, "a fresh reservation is 64 KiB aligned");
        assert_eq!(second.base.0 % 0x1_0000, 0, "every fresh reservation is");
    }
}
