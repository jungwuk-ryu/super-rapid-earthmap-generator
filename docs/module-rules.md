# Module Rules

The active project uses a Rust workspace layout.

Keep crate boundaries clear:

- `earthmap-cli`: command entrypoint, argument adaptation, and user-facing output.
- `earthmap-core`: shared command metadata and cross-crate policy types.
- `earthmap-raster`: raster readers, caches, and sampling helpers.
- `earthmap-surface`: surface sampling, photo solver, and region generation.
- `earthmap-minecraft`: chunk, NBT, region, MCA, and Linear primitives.
- `earthmap-quality`: renderers, metrics, and contact-sheet harnesses.
- `earthmap-gameplay`: survival validators and delegated gameplay helpers.
- `earthmap-osm`: OSM scan, validation, extraction, and mask generation.

Do not merge crates or introduce new crate boundaries until the solver, CLI command, and generation-context boundaries
are stable under the Rust-only gates.
