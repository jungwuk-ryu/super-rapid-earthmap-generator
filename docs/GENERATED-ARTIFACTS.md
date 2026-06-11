# Generated Artifacts

## Do Not Commit

- `build/`
- `*.log`
- `hs_err_pid*.log`
- generated worlds
- generated `.mca` and `.linear` region files
- quality runs
- preview tiles and mosaics
- server logs
- agent-run ledgers
- external raster data such as `*.tif`, `*.tiff`, `*.vrt`, and `TifFiles/`

## Archive Roots

Keep reset/archive material outside source-controlled paths. Prefer:

```text
<EARTHMAP_OUTPUT_ROOT>/archive/<description>
out/archive/<description>
```

Archive folders may contain old run logs/docs, accidental repo-root output folders, and long handoff/playbook files.

## Quality Output Convention

Use external artifact roots:

```text
<EARTHMAP_OUTPUT_ROOT>/quality/photo-parity/vNNN-description
<EARTHMAP_OUTPUT_ROOT>/quality-acceptance-vNNN-description
out/quality/photo-parity/vNNN-description
out/quality-acceptance-vNNN-description
```

Each candidate should have a short entry in `docs/EXPERIMENTS.md` with status: accepted, rejected, or research-only.
