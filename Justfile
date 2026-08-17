# Exbawks development tasks. Run `just` to list recipes.
#
# Recipes shared with `cargo xtask` delegate to it. The required check
# sequence stays defined once, in xtask/src/main.rs.

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

synthetic_xbe := "fixtures/synthetic/minimal-retail.xbe"

[private]
default:
    @just --list --unsorted

# Install the pinned toolchain and fetch dependencies.
[windows]
[group("setup")]
bootstrap:
    ./scripts/bootstrap.ps1

# Report host runtime capabilities.
[group("setup")]
doctor:
    cargo exbawks doctor

# Run repository checks that need no Rust toolchain.
[group("checks")]
validate:
    python scripts/static-validate.py

# Run the required check sequence: fmt, check, clippy, test, doc.
[group("checks")]
check:
    cargo xtask check

# Run everything CI runs: static validation, required checks, smoke test.
[group("checks")]
ci: validate check smoke

# Format the workspace.
[group("checks")]
fmt:
    cargo xtask fmt

# Check formatting without changing files.
[group("checks")]
fmt-check:
    cargo xtask fmt-check

# Check all workspace targets.
[group("checks")]
build:
    cargo xtask build

# Run Clippy with warnings denied.
[group("checks")]
lint:
    cargo xtask lint

# Run all tests.
[group("checks")]
test:
    cargo xtask test

# Build workspace documentation.
[group("checks")]
doc:
    cargo xtask doc

# Check the dependency policy. Requires cargo-deny.
[group("checks")]
deny:
    cargo deny check

# Run the synthetic CLI smoke test from CI.
[group("emulator")]
smoke:
    cargo exbawks plan '{{synthetic_xbe}}' --json

# Run the private golden frames (needs EXBAWKS_PRIVATE_FIXTURES set).
[group("emulator")]
goldens:
    cargo test --release -p exbawks-core --test private_goldens -- --ignored --nocapture

# Parse and describe one XBE file.
[group("emulator")]
inspect xbe *flags:
    cargo exbawks inspect '{{ join(invocation_directory_native(), xbe) }}' {{flags}}

# Decode a hexadecimal 32-bit x86 byte sequence.
[group("emulator")]
decode hex ip="0x00010000" *flags:
    cargo exbawks decode --ip '{{ip}}' --hex '{{hex}}' {{flags}}

# Load an XBE and create an entry-block translation plan.
[group("emulator")]
plan xbe ram="64" *flags:
    cargo exbawks plan '{{ join(invocation_directory_native(), xbe) }}' --ram-mib '{{ram}}' {{flags}}

# Read the terminated kernel import thunk table.
[group("emulator")]
thunks xbe *flags:
    cargo exbawks thunks '{{ join(invocation_directory_native(), xbe) }}' {{flags}}

# Perform the implemented load and planning stages.
[group("emulator")]
run xbe ram="64" *flags:
    cargo exbawks run '{{ join(invocation_directory_native(), xbe) }}' --ram-mib '{{ram}}' {{flags}}

# Report the implementation burndown across emulator surfaces.
[group("emulator")]
coverage *flags:
    cargo exbawks coverage {{flags}}

# Create a numbered ADR from the template.
[windows]
[group("maintenance")]
adr slug title:
    ./scripts/new-adr.ps1 -Slug '{{slug}}' -Title '{{replace(title, "'", "''")}}'

# Regenerate the synthetic XBE fixture.
[group("maintenance")]
fixture:
    python scripts/make-synthetic-xbe.py '{{synthetic_xbe}}'

# Remove build artifacts.
[group("maintenance")]
clean:
    cargo clean
