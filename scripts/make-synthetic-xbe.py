#!/usr/bin/env python3
"""Create the public synthetic XBE used by documentation and smoke tests."""

from __future__ import annotations

import argparse
from pathlib import Path

ENTRY_RETAIL_XOR = 0xA8FC57AB
KERNEL_RETAIL_XOR = 0x5B6D40B6
EXECUTABLE_SECTION = 0x00000004


def put_u32(buffer: bytearray, offset: int, value: int) -> None:
    buffer[offset : offset + 4] = value.to_bytes(4, "little")


def make_image() -> bytes:
    buffer = bytearray(0x282)
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
    put_u32(buffer, 0x208, 2)
    put_u32(buffer, 0x20C, 0x280)
    put_u32(buffer, 0x210, 2)
    put_u32(buffer, 0x214, base + 0x238)
    buffer[0x238:0x23E] = b".text\0"
    buffer[0x280:0x282] = b"\x90\xC3"
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
