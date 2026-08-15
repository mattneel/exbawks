# Graphics HLE

## Scope

The Xbox graphics API differs from desktop Direct3D 8.

Exbawks will intercept high-level API calls and lower push-buffer commands where required.

The first host backend target is a modern Windows graphics API. The exact D3D11 or D3D12 choice remains open.

## Frontend boundary

Guest-facing code emits host-neutral graphics commands.

Host backends must not parse guest pointers directly. The frontend validates and copies or pins each guest range.

## Resource model

Use stable emulator identifiers for these resources:

- Devices.
- Surfaces.
- Textures.
- Vertex buffers.
- Index buffers.
- Shaders.
- Palettes.

Do not expose host COM pointers to guest code.

## State tracking

Keep one explicit guest graphics state object.

Coalesce redundant state changes before host submission.

Record unknown state values in traces before fallback behavior.

## Shaders

Start with captured fixed-function and known shader paths.

Add a shader translation cache keyed by guest microcode and relevant render state.

Store generated host shader source or IR in debug artifacts when a trace flag requests it.

## Push buffers

Parse push buffers with checked bounds.

Stop on unknown packet forms in strict mode.

Record the packet address, method, and payload for diagnostics.

Do not trust guest-declared packet lengths.

## Presentation

The host backend owns window, swap-chain, and presentation state.

The emulator core sends an explicit present command. It does not call a host graphics API directly.

## Current implementation

The repository defines host-neutral commands and a null backend.

The null backend records counts for deterministic tests.
