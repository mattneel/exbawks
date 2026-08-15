# Security Policy

## Supported versions

Only the latest `main` branch receives security fixes during early development.

## Report a vulnerability

Do not open a public issue for an exploitable host vulnerability.

Send a private report through the repository security advisory feature.

Include these facts:

- The affected commit.
- The host operating system.
- The input type.
- The failure path.
- A minimal synthetic reproducer when possible.

Do not attach copyrighted game or firmware data.

## Security boundaries

Treat every XBE file as untrusted input.

Treat every guest pointer as untrusted input.

Treat generated code and fault metadata as security-sensitive data.

The emulator does not provide a strong sandbox yet. Do not run hostile files.
