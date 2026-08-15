# Repository Manifest

## Root files

| Path | Purpose |
| --- | --- |
| `README.md` | Project overview and start commands. |
| `AGENTS.md` | Mandatory coding-agent rules. |
| `Cargo.toml` | Workspace members, dependencies, and lint policy. |
| `rust-toolchain.toml` | Pinned Rust toolchain and components. |
| `rustfmt.toml` | Rust formatting policy. |
| `deny.toml` | Dependency source and license policy. |
| `CONTRIBUTING.md` | Contribution procedure. |
| `SECURITY.md` | Security reporting process. |
| `CHANGELOG.md` | User-visible project changes. |
| `VALIDATION.md` | Local validation results and remaining checks. |

## Applications

| Path | Purpose |
| --- | --- |
| `apps/exbawks-cli` | Host checks, XBE inspection, decode tools, and boot plans. |

## Libraries

| Crate | Responsibility |
| --- | --- |
| `exbawks-types` | Guest addresses, permissions, backend identifiers, and stop reasons. |
| `exbawks-platform` | Windows host capabilities and virtual-memory ownership. |
| `exbawks-memory` | Page metadata, software RAM, aliases, and arena plans. |
| `exbawks-xbe` | Checked XBE header and section parsing. |
| `exbawks-cpu` | Guest CPU state and `iced-x86` block decoding. |
| `exbawks-jit` | Translation plans, backend contracts, and code-cache metadata. |
| `exbawks-kernel` | Kernel HLE export registration and dispatch. |
| `exbawks-gpu` | Host-neutral graphics commands and a null backend. |
| `exbawks-debug` | Breakpoints and structured trace events. |
| `exbawks-core` | Emulator composition, XBE mapping, and entry planning. |

## Automation

| Path | Purpose |
| --- | --- |
| `xtask` | Format, check, lint, test, and documentation commands. |
| `scripts/bootstrap.ps1` | Windows toolchain preparation. |
| `scripts/check.ps1` | Repository check wrapper. |
| `scripts/run.ps1` | Current staged run wrapper. |
| `scripts/new-adr.ps1` | ADR creation helper. |
| `scripts/make-synthetic-xbe.py` | Public synthetic fixture generator. |
| `scripts/static-validate.py` | Toolchain-independent repository validation. |
| `.github/workflows/ci.yml` | Windows and portable Rust checks. |
| `.github/workflows/security.yml` | Scheduled dependency policy checks. |

## Documentation

`docs/README.md` links every design, runbook, policy, and ADR.

`docs/task-board.md` is the implementation queue for a coding agent.

`docs/agent-handoff.md` defines the immediate objective and stop conditions.

## Fixtures

`fixtures/synthetic/minimal-retail.xbe` contains generated test bytes only.

`fixtures/private` is ignored and reserved for lawful local inputs.

## Intentionally absent files

`Cargo.lock` is absent because this environment cannot run Cargo.

Generate and commit it during the first successful Windows validation.

Executable JIT output, a Windows arena backend, and graphics rendering remain future work.
