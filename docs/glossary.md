# Glossary

## Guest

The emulated original Xbox environment.

## Host

The Windows computer that runs Exbawks.

## XBE

The executable image format used by original Xbox software.

## HLE

High-level emulation. Exbawks implements an interface through host code instead of reproducing its complete lower-level implementation.

## JIT

Just-in-time compilation. Exbawks translates guest code into host code during execution.

## Direct rewriting

A codegen method that retains safe guest instruction forms and rewrites operations that need host adaptation.

## Guest virtual address

A 32-bit address observed by guest software.

## Guest physical address

An offset into emulated physical memory.

## High arena

A host virtual range where the offset from the arena base equals the guest virtual address.

## Identity view

A host mapping whose host virtual address equals the guest virtual address.

## MMIO

Memory-mapped input and output. Accesses represent device operations instead of normal RAM.

## Physical-page generation

A counter that changes when code bytes on a guest physical page can change.

## Fault site

A generated host instruction location with metadata for an expected guest memory fault.
