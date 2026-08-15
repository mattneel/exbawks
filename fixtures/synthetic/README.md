# Synthetic fixtures

`minimal-retail.xbe` is a generated parsing, planning, and execution fixture.

The file contains one `.text` section with the first-milestone boot title:
register arithmetic, one `DbgPrint` thunk call, one `HalReturnToFirmware`
thunk call, and a kernel import table with both ordinals.
It contains no Microsoft code, signature, key, firmware, or game data.

Regenerate the file:

```powershell
python .\scripts\make-synthetic-xbe.py
```

Inspect and execute the file:

```powershell
cargo exbawks inspect .\fixtures\synthetic\minimal-retail.xbe
cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe
cargo exbawks run .\fixtures\synthetic\minimal-retail.xbe
```

On Windows the run command reports `GuestExit { code: 0 }`. `ESI` holds
`0x2A` (translated arithmetic that survived the kernel call), and `EDI`
holds the status `DbgPrint` returned for the register-only guest's null
format pointer.
