# Compatibility Policy

## Status levels

Use these levels for future compatibility reports:

- `not-tested` means no recorded result exists.
- `load-fails` means checked image loading fails.
- `boots` means guest entry execution starts.
- `menu` means the primary menu becomes usable.
- `in-game` means interactive title logic starts.
- `playable` means normal use completes with minor defects.
- `complete` means no known material defect remains.

Do not infer one level from screenshots alone.

## Evidence

Record these fields for each result:

- Exbawks commit.
- Title identity metadata.
- Input hash.
- Host CPU.
- Windows build.
- Graphics backend.
- Configuration.
- Reproduction steps.
- Known defects.

Do not publish copyrighted input files.

## Title-specific behavior

Add a compatibility rule only after a reproducible technical finding.

Document the guest behavior and the selected workaround.

Keep a rule narrow enough to avoid unrelated titles.

Add a regression test when public synthetic data can reproduce the behavior.

## Performance claims

Record frame pacing and emulation speed separately.

Include the host hardware and test scene.

Do not compare results from different emulator commits without disclosure.
