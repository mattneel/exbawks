# Validation Report

## Scope

This report records checks completed in the repository creation environment.

The environment provides Python 3.13. It does not provide Cargo, Rustc, Rustfmt, or a Windows host.

## Completed checks

The repository static validator checks these areas:

- Required files.
- UTF-8 text and final newlines.
- TOML syntax.
- YAML syntax when PyYAML is available.
- Python syntax.
- Cargo workspace membership and crate roots.
- Rust module paths.
- Balanced Rust delimiters.
- Local Markdown links.
- Synthetic XBE reproducibility.
- Safety comments near unsafe blocks.
- Forbidden private fixture extensions.

Run the validator with this command:

```powershell
python scripts/static-validate.py
```

The final result appears below after repository packaging.

## Checks that require Rust

Run these commands on a host with Rust 1.97.1:

```powershell
cargo xtask check
cargo exbawks doctor
cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe --json
```

These commands verify formatting, compilation, Clippy, tests, documentation, and the CLI smoke path.

All three commands passed on Windows 11 x86-64 with Rust 1.97.1 on August 15, 2026.

## Checks that require Windows 11

The Windows-specific tests passed on Windows 11 x86-64 on August 15, 2026.

The verified behavior includes:

- Pagefile section creation.
- Placeholder reservation, aligned reservation, split, coalesce, and release.
- Section-view replacement at 4 KiB page granularity.
- Coherent alias mappings through one physical section.
- Host protection changes on mapped views.
- Backend equivalence between the software and Windows address spaces.

Future vectored exception redirection still needs verification.

## Known implementation limits

The direct backend emits and executes the register-only subset on Windows. Memory operands, guest control flow, and fault redirection remain untranslated; blocks that reach them exit through the runtime.

The loader accepts genuine retail XBEs whose sections are byte-contiguous and share boundary pages (ADR 0007). A real retail image loads, decodes, and executes up to its first memory-operand or control-flow instruction, then stops with `UnsupportedInstruction`. Real titles reach memory operands almost immediately, so translated execution of a commercial title needs the next JIT tier.

Portable hosts create translation plans only. The Cranelift backend reports that it is unavailable.

Runtime guest writes do not yet bump physical-page generations, so self-modifying guest code is not detected during execution. The code cache tracks physical-page generations (ADR 0005); wiring automatic write detection is future work. The milestone title is not self-modifying.

The kernel gate region at `0xFF80_0000` is not reserved in the sidecar page table. The loader controls all guest mappings, so no synthetic path maps there; reserving the region is future hardening.

Registered but unimplemented kernel exports halt the run with `UnimplementedKernelExport` rather than continuing past a stub with an unbalanced guest stack.

The software address space updates physical generation values through an explicit method. Guest writes do not trigger code invalidation automatically.

The XBE loader supports the initial header and section subset. It does not parse certificates, TLS metadata, library tables, or debug metadata.

The kernel and graphics crates define dispatch contracts. They do not implement a title-compatible export set or a renderer.

## Final result

The static validator passed on August 15, 2026.

It reported these results:

- 13 required files.
- 118 UTF-8 text files.
- 17 TOML files.
- 7 YAML files.
- 2 Python files.
- 12 workspace members.
- 39 Rust module declarations.
- 51 Rust files with balanced delimiters.
- 34 valid local Markdown links.
- 12 unsafe blocks with nearby safety comments.
- One reproducible synthetic XBE fixture.
- No forbidden private fixture extensions.

The repository contains 124 files, 5,321 Rust lines, 37 Markdown files, and 38 Rust tests.

Rust compilation and Windows runtime checks did not run in this environment.
The first Windows handoff task must run every command in the Rust and Windows sections above.
