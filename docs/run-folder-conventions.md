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
<outputRoot>/<sample>/quality-production-sample.properties
```

Only the short experiment index belongs in the repo. Large images, worlds, logs, and generated reports stay outside
the repo and are summarized in `docs/EXPERIMENTS.md`.
