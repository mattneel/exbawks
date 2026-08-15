# Synthetic fixtures

`minimal-retail.xbe` is a generated parser and boot-planning fixture.

The file contains one `.text` section with `NOP` and `RET` instructions.
It contains no Microsoft code, signature, key, firmware, or game data.

Regenerate the file:

```powershell
python .\scripts\make-synthetic-xbe.py
```

Inspect the file:

```powershell
cargo exbawks inspect .\fixtures\synthetic\minimal-retail.xbe
cargo exbawks plan .\fixtures\synthetic\minimal-retail.xbe
```
