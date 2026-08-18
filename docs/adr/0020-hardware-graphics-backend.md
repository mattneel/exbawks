# ADR 0020: A hardware graphics backend

## Status

Proposed.

## Context

The graphics engine rasterizes in software, on one thread, in this process.
That was the right way to get the picture correct: every step is inspectable,
every frame is reproducible, and a recorded digest means something. It is how
the title screen was made to render at all, and it is what the golden tests
compare against.

It is also why the emulator cannot be played. Measured by removing one stage
at a time from a full run of the retail title: eight seconds of guest
execution and geometry, twenty-four in the register combiners, twenty-four in
texture filtering, about thirteen in the rest of sampling, and about nine in
the remaining per-pixel work. **Ninety percent of a run is pixels.** Holding
guest RAM for a whole submission rather than going through the address space
per four bytes took a run from 186 seconds to 79. Three further attempts —
stepping the edge functions, caching decoded compressed blocks, compiling the
combiner program — were measured and were no faster or slower. The per-pixel
path is close to what a scalar software rasterizer costs, and what remains is
the work itself: 388 million pixels a run, each sampling two textures with
four texels apiece and running a two-stage combiner.

xemu does not rasterize in software. It registers a `PGRAPHRenderer` with
operations for `draw_begin`, `draw_end`, `surface_update`, `clear_surface`
and the rest, and implements that interface over OpenGL and Vulkan, with a
null renderer as the fallback. Its `psh.c` and `vsh-ff.c` do not interpret
the console's shaders; they generate GLSL from them. The console's vertex
program and register combiners become host shader source, and the host
graphics processor draws the pixels.

That is the difference in kind. No arrangement of the scalar loop reaches it.

## Decision

The graphics engine gains a second backend that translates what it has
already decoded into host graphics work, and the software rasterizer stays.

The decode is not the part that changes. The pushbuffer walk, the method
numbering, texture formats and swizzling, mip chains, texgen, the fixed
pipeline's lighting, the combiner semantics, and vertex assembly are all
title-facing work that took this project a long time to get right, and all of
it is the *input* to a backend rather than part of one. What a hardware
backend adds is a translation of that decoded state into pipeline state,
vertex buffers, textures, and generated shaders.

The two backends are not alternatives of equal standing:

- **The software rasterizer is the reference.** It is deterministic, it is
  what the recorded digests were taken from, and a golden may only be
  recorded from it. It stays the default.
- **The hardware backend is for playing.** Its output depends on the host's
  graphics driver and will not be pixel-identical, so a run using it may not
  record a golden — the same rule live controller input already carries.

`GraphicsBackend` already exists as a trait with a null implementation, so
the seam is where it needs to be; what it carries today is too coarse and
will widen to describe pipeline state, bound textures, and shader programs.

## Consequences

The emulator becomes playable, which is the point: a person can watch the
title, press a button, and see what happens, at a frame rate that makes the
result mean something.

It costs a second renderer to keep correct, and two renderers disagreeing is
a new class of defect. That is what the software reference is for: a frame
from the hardware backend can be compared against one from the software
rasterizer on the same input, and a disagreement is a bug in the new backend
rather than a mystery.

The console's shaders must be generated rather than interpreted, which is the
substantial new work: the vertex program's instruction encoding is already
decoded and would be emitted as host shader source, and the combiners'
inputs, mappings, and scales would become a fragment shader. Both are already
understood well enough to execute, which is most of what generating them
requires.

Nothing about the title-facing decode changes, so the goldens continue to
mean what they meant, and a regression in the shared path shows up in both
backends at once.
