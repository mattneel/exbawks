//! The guest thread table and kernel service implementation (ADR 0011/0012).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use exbawks_cpu::{CpuState, Segment, SegmentState};
use exbawks_kernel::{KernelServiceError, KernelServices, ThreadCreateRequest, ThreadCreated};
use exbawks_memory::{GuestMemory, MemoryError, SoftwareAddressSpace};
use exbawks_types::{GUEST_PAGE_SIZE, GuestRange, GuestVa, MemoryPermissions};

/// The reserved return address for guest thread start routines.
///
/// It sits in the reserved region above the kernel gate range, so
/// `gate_ordinal` never resolves it to a real export; the run loop treats
/// execution arriving here as an implicit thread exit whose status is EAX
/// (ADR 0011).
pub(crate) const THREAD_EXIT_SENTINEL: GuestVa = GuestVa(0xFFBF_FFF4);

/// The first virtual address of the kernel-owned object region (ADR 0010).
///
/// Until the real allocators land (MEM-006/007), kernel blocks are
/// bump-mapped here; the guest-visible property that matters is that every
/// kernel pointer compares above `0x8000_0000`.
const KERNEL_REGION_BASE: u32 = 0x8001_0000;

/// The KTHREAD block offset inside each thread's KPCR page.
const KTHREAD_OFFSET: u32 = 0x200;
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
    #[allow(dead_code)]
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
}

/// The guest thread table, kernel-object allocator, and service surface.
pub(crate) struct ThreadManager {
    memory: Arc<SoftwareAddressSpace>,
    threads: Vec<GuestThread>,
    current: usize,
    pending: Option<PendingAction>,
    kernel_cursor: u32,
    kernel_region_end: u32,
    /// Open guest handles (minimal object table; the full object manager is
    /// HLE-005). Thread creation registers its handle here.
    handles: HashSet<u32>,
}

impl ThreadManager {
    pub(crate) fn new(memory: Arc<SoftwareAddressSpace>) -> Self {
        // Kernel blocks stay inside the cached physical window
        // (`0x8000_0000 | PA`, ADR 0010) so they never stray into the
        // higher windows the ADR reserves.
        let window = u32::try_from(memory.physical_len()).unwrap_or(u32::MAX);
        let kernel_region_end = 0x8000_0000_u32.saturating_add(window);
        Self {
            memory,
            threads: Vec::new(),
            current: 0,
            pending: None,
            kernel_cursor: KERNEL_REGION_BASE,
            kernel_region_end,
            handles: HashSet::new(),
        }
    }

    /// Builds the boot thread's KPCR/KTHREAD pages and registers thread one.
    ///
    /// Returns the KPCR address the caller wires into the active `fs` base.
    pub(crate) fn create_boot_environment(
        &mut self,
        stack_base: GuestVa,
        stack_bytes: u32,
    ) -> Result<GuestVa, MemoryError> {
        let kpcr = self.build_thread_pages(stack_base, stack_bytes, GuestVa(0))?;
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
    fn allocate_kernel_block(&mut self, bytes: u32) -> Result<GuestRange, MemoryError> {
        let page = GUEST_PAGE_SIZE;
        let rounded = bytes.max(1).div_ceil(page).saturating_mul(page);
        let Some(end) =
            self.kernel_cursor.checked_add(rounded).filter(|end| *end <= self.kernel_region_end)
        else {
            return Err(MemoryError::OutOfPhysicalMemory { requested_pages: rounded / page });
        };
        let range = GuestRange::new(GuestVa(self.kernel_cursor), u64::from(rounded))?;
        self.memory.map_anonymous(range, MemoryPermissions::READ | MemoryPermissions::WRITE)?;
        self.kernel_cursor = end;
        Ok(range)
    }

    /// Advances the cursor past one unmapped guard page.
    ///
    /// A guard below a stack limit turns an overflow into a fault instead of
    /// silent corruption of the neighboring kernel block (ADR 0010).
    fn reserve_guard_page(&mut self) {
        self.kernel_cursor = self.kernel_cursor.saturating_add(GUEST_PAGE_SIZE);
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
        tls_data: GuestVa,
    ) -> Result<GuestVa, MemoryError> {
        let kpcr_range = self.allocate_kernel_block(GUEST_PAGE_SIZE)?;
        let kpcr = kpcr_range.start();
        let kthread = GuestVa(kpcr.0 + KTHREAD_OFFSET);
        let stack_top = stack_base.0.wrapping_add(stack_bytes);

        // KPCR / NT_TIB / KPRCB fields (Xbox layout, ADR 0010).
        self.memory.write_u32(kpcr, 0xFFFF_FFFF)?; // fs:[0x00] NtTib.ExceptionList
        self.memory.write_u32(GuestVa(kpcr.0 + 0x04), stack_top)?; // NtTib.StackBase
        self.memory.write_u32(GuestVa(kpcr.0 + 0x08), stack_base.0)?; // NtTib.StackLimit
        self.memory.write_u32(GuestVa(kpcr.0 + 0x18), kpcr.0)?; // NtTib.Self
        self.memory.write_u32(GuestVa(kpcr.0 + 0x1C), kpcr.0)?; // KPCR.SelfPcr
        self.memory.write_u32(GuestVa(kpcr.0 + 0x20), kpcr.0 + PRCB_OFFSET)?; // KPCR.Prcb
        self.memory.write_u32(GuestVa(kpcr.0 + PRCB_OFFSET), kthread.0)?; // Prcb.CurrentThread

        // Synthetic KTHREAD fields XAPI consumes.
        self.memory.write_u32(GuestVa(kthread.0 + 0x1C), stack_top)?;
        self.memory.write_u32(GuestVa(kthread.0 + 0x20), stack_base.0)?;
        self.memory.write_u32(GuestVa(kthread.0 + 0x28), tls_data.0)?;
        Ok(kpcr)
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
        // FIFO by creation order (ADR 0011).
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

    /// Returns the number of live (non-terminated) threads.
    #[cfg(test)]
    pub(crate) fn live_threads(&self) -> usize {
        self.threads.iter().filter(|thread| thread.state != ThreadState::Terminated).count()
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
        let tls = if request.tls_data_size == 0 {
            GuestVa(0)
        } else {
            self.allocate_kernel_block(request.tls_data_size)
                .map_err(|_| KernelServiceError::ResourceExhausted)?
                .start()
        };
        let kpcr = self
            .build_thread_pages(stack.start(), stack.len() as u32, tls)
            .map_err(|_| KernelServiceError::ResourceExhausted)?;
        let kthread = GuestVa(kpcr.0 + KTHREAD_OFFSET);

        // The initial frame: the start routine returns to the exit sentinel
        // and receives its two context arguments. The cursor bound keeps
        // stack_top well below 2^32, so this cannot underflow.
        let stack_top = stack.start().0.wrapping_add(stack.len() as u32);
        let esp = stack_top.saturating_sub(STACK_SCRATCH).saturating_sub(12);
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
        self.handles.remove(&handle)
    }
}
