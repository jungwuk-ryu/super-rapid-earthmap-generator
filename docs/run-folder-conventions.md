# Run Folder Conventions

Prefer external artifact roots:

```text
D:\earthmap\quality\photo-parity\vNNN-description
D:\earthmap\agent-runs\YYYYMMDD-short-task
```

For `quality-production-sample-batch`, each sample writes:

```text
<outputRoot>/<sample>/world
<outputRoot>/<sample>/photo-parity
<outputRoot>/<sample>/preview-debug          # only when previewDebug=auto|dir
<outputRoot>/<sample>/quality-production-sample.properties
<outputRoot>/quality-production-sample-summary.csv
<outputRoot>/quality-production-sample-contact-sheet.png
```

Only the short experiment index belongs in the repo. Large images, worlds, logs, and generated reports stay outside
the repo and are summarized in `docs/EXPERIMENTS.md`.

For Rust `quality-candidate`, each sample writes:

```text
<outputRoot>/<sample>/region
<outputRoot>/<sample>/rust-quality-evidence/quality-candidate.json
<outputRoot>/<sample>/rust-quality-evidence/preview.png
<outputRoot>/<sample>/rust-quality-evidence/quality-stats.json
<outputRoot>/<sample>/rust-quality-evidence/top-block-distribution.csv
<outputRoot>/<sample>/rust-quality-evidence/biome-distribution.csv
<outputRoot>/<sample>/rust-quality-evidence/region-payload-manifest.csv
<outputRoot>/<sample>/rust-quality-evidence/region-payload-manifest-rerun.csv
<outputRoot>/<sample>/rust-quality-evidence/playability-smoke.json
<outputRoot>/raster-smoke.json
<outputRoot>/writer-benchmark/region-writer-benchmark.json
```
