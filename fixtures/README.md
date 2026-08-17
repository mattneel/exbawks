# Fixtures

Store only synthetic and redistributable test data in this directory.

Do not commit commercial XBE files, BIOS images, dashboard files, keys, or extracted game data.

Unit tests currently construct minimal XBE images in memory.

Use `fixtures/private` for local files. Git ignores that directory.

## Private goldens

`fixtures/private` also holds golden manifests, one `<name>.golden` per title, naming a
local image and the frame digest it renders:

```text
image = C:/games/title/default.xbe
max_blocks = 8000000
ram_mib = 128
frame = e5fd3002468274f8
```

Point `EXBAWKS_PRIVATE_FIXTURES` at the directory holding them and run `just goldens`
(or `cargo test -p exbawks-core --test private_goldens -- --ignored`). The suite is
ignored by default and inert without that variable, so a clean checkout still runs
`cargo test`. Record a digest with `exbawks run --frame-digest`, and only after looking
at the frame it names.
