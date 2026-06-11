# MIT License Review

Status reviewed: 2026-06-12.

## Decision

MIT is suitable for publishing this repository as permissive open-source software, assuming the project owners intend to
allow broad use, modification, redistribution, sublicensing, and commercial use with attribution and warranty disclaimer.

This is an engineering release-readiness review, not legal advice.

## Basis

- SPDX lists `MIT` as a standard license identifier with canonical license text.
- GitHub documentation warns that without a license, default copyright law leaves reuse rights unclear; adding an
  open-source license is the correct public-repository move when reuse is intended.
- The current Rust dependency set is mostly permissive-license oriented from Cargo metadata inspection. There are
  non-MIT licenses in the graph, including Apache-2.0, BSD, BSL, ISC, Unicode, OFL, Ubuntu-font, Zlib, Unlicense, and
  CC0-style terms, so a future release should add automated license inventory checks before binary distribution.

## Boundaries

- The MIT license applies to this repository's source code and documentation unless a file says otherwise.
- It does not grant rights to external datasets, satellite rasters, HeightMap GeoTIFFs, TrueMarble-style imagery,
  Minecraft server jars, DivineMC builds, Mojang/Microsoft trademarks, or user-provided world data.
- External data must stay outside the repository and is governed by its own provider terms.

## Follow-Up

- Keep `license = "MIT"` in the Rust workspace metadata.
- Add automated dependency policy tooling, such as `cargo-deny`, before publishing packaged binaries or installers.
- Keep bundled assets limited to project-owned or procedurally generated material.
