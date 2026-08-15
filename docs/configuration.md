# Configuration

## Stable emulator settings

`EmulatorConfig` defines the current library settings.

| Setting | Default | Meaning |
| --- | ---: | --- |
| `physical_memory_bytes` | 64 MiB | Emulated physical RAM size. |
| `backend` | Direct rewrite | Selected code generation backend. |
| `max_block_instructions` | 256 | Decode limit for one block. |
| `max_block_bytes` | 4096 | Byte limit for one block. |
| `max_kernel_thunks` | 4096 | Thunk table inspection limit. |

The Cranelift backend returns an unavailable error in the current scaffold.

## Environment variables

`RUST_LOG` selects tracing filters.

`RUST_BACKTRACE` defaults to `1` through `.cargo/config.toml`.

No environment variable changes guest compatibility behavior yet.

## CLI commands

### `doctor`

Report host capabilities.

```powershell
cargo exbawks doctor
cargo exbawks doctor --json
```

### `inspect`

Parse an XBE and report checked metadata.

```powershell
cargo exbawks inspect C:\private\default.xbe
```

### `decode`

Decode one hexadecimal 32-bit x86 block.

```powershell
cargo exbawks decode --ip 0x00010000 --hex "8B 01 83 C0 01 C3"
```

### `plan`

Load an XBE and create an entry-block translation plan.

```powershell
cargo exbawks plan C:\private\default.xbe --backend direct --ram-mib 64
```

### `thunks`

Read a terminated kernel import thunk table.

```powershell
cargo exbawks thunks C:\private\default.xbe --limit 4096
```

### `run`

Perform the implemented load and planning stages.

```powershell
cargo exbawks run C:\private\default.xbe --ram-mib 64
```

The command does not execute translated code yet.

## JSON output

Place `--json` before or after the subcommand.

Use JSON output for automation and regression snapshots.
