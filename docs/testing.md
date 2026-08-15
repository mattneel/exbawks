# Testing

## Required checks

Run the toolchain-independent validator first.

```powershell
python scripts/static-validate.py
```

Run the complete Rust checks next.

```powershell
cargo xtask check
```

## Test layers

Use four test layers.

### Unit tests

Test address arithmetic, page descriptors, XBE ranges, decoder limits, and HLE registration.

Use synthetic byte arrays.

### Integration tests

Build synthetic XBE images in memory.

Load each image through `exbawks-core`. Verify mappings, entry state, and translation plans.

### Windows host tests

Test placeholder reservation, section mapping, alias coherence, and page protection on Windows.

Keep these tests independent from Xbox data.

### Compatibility tests

Keep private compatibility inputs outside the repository.

Store only hashes, public metadata, and expected behavior when redistribution permits it.

## Determinism

Use a deterministic clock in tests.

Use fixed random seeds for fuzz reproductions.

Do not depend on host directory order.

## Fuzz targets

Add fuzz targets for these parsers:

- XBE image headers.
- XBE section tables.
- Kernel thunk tables.
- Push-buffer packets.
- Guest string and object structures.

A fuzz target must cap allocation and iteration counts.

## Differential tests

Use `iced-x86` instruction information as one source for decoder effects.

Compare direct and Cranelift backends after both execute the same normalized operations.

Compare software and Windows memory backends with generated mapping sequences.

## Performance tests

Keep performance tests outside normal unit tests.

Measure block decode time, translation time, code-cache lookup, alias invalidation, and HLE call overhead.

Record the host CPU and Windows build with each result.
