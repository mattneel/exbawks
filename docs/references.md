# Technical References

Use primary documentation and active open-source implementations when possible.

## Rust and code generation

- Rust release announcements: `https://blog.rust-lang.org/releases/`
- Cargo manifest format: `https://doc.rust-lang.org/cargo/reference/manifest.html`
- iced repository: `https://github.com/icedland/iced`
- iced-x86 Rust documentation: `https://docs.rs/iced-x86/`
- Cranelift documentation: `https://github.com/bytecodealliance/wasmtime/tree/main/cranelift/docs`

## Windows virtual memory

- VirtualAlloc2: `https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-virtualalloc2`
- MapViewOfFile3: `https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile3`
- UnmapViewOfFile2: `https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-unmapviewoffile2`
- MapViewOfFile coherency: `https://learn.microsoft.com/windows/win32/api/memoryapi/nf-memoryapi-mapviewoffile`
- IsProcessorFeaturePresent: `https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-isprocessorfeaturepresent`

## Xbox executable format

- XboxDevWiki XBE page: `https://xboxdevwiki.net/Xbe`
- Cxbx-Reloaded XBE definitions: `https://github.com/Cxbx-Reloaded/Cxbx-Reloaded/blob/master/src/common/xbe/Xbe.h`
- radare2 XBE definitions: `https://github.com/radareorg/radare2/blob/master/libr/bin/format/xbe/xbe.h`

## Source policy

Do not copy proprietary SDK text or code.

Record new sources in this document before a major compatibility implementation.
