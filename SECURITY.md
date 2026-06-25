# Security Policy

## Supported Versions

The public repository tracks the current `main` branch. Security fixes should target the latest branch unless a
maintainer states otherwise.

## Reporting A Vulnerability

Please do not open a public issue for exploitable security problems.

Report privately through GitHub Security Advisories when available, or contact the repository owner through the listed
GitHub maintainer channel. Include:

- affected command, script, or crate
- reproduction steps
- impact and whether untrusted input is required
- relevant logs without secrets or private data

## Security Scope

Important areas include:

- parsing untrusted GeoTIFF/VRT/PNG/CSV/JSON/properties input
- output path handling and overwrite behavior
- RCON helper scripts and server automation
- dependency advisories and binary release provenance

Do not commit secrets, tokens, private server addresses, or third-party datasets.
