//! Implementation burndown across the emulator's HLE surfaces.
//!
//! The road to a booting title is a burndown across three surfaces: guest
//! instructions the interpreter oracle covers, kernel HLE ordinals, and GPU
//! methods. This module is the generic ledger; each subsystem supplies its
//! own items, and the application renders them (miette for the runtime
//! diagnosis, ariadne for the annotated call site).

use std::fmt;

use miette::Diagnostic;
use thiserror::Error;

/// One emulation surface tracked for implementation burndown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Guest instruction coverage in the interpreter oracle tier.
    Cpu,
    /// Kernel HLE export ordinals.
    Kernel,
    /// NV2A graphics methods.
    Gpu,
}

impl Surface {
    /// Returns the lowercase surface name used in diagnostic codes and CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Kernel => "kernel",
            Self::Gpu => "gpu",
        }
    }
}

impl fmt::Display for Surface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The implementation status of one surface element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    /// A real implementation with semantics.
    Implemented,
    /// A registered placeholder that does not halt but has no semantics.
    Stub,
    /// No implementation; reaching it stops the run.
    Missing,
}

impl CoverageStatus {
    /// Returns the lowercase status name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::Stub => "stub",
            Self::Missing => "missing",
        }
    }
}

/// One tracked element of a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageItem {
    /// A surface-local identifier (ordinal, opcode class, or method).
    pub id: u32,
    /// The element name.
    pub name: String,
    /// The implementation status.
    pub status: CoverageStatus,
    /// An optional note (calling convention, group, blocking task).
    pub note: Option<String>,
}

/// The coverage of one surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceCoverage {
    /// The surface these items belong to.
    pub surface: Surface,
    /// The tracked elements.
    pub items: Vec<CoverageItem>,
}

impl SurfaceCoverage {
    /// Creates a surface coverage set.
    #[must_use]
    pub fn new(surface: Surface, items: Vec<CoverageItem>) -> Self {
        Self { surface, items }
    }

    /// Counts items with one status.
    #[must_use]
    pub fn count(&self, status: CoverageStatus) -> usize {
        self.items.iter().filter(|item| item.status == status).count()
    }

    /// Returns the total tracked element count.
    #[must_use]
    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// Returns the implemented fraction as a percentage in `0..=100`.
    #[must_use]
    pub fn percent_implemented(&self) -> u32 {
        let total = self.total();
        if total == 0 {
            return 100;
        }
        u32::try_from(self.count(CoverageStatus::Implemented) * 100 / total).unwrap_or(100)
    }

    /// Returns the missing items, which are what a burndown targets next.
    #[must_use]
    pub fn missing(&self) -> impl Iterator<Item = &CoverageItem> {
        self.items.iter().filter(|item| item.status == CoverageStatus::Missing)
    }
}

/// A burndown ledger across surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageLedger {
    /// The tracked surfaces.
    pub surfaces: Vec<SurfaceCoverage>,
}

impl CoverageLedger {
    /// Adds one surface's coverage.
    pub fn push(&mut self, surface: SurfaceCoverage) {
        self.surfaces.push(surface);
    }

    /// Returns the coverage for one surface, when present.
    #[must_use]
    pub fn surface(&self, surface: Surface) -> Option<&SurfaceCoverage> {
        self.surfaces.iter().find(|entry| entry.surface == surface)
    }
}

/// A coverage gap reached at runtime, rendered as a rich diagnostic.
///
/// The application renders this through miette so a boot that stops at an
/// unimplemented surface element explains itself and points at the burndown.
#[derive(Debug, Clone, Error, Diagnostic)]
#[error("{surface} surface reached an unimplemented element: {name}")]
#[diagnostic(code(exbawks::coverage::gap))]
pub struct CoverageGap {
    /// The surface the gap belongs to.
    pub surface: Surface,
    /// The surface-local identifier.
    pub id: u32,
    /// The element name.
    pub name: String,
    /// Burndown context shown as diagnostic help.
    #[help]
    pub help: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, status: CoverageStatus) -> CoverageItem {
        CoverageItem { id, name: format!("item{id}"), status, note: None }
    }

    #[test]
    fn counts_and_percentage_track_status() {
        let coverage = SurfaceCoverage::new(
            Surface::Kernel,
            vec![
                item(1, CoverageStatus::Implemented),
                item(2, CoverageStatus::Implemented),
                item(3, CoverageStatus::Stub),
                item(4, CoverageStatus::Missing),
            ],
        );
        assert_eq!(coverage.total(), 4);
        assert_eq!(coverage.count(CoverageStatus::Implemented), 2);
        assert_eq!(coverage.count(CoverageStatus::Missing), 1);
        assert_eq!(coverage.percent_implemented(), 50);
        assert_eq!(coverage.missing().count(), 1);
    }

    #[test]
    fn empty_surface_reports_full_coverage() {
        let coverage = SurfaceCoverage::new(Surface::Gpu, Vec::new());
        assert_eq!(coverage.percent_implemented(), 100);
    }
}
