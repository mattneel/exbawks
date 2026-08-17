# ADR 0019: Guest input through an emulated USB controller

## Status

Accepted.

## Context

The retail title renders its title screen and then waits for a button that
cannot arrive. Reading back the registers it wrote settles what it is waiting
on: `HcControl` at `0xFED0_0004` holds `0xBE`, so the first OHCI host
controller is in `UsbOperational` with its control, bulk, and periodic lists
enabled, and `HcHCCA` at `0xFED0_0018` holds `0x010D_A000`, so the title has
allocated the area the controller writes completions into. The root port
status registers report nothing connected, because the device stub answers
every unwritten register with zero. The title's USB stack is running and
enumerating an empty bus.

Nothing above that layer can help. A title reaches its gamepad through XAPI's
`XInput*` functions, and XAPI is statically linked into the executable exactly
as Direct3D is — the same reason the graphics work went to the pushbuffer
rather than to call interception. There is no import to implement and no thunk
to patch; the title contains its own USB driver and drives the controller
registers directly.

Input is also the first guest-visible state in this emulator that comes from
outside the machine. Everything modelled so far is a pure function of the
image and the clock, which is what makes a recorded frame digest mean
anything. A controller read at an arbitrary moment would make every run
different from the last.

## Decision

The emulator models the OHCI host controller and a synthetic Xbox gamepad
attached to its first root port, and the title enumerates that gamepad with
its own driver. The controller walks the endpoint and transfer descriptor
lists the title builds in guest memory, answers the standard control requests
that enumeration issues, and completes the interrupt transfer the driver polls
with a twenty-byte gamepad report.

The guest-facing model is pure logic in its own crate, reaching guest memory
through a narrow trait in the same shape as the graphics engine's, so it stays
portable and testable and never sees a host pointer. Reading a real controller
is a host operation and lives in `exbawks-platform` with the rest of them.

Input is opt-in, and the default is no controller at all. A run with no input
source attached reports an empty port, behaves exactly as it does today, and
stays a pure function of the image — which is what the recorded frame digests
depend on. Three sources exist:

- **None**, the default: the port is empty.
- **Scripted**: a recorded sequence of button states advanced by frame number,
  which is deterministic and is what any test involving input must use.
- **Live**: a controller read from the host, which makes a run
  non-reproducible by construction. A run using it may not record a golden.

The synthetic gamepad is described by its own descriptors rather than by
forwarding the host device's. A DualSense and an Xbox controller do not
present the same descriptors, endpoints, or report layout, and the title's
driver is written against the Xbox one; the host device is a source of button
and axis values, not a device to be passed through.

## Consequences

The title can be given a button press, which is the only way past a screen
that asks for one, and the menu behind it becomes reachable for the first
time.

The device model gains a component that writes into guest memory on its own
schedule rather than only answering reads. That is what the hardware does, and
it means a defect here corrupts guest memory rather than merely returning a
wrong value; the descriptor walk is bounded and every guest-supplied pointer
is checked before it is followed, as the pushbuffer engine's already are.

Determinism becomes a property of the configured input source rather than of
the emulator as a whole. The golden manifest gains no input by default, so
existing recorded digests keep their meaning, and any future golden that
involves input must name a scripted source.

Only the first controller is modelled, because the title only programs the
first. The second remains a stub reporting an empty bus.
