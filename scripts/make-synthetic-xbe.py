#!/usr/bin/env python3
"""Create the public synthetic XBE used by documentation and smoke tests."""

from __future__ import annotations

import argparse
from pathlib import Path

ENTRY_RETAIL_XOR = 0xA8FC57AB
KERNEL_RETAIL_XOR = 0x5B6D40B6
EXECUTABLE_SECTION = 0x00000004

# The boot title for the first execution milestone:
#   0x11000  mov eax, 5
#   0x11005  add eax, 0x25        ; translated work before the kernel call (eax=42)
#   0x11008  mov esi, eax         ; esi=42, preserved across the call
#   0x1100A  call [0x11200]       ; DbgPrint through the first thunk gate
#   0x11010  mov edi, eax         ; edi = returned status (translated work after return)
#   0x11012  call [0x11204]       ; HalReturnToFirmware requests a guest exit
#   0x11018  ret
BOOT_CODE = bytes(
    [
        0xB8, 0x05, 0x00, 0x00, 0x00,  # mov eax, 5
        0x83, 0xC0, 0x25,  # add eax, 0x25   -> eax = 42
        0x89, 0xC6,  # mov esi, eax          -> esi preserved across the call
        0xFF, 0x15, 0x00, 0x12, 0x01, 0x00,  # call [0x11200]  DbgPrint
        0x89, 0xC7,  # mov edi, eax          -> edi = returned status
        0xFF, 0x15, 0x04, 0x12, 0x01, 0x00,  # call [0x11204]  HalReturnToFirmware
        0xC3,  # ret
    ]
)

# Kernel import ordinals: DbgPrint (8) and HalReturnToFirmware (49).
BOOT_THUNKS = [0x80000008, 0x80000031]


def put_u32(buffer: bytearray, offset: int, value: int) -> None:
    buffer[offset : offset + 4] = value.to_bytes(4, "little")


def make_image() -> bytes:
    buffer = bytearray(0x580)
    buffer[0:4] = b"XBEH"
    base = 0x00010000

    put_u32(buffer, 0x104, base)
    put_u32(buffer, 0x108, 0x280)
    put_u32(buffer, 0x10C, 0x4000)
    put_u32(buffer, 0x110, 0x178)
    put_u32(buffer, 0x118, base + 0x178)
    put_u32(buffer, 0x11C, 1)
    put_u32(buffer, 0x120, base + 0x200)
    put_u32(buffer, 0x128, (base + 0x1000) ^ ENTRY_RETAIL_XOR)
    put_u32(buffer, 0x130, 0x10000)
    put_u32(buffer, 0x134, 0x100000)
    put_u32(buffer, 0x138, 0x1000)
    put_u32(buffer, 0x158, (base + 0x1200) ^ KERNEL_RETAIL_XOR)

    put_u32(buffer, 0x200, EXECUTABLE_SECTION)
    put_u32(buffer, 0x204, base + 0x1000)
    put_u32(buffer, 0x208, 0x300)
    put_u32(buffer, 0x20C, 0x280)
    put_u32(buffer, 0x210, 0x300)
    put_u32(buffer, 0x214, base + 0x238)
    buffer[0x238:0x23E] = b".text\0"

    buffer[0x280 : 0x280 + len(BOOT_CODE)] = BOOT_CODE
    for index, value in enumerate(BOOT_THUNKS):
        put_u32(buffer, 0x480 + index * 4, value)
    return bytes(buffer)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "output",
        nargs="?",
        type=Path,
        default=Path("fixtures/synthetic/minimal-retail.xbe"),
    )
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(make_image())
    print(args.output)


if __name__ == "__main__":
    main()
