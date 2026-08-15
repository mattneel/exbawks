//! Generated equivalence tests for the software and Windows backends.
//!
//! Every scenario runs the same operation list against both backends and
//! requires identical typed failures and identical readable bytes.

#![cfg(windows)]

use exbawks_memory::{GuestMemory, MemoryError, SoftwareAddressSpace, WindowsAddressSpace};
use exbawks_types::{GUEST_PAGE_SIZE, GuestPa, GuestRange, GuestVa, MemoryPermissions};

const PAGE: u64 = GUEST_PAGE_SIZE as u64;
// 64 physical pages, so generated scenarios reach allocator exhaustion.
const PHYSICAL_BYTES: usize = 256 * 1024;
const WINDOW_PAGES: u32 = 64;

/// One backend-neutral mapping interface for generated scenarios.
trait Backend: GuestMemory {
    fn map_anonymous(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError>;

    fn map_alias(
        &self,
        range: GuestRange,
        physical_start: GuestPa,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError>;

    fn load_region(
        &self,
        address: GuestVa,
        virtual_size: u32,
        initial: &[u8],
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError>;

    fn protect(&self, range: GuestRange, permissions: MemoryPermissions)
    -> Result<(), MemoryError>;
}

impl Backend for SoftwareAddressSpace {
    fn map_anonymous(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        Self::map_anonymous(self, range, permissions)
    }

    fn map_alias(
        &self,
        range: GuestRange,
        physical_start: GuestPa,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        Self::map_alias(self, range, physical_start, permissions)
    }

    fn load_region(
        &self,
        address: GuestVa,
        virtual_size: u32,
        initial: &[u8],
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        Self::load_region(self, address, virtual_size, initial, permissions)
    }

    fn protect(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        Self::protect(self, range, permissions)
    }
}

impl Backend for WindowsAddressSpace {
    fn map_anonymous(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        Self::map_anonymous(self, range, permissions)
    }

    fn map_alias(
        &self,
        range: GuestRange,
        physical_start: GuestPa,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        Self::map_alias(self, range, physical_start, permissions)
    }

    fn load_region(
        &self,
        address: GuestVa,
        virtual_size: u32,
        initial: &[u8],
        permissions: MemoryPermissions,
    ) -> Result<GuestPa, MemoryError> {
        Self::load_region(self, address, virtual_size, initial, permissions)
    }

    fn protect(
        &self,
        range: GuestRange,
        permissions: MemoryPermissions,
    ) -> Result<(), MemoryError> {
        Self::protect(self, range, permissions)
    }
}

/// One generated backend operation.
#[derive(Debug, Clone)]
enum Op {
    MapAnonymous { start: u32, len: u64, permissions: MemoryPermissions },
    MapAlias { start: u32, len: u64, physical: u32, permissions: MemoryPermissions },
    LoadRegion { start: u32, virtual_size: u32, payload_len: usize, permissions: MemoryPermissions },
    Protect { start: u32, len: u64, permissions: MemoryPermissions },
    Write { address: u32, len: usize, seed: u8 },
    Read { address: u32, len: usize },
    Fetch { address: u32, len: usize },
}

/// A deterministic linear congruential generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn generated_permissions(generator: &mut Lcg) -> MemoryPermissions {
    MemoryPermissions::from_bits_retain(generator.below(8) as u8)
}

/// Generates one operation inside a small page window.
///
/// One map input in eight is unaligned, so both backends must reject it
/// with identical typed failures. Physical offsets can exceed the section.
fn generated_op(generator: &mut Lcg) -> Op {
    let byte_address = |generator: &mut Lcg| generator.below(u64::from(WINDOW_PAGES) * PAGE) as u32;
    let map_start = |generator: &mut Lcg| {
        let page = (generator.below(u64::from(WINDOW_PAGES)) as u32) << 12;
        if generator.below(8) == 0 { page | (generator.below(PAGE) as u32) } else { page }
    };
    let map_len = |generator: &mut Lcg| {
        let pages = (generator.below(4) + 1) * PAGE;
        if generator.below(8) == 0 { pages + generator.below(PAGE) + 1 } else { pages }
    };
    let physical = |generator: &mut Lcg| {
        let page = (generator.below(80) as u32) << 12;
        if generator.below(8) == 0 { page | (generator.below(PAGE) as u32) } else { page }
    };

    match generator.below(9) {
        0 | 1 => Op::MapAnonymous {
            start: map_start(generator),
            len: map_len(generator),
            permissions: generated_permissions(generator),
        },
        2 => Op::MapAlias {
            start: map_start(generator),
            len: map_len(generator),
            physical: physical(generator),
            permissions: generated_permissions(generator),
        },
        3 => Op::LoadRegion {
            start: map_start(generator),
            virtual_size: generator.below(3 * PAGE) as u32 + 1,
            payload_len: generator.below(2 * PAGE) as usize,
            permissions: generated_permissions(generator),
        },
        4 => Op::Protect {
            start: map_start(generator),
            len: map_len(generator),
            permissions: generated_permissions(generator),
        },
        5 | 6 => Op::Write {
            address: byte_address(generator),
            len: generator.below(2 * PAGE) as usize + 1,
            seed: generator.below(256) as u8,
        },
        7 => Op::Read {
            address: byte_address(generator),
            len: generator.below(2 * PAGE) as usize + 1,
        },
        _ => Op::Fetch { address: byte_address(generator), len: generator.below(64) as usize + 1 },
    }
}

/// Applies one operation and returns a comparable outcome string.
fn apply(backend: &dyn Backend, op: &Op) -> String {
    fn outcome<T>(result: Result<T, MemoryError>, ok: impl FnOnce(T) -> String) -> String {
        match result {
            Ok(value) => ok(value),
            Err(error) => format!("error: {error}"),
        }
    }

    match op {
        Op::MapAnonymous { start, len, permissions } => {
            match GuestRange::new(GuestVa(*start), *len) {
                Ok(range) => outcome(backend.map_anonymous(range, *permissions), |physical| {
                    format!("mapped {physical}")
                }),
                Err(error) => format!("range error: {error}"),
            }
        }
        Op::MapAlias { start, len, physical, permissions } => {
            match GuestRange::new(GuestVa(*start), *len) {
                Ok(range) => {
                    outcome(backend.map_alias(range, GuestPa(*physical), *permissions), |()| {
                        "aliased".to_owned()
                    })
                }
                Err(error) => format!("range error: {error}"),
            }
        }
        Op::LoadRegion { start, virtual_size, payload_len, permissions } => {
            let payload: Vec<u8> = (0..*payload_len).map(|index| (index % 251) as u8).collect();
            outcome(
                backend.load_region(GuestVa(*start), *virtual_size, &payload, *permissions),
                |physical| format!("loaded {physical}"),
            )
        }
        Op::Protect { start, len, permissions } => match GuestRange::new(GuestVa(*start), *len) {
            Ok(range) => outcome(backend.protect(range, *permissions), |()| "protected".to_owned()),
            Err(error) => format!("range error: {error}"),
        },
        Op::Write { address, len, seed } => {
            let payload: Vec<u8> =
                (0..*len).map(|index| (index as u8).wrapping_add(*seed)).collect();
            outcome(backend.write(GuestVa(*address), &payload), |()| "written".to_owned())
        }
        Op::Read { address, len } => {
            let mut output = vec![0_u8; *len];
            outcome(backend.read(GuestVa(*address), &mut output), |()| format!("read {output:?}"))
        }
        Op::Fetch { address, len } => {
            let mut output = vec![0_u8; *len];
            outcome(backend.fetch(GuestVa(*address), &mut output), |()| {
                format!("fetched {output:?}")
            })
        }
    }
}

/// Compares every readable window byte between both backends.
fn require_identical_window(software: &SoftwareAddressSpace, windows: &WindowsAddressSpace) {
    for page in 0..WINDOW_PAGES {
        let address = GuestVa(page << 12);
        let mut software_bytes = vec![0_u8; GUEST_PAGE_SIZE as usize];
        let mut windows_bytes = vec![0_u8; GUEST_PAGE_SIZE as usize];
        let software_result = software.read(address, &mut software_bytes);
        let windows_result = windows.read(address, &mut windows_bytes);

        match (software_result, windows_result) {
            (Ok(()), Ok(())) => {
                assert_eq!(software_bytes, windows_bytes, "page {page} bytes must match");
            }
            (Err(software_error), Err(windows_error)) => {
                assert_eq!(
                    software_error.to_string(),
                    windows_error.to_string(),
                    "page {page} failures must match"
                );
            }
            (software_result, windows_result) => {
                panic!(
                    "page {page} outcome diverged: software {software_result:?}, \
                     windows {windows_result:?}"
                );
            }
        }
    }
}

fn run_generated_scenario(seed: u64, op_count: usize) {
    let software = SoftwareAddressSpace::new(PHYSICAL_BYTES).expect("software space is valid");
    let windows = WindowsAddressSpace::new(PHYSICAL_BYTES).expect("windows space is valid");
    let mut generator = Lcg(seed);

    for index in 0..op_count {
        let op = generated_op(&mut generator);
        let software_outcome = apply(&software, &op);
        let windows_outcome = apply(&windows, &op);
        assert_eq!(software_outcome, windows_outcome, "seed {seed} op {index} diverged: {op:?}");
    }

    require_identical_window(&software, &windows);
}

#[test]
fn generated_scenarios_match_across_backends() {
    for seed in [1, 7, 42, 20260815] {
        run_generated_scenario(seed, 300);
    }
}

#[test]
fn permission_matrix_matches_across_backends() {
    for bits in 0..8_u8 {
        let permissions = MemoryPermissions::from_bits_retain(bits);
        let software = SoftwareAddressSpace::new(PHYSICAL_BYTES).expect("software space is valid");
        let windows = WindowsAddressSpace::new(PHYSICAL_BYTES).expect("windows space is valid");
        let range = GuestRange::page_aligned(GuestVa(0x1000), PAGE).expect("range is valid");
        software.map_anonymous(range, permissions).expect("software mapping succeeds");
        windows.map_anonymous(range, permissions).expect("windows mapping succeeds");

        for op in [
            Op::Read { address: 0x1000, len: 16 },
            Op::Write { address: 0x1000, len: 16, seed: 9 },
            Op::Fetch { address: 0x1000, len: 16 },
        ] {
            assert_eq!(
                apply(&software, &op),
                apply(&windows, &op),
                "permissions {permissions:?} diverged on {op:?}"
            );
        }
    }
}

#[test]
fn execute_fetch_requires_execute_permission_on_both_backends() {
    let software = SoftwareAddressSpace::new(PHYSICAL_BYTES).expect("software space is valid");
    let windows = WindowsAddressSpace::new(PHYSICAL_BYTES).expect("windows space is valid");
    let range = GuestRange::page_aligned(GuestVa(0x1000), PAGE).expect("range is valid");
    let readable = MemoryPermissions::READ | MemoryPermissions::WRITE;
    software.map_anonymous(range, readable).expect("software mapping succeeds");
    windows.map_anonymous(range, readable).expect("windows mapping succeeds");

    let mut byte = [0_u8; 1];
    let software_error =
        software.fetch(GuestVa(0x1000), &mut byte).expect_err("software fetch must fail");
    let windows_error =
        windows.fetch(GuestVa(0x1000), &mut byte).expect_err("windows fetch must fail");
    assert!(matches!(software_error, MemoryError::AccessDenied { .. }));
    assert!(matches!(windows_error, MemoryError::AccessDenied { .. }));
    assert_eq!(software_error.to_string(), windows_error.to_string());

    let executable = MemoryPermissions::READ | MemoryPermissions::EXECUTE;
    software.protect(range, executable).expect("software protect succeeds");
    windows.protect(range, executable).expect("windows protect succeeds");
    software.fetch(GuestVa(0x1000), &mut byte).expect("software fetch succeeds");
    windows.fetch(GuestVa(0x1000), &mut byte).expect("windows fetch succeeds");
}

#[test]
fn fixed_edge_cases_match_across_backends() {
    let software = SoftwareAddressSpace::new(PHYSICAL_BYTES).expect("software space is valid");
    let windows = WindowsAddressSpace::new(PHYSICAL_BYTES).expect("windows space is valid");
    let readable = MemoryPermissions::READ | MemoryPermissions::WRITE;

    // The top guest page must accept accesses that end at the address-space
    // limit and reject accesses that overflow it.
    let top = GuestRange::page_aligned(GuestVa(0xFFFF_F000), PAGE).expect("range is valid");
    software.map_anonymous(top, readable).expect("software mapping succeeds");
    windows.map_anonymous(top, readable).expect("windows mapping succeeds");

    for op in [
        Op::Write { address: 0xFFFF_FFF0, len: 16, seed: 3 },
        Op::Read { address: 0xFFFF_FFF0, len: 16 },
        Op::Read { address: 0xFFFF_FFF0, len: 17 },
        Op::Read { address: 0xFFFF_F000, len: 0x2000 },
    ] {
        assert_eq!(apply(&software, &op), apply(&windows, &op), "diverged on {op:?}");
    }

    // Zero-length accesses succeed everywhere on both backends.
    let mut empty = [0_u8; 0];
    software.read(GuestVa(0xDEAD_BEEF), &mut empty).expect("software empty read succeeds");
    windows.read(GuestVa(0xDEAD_BEEF), &mut empty).expect("windows empty read succeeds");

    // Cross-page writes that hit a permission wall fail identically and
    // leave the first page unchanged on both backends.
    let guarded = GuestRange::page_aligned(GuestVa(0x4000), 2 * PAGE).expect("range is valid");
    software.map_anonymous(guarded, readable).expect("software mapping succeeds");
    windows.map_anonymous(guarded, readable).expect("windows mapping succeeds");
    let second = GuestRange::page_aligned(GuestVa(0x5000), PAGE).expect("range is valid");
    software.protect(second, MemoryPermissions::READ).expect("software protect succeeds");
    windows.protect(second, MemoryPermissions::READ).expect("windows protect succeeds");

    for op in [Op::Write { address: 0x4FFF, len: 2, seed: 5 }, Op::Read { address: 0x4FFE, len: 4 }]
    {
        assert_eq!(apply(&software, &op), apply(&windows, &op), "diverged on {op:?}");
    }
}
