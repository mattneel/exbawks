# Release Checklist

## Source checks

- [ ] Run `cargo xtask check` on Windows 11.
- [ ] Run portable workspace tests on Ubuntu.
- [ ] Run dependency policy checks.
- [ ] Commit the current `Cargo.lock`.
- [ ] Confirm that generated documentation has no warnings.

## Runtime checks

- [ ] Run `cargo exbawks doctor` on the release host.
- [ ] Run the synthetic XBE smoke test.
- [ ] Run all Windows virtual-memory tests.
- [ ] Verify that executable pages are not writable.
- [ ] Verify that expected faults use registered metadata.

## Documentation checks

- [ ] Update `CHANGELOG.md`.
- [ ] Update `docs/roadmap.md`.
- [ ] Update `docs/agent-handoff.md`.
- [ ] Review all accepted ADRs.
- [ ] Record known limitations.

## Legal and data checks

- [ ] Scan tracked files for proprietary Xbox data.
- [ ] Confirm that fixtures are synthetic or redistributable.
- [ ] Confirm that logs contain no private paths or keys.
- [ ] Include both license files in source archives.

## Package checks

- [ ] Build the release from a clean checkout.
- [ ] Record source archive checksums.
- [ ] Sign release artifacts.
- [ ] Test archive extraction on Windows.
- [ ] Publish the exact commit identifier.
