# Module Rules

The project currently uses a JDK-only source layout, not Maven/Gradle modules.

Keep package boundaries clear:

- `net.earthmap.cli`: command entrypoint and argument adaptation
- `net.earthmap.geo`: raster readers and caches
- `net.earthmap.terrain`: surface sampling, solver, and region generation
- `net.earthmap.minecraft`: chunk/NBT/region primitives
- `net.earthmap.quality`: renderers, metrics, and contact-sheet harnesses
- `net.earthmap.gameplay`: survival validators and delegated gameplay helpers

Do not move to physical modules until the solver, CLI command, and generation-context boundaries are stable.
