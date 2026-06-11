# External Data Policy

Status reviewed: 2026-06-12.

This repository contains source code, documentation, small research tables, and metadata pins. It must not contain
third-party geospatial datasets or Minecraft/server binaries.

## Do Not Commit

- HeightMap GeoTIFFs
- TrueMarble or other satellite raster tiles, VRTs, previews, or derived imagery
- `TifFiles/` dataset roots
- Minecraft server jars, DivineMC jars, plugin jars, or mod packs
- Generated Minecraft worlds, region files, Dynmap tiles, quality images, and benchmark output
- Local crash dumps, WER archives, logs, and agent-run ledgers

## Expected Local Layout

Use environment variables for local data:

```text
EARTHMAP_HEIGHTMAP=<absolute path to heightmap GeoTIFF>
EARTHMAP_DATA_ROOT=<absolute path to local data root>
EARTHMAP_TIF_ROOT=<absolute path to local TifFiles root>
EARTHMAP_SURFACE_RASTER=<absolute path to terrain/TrueMarble.vrt or equivalent raster>
EARTHMAP_OUTPUT_ROOT=<absolute path to generated output root>
```

The GUI and CLI may reference `terrain/TrueMarble.vrt` by convention, but that file is user-provided external data.
It is not bundled with the project.

## Attribution

Document the source and license/terms for each local dataset in your own run notes. Do not copy those datasets into this
repository unless their license explicitly permits redistribution and the maintainers approve the addition.
