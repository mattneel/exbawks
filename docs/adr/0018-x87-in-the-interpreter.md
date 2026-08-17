# ADR 0018: x87 in the interpreter

## Status

Accepted.

## Context

ADR 0008 puts x87 inside the interpreter's scope but does not say how an x87
value is represented, and the representation decides something the rest of the
project depends on: whether the interpreter can still be compared against the
hypervisor tier result for result.

The interpreter today models only the x87 control instructions — `fninit`,
`fnclex`, `fnstsw`, `fnstcw`, `fldcw` — which is enough for a C runtime to
satisfy itself that a floating-point unit exists. Every instruction that moves
or computes a value is unsupported, and three separate pieces of work stop
there:

- The write watchpoint (`exbawks run --watch-write`) steps the instruction that
  touches a watched page. The write it was built to catch is an `fstp`, so the
  run ends at the first one.
- The MMIO path single-steps the instruction that touched device space. A title
  that writes a device register from a floating-point register cannot be
  serviced.
- The oracle tier cannot execute any code path that passes through x87, which
  is most of a title's geometry setup.

The hardware register is 80-bit extended: a 64-bit mantissa and a 15-bit
exponent. Nothing in Rust offers that type, and no host primitive matches it —
x86-64 retains the x87 unit, but reaching it from Rust means inline assembly,
which would make results depend on the host's rounding state and put unsafe
code in a crate that forbids it.

## Decision

The interpreter keeps its x87 register stack as `f64`, converting at the memory
boundary: a load from an `m32fp`, `m64fp`, or `m80fp` operand widens to `f64`,
and a store narrows back to the operand's own format. The stack top lives in
the status word's bits 11 to 13, and the tag word tracks which registers hold
a value, so `fstp` and friends move the top exactly as hardware does and a
title reading the status word sees a consistent one.

`f64` is chosen because it is exact for everything that crosses the boundary a
title can observe. A title stores single or double precision to memory; both
survive a round trip through `f64` unchanged. The 11 extra mantissa bits of the
extended format are visible only to a computation that keeps a value in a
register across several operations and depends on the extra precision — which a
game's geometry code does not, and which its results are not authored against.

The alternatives were weighed and rejected:

- A software 80-bit float is exact and slow, and it is a large amount of
  delicate code whose only customer is a divergence nobody has observed.
- Host x87 through inline assembly is exact and introduces unsafe code, host
  rounding-mode dependence, and a result that varies with the host — the
  opposite of what an oracle is for.

## Consequences

The three blocked pieces of work unblock together: the watchpoint can step past
an `fstp`, the MMIO path can service a floating-point store to a device
register, and the oracle tier can follow a title through its geometry code.

The interpreter stops being a bit-exact oracle for x87. It remains exact for
every value that reaches memory in a 32-bit or 64-bit format, which is what a
comparison against the hypervisor tier actually reads; a disagreement in the
low mantissa bits of a long register-resident computation is expected rather
than a defect, and the hypervisor tier is authoritative there. Any equivalence
test that compares the two tiers over x87 code must compare stored values, not
register contents.

An `m80fp` store writes the value converted from `f64`, so a title that stores
an extended value and reloads it gets its own value back, but a title that
inspects the stored bytes sees a mantissa whose low bits are zero.
