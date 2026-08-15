# Contributing

## Prepare the repository

Run the bootstrap script on Windows:

```powershell
.\scripts\bootstrap.ps1
```

## Create a change

Create one focused branch.

Add tests before or with the implementation.

Update the related design document.

Do not add proprietary Xbox data.

## Verify the change

Run these commands:

```powershell
python scripts/static-validate.py
cargo xtask check
```

If the command fails, fix the first reported error.

## Submit the change

Describe the guest behavior that changed.

Describe each unsafe invariant.

List the tests that cover the change.

Link an ADR when the change affects architecture.

## Commit style

Use an imperative summary under 72 characters.

Use a subsystem prefix when it adds useful context.

Examples:

```text
memory: add physical alias invalidation
jit: classify string memory operations
xbe: reject section ranges outside the file
```
