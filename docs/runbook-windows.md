# Windows Development Runbook

## Prepare Visual Studio

Install Visual Studio 2022 Build Tools.

Select the Desktop development with C++ workload.

Install one current Windows SDK.

## Prepare Rust

Open PowerShell in the repository root.

Run this command:

```powershell
.\scripts\bootstrap.ps1
```

## Verify the host

Run this command:

```powershell
cargo exbawks doctor
```

Confirm that the target is Windows on x86-64.

## Run repository checks

Run these commands:

```powershell
python scripts/static-validate.py
cargo xtask check
```

## Inspect an input

Place private XBE files outside tracked directories.

Run this command:

```powershell
cargo exbawks inspect C:\private\default.xbe
```

## Collect a trace

Set the log level.

```powershell
$env:RUST_LOG = "exbawks=trace"
cargo exbawks plan C:\private\default.xbe *> exbawks.log
```

Remove private paths before sharing the log.
