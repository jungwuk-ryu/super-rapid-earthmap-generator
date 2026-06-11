# Module Rules

The active project uses a Rust workspace layout.

Keep crate boundaries clear:

- `earthmap-cli`: command entrypoint, argument adaptation, user-facing output, and thin command-family modules.
- `earthmap-core`: shared command metadata and cross-crate policy types.
- `earthmap-geo`: Earth scale mapping, GeoTIFF/VRT readers, raster caches, and sampling helpers.
- `earthmap-surface`: surface sampling, photo material solving, and region surface generation.
- `earthmap-minecraft`: chunk, NBT, palette, heightmap, and level.dat primitives.
- `earthmap-region`: MCA and Linear region file read/write/validation primitives.
- `earthmap-quality`: renderers, metrics, and contact-sheet harnesses.
- `earthmap-gameplay`: survival validators and delegated gameplay helpers.
- `earthmap-osm`: OSM scan, validation, extraction, and mask generation.
- `earthmap-parity`: parity harness helpers.
- `earthmap-gui`: native GUI wrapper around the Rust CLI process and progress stream.

Incremental extraction inside a crate is allowed when the public command/output contract is already covered by tests.
Do not introduce new crate boundaries until the solver, CLI command, and generation-context boundaries are stable under
the Rust-only gates.
