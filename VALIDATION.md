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

## Checks that require Windows 11

Run the Windows-specific tests on Windows 11 x86-64.

The current scaffold contains Windows section and placeholder wrappers. The sparse 4 GiB arena remains incomplete.

The following behavior still needs Windows verification:

- Pagefile section creation.
- Placeholder reservation and release.
- Section-view replacement.
- Coherent alias mappings.
- Memory protection changes.
- Future vectored exception redirection.

## Known implementation limits

The code does not execute translated guest instructions.

The direct backend creates translation plans only. The Cranelift backend reports that it is unavailable.

The software address space updates physical generation values through an explicit method. Guest writes do not trigger code invalidation automatically.

The XBE loader supports the initial header and section subset. It does not parse certificates, TLS metadata, library tables, or debug metadata.

The kernel and graphics crates define dispatch contracts. They do not implement a title-compatible export set or a renderer.

`Cargo.lock` is absent. Generate and commit it during the first successful Rust validation.

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
