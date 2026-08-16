//! The guest thread table and kernel service implementation (ADR 0011/0012).

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

        // KPCR/TIB fields per ADR 0010.
        self.memory.write_u32(kpcr, 0xFFFF_FFFF)?; // fs:[0] SEH list head
        self.memory.write_u32(GuestVa(kpcr.0 + 0x04), stack_top)?;
        self.memory.write_u32(GuestVa(kpcr.0 + 0x08), stack_base.0)?;
        self.memory.write_u32(GuestVa(kpcr.0 + 0x1C), kpcr.0)?; // self
        self.memory.write_u32(GuestVa(kpcr.0 + 0x28), kthread.0)?; // current KTHREAD

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

        let mut cpu = CpuState::default();
        cpu.eip = request.start_routine.0;
        cpu.gpr[4] = esp;
        cpu.set_segment(Segment::Fs, SegmentState { base: kpcr.0, ..SegmentState::default() });

        let index = self.threads.len();
        let id = index as u32 + 1;
        let handle = 0x0000_E000 + (index as u32) * 4;
        self.threads.push(GuestThread {
            cpu,
            state: if request.create_suspended {
                ThreadState::Suspended
            } else {
                ThreadState::Ready
            },
            id,
            kpcr,
        });

        Ok(ThreadCreated { handle, thread_id: id, kthread })
    }

    fn exit_current_thread(&mut self, status: u32) {
        self.pending = Some(PendingAction::Exit { status });
    }
}
