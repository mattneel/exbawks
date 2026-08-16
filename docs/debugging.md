# Debugging

## Logging

Set `RUST_LOG` before the command.

```powershell
$env:RUST_LOG = "exbawks=debug"
cargo exbawks plan C:\path\to\default.xbe
```

Use `trace` only for focused runs. Instruction traces can become large.

## Structured traces

The debug crate defines structured trace events.

`exbawks run --trace <file>` writes one JSON object per line. Kernel-call
records carry the verified export name when the ordinal table knows it.

Restrict output to selected event kinds with
`--trace-filter block,kernel,graphics,memory,stop` (comma-separated).
Filtered files keep monotonic sequence numbers without gaps.

Future JSON Lines output will include these event groups:

- Block decode and compile.
- MMIO accesses.
- Code invalidation.
- Exceptions and stop reasons.

## Crash dumps

Keep Windows crash dumps outside the repository.

Record the commit, command line, host build, and input hash with each dump.

Do not share a dump that contains proprietary guest data.

## JIT source maps

Each emitted block will map host offsets to guest instruction addresses.

Add the source map before the first executable backend lands.

A debugger command can then show both host and guest disassembly.

## Fault diagnosis

A registered JIT memory fault must include a fault-site record.

If no record matches, treat the fault as a host defect.

Do not silently convert arbitrary host access violations into guest faults.
