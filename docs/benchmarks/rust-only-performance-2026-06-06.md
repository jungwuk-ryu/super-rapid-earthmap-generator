# Rust-only performance evidence (2026-06-06)

Cold start means a fresh OS process for each command. Timings use wall-clock elapsed time; memory is direct process working-set peak sampled while the process is running. Large generated artifacts stay outside the repo at `D:\earthmap\rust-only-performance-2026-06-06-artifacts`.

| Case | Java s | Rust s | Speedup | Java MiB | Rust MiB | Result | Workload |
|---|---:|---:|---:|---:|---:|---|---|
| benchmark-height-regions | 2.272 | 1.759 | 1.29x | 4.7 | 5.0 | Rust faster | One-region height benchmark command |
| photo-parity-metric-crop | 0.613 | 0.071 | 8.63x | 4.2 | 3.9 | Rust faster | Single 16x16 PNG metric crop |
| photo-production-candidate-diff-crop | 0.383 | 0.040 | 9.57x | 3.8 | 3.9 | Rust faster | Single 16x16 candidate diff crop |
| photo-standard-remap-parity-batch-5-crop | 0.418 | 0.051 | 8.20x | 4.5 | 3.9 | Rust faster | Five 4x4 Standard remap parity crops, two worker threads |
| photo-standard-remap-parity-crop | 0.376 | 0.041 | 9.17x | 4.1 | 4.0 | Rust faster | Single 16x16 Standard remap parity crop |
| plan-representative-regions | 0.236 | 0.042 | 5.62x | 4.2 | 3.9 | Rust faster | Four representative regions from synthetic GeoTIFF |
| quality-production-sample-batch-5-sample | 28.254 | 12.881 | 2.19x | 3.8 | 4.0 | Rust faster | Five-sample cold-start batch with previewDebug=auto on synthetic 1-region MCA fixture |
| photo-parity-metric-batch-5-crop | 0.738 | 0.114 | 6.47x | 4.1 | 3.9 | Rust faster | Five 4x4 PNG metric crops, two worker threads |
| scan-osm-pbf | 0.207 | 0.060 | 3.45x | 4.1 | 3.6 | Rust faster | Tiny PBF scan, max 2 blobs |
| validate-cave-density | 0.293 | 0.044 | 6.66x | 4.0 | 3.9 | Rust faster | Synthetic cave density validator |
| validate-height-seam | 0.196 | 0.045 | 4.36x | 4.3 | 3.9 | Rust faster | Synthetic GeoTIFF east seam validator, scale 10 command-contract workload |
| validate-linear-region | 0.354 | 0.044 | 8.05x | 4.3 | 3.8 | Rust faster | Flat Linear V2 region structural validator |
| validate-mca-region | 0.229 | 0.073 | 3.14x | 3.8 | 4.7 | Rust faster | Flat MCA region structural validator |
| validate-osm-pbf | 0.192 | 0.043 | 4.47x | 4.0 | 3.9 | Rust faster | Tiny PBF validation, max 2 blobs |
| validate-surface-spawn | 0.296 | 0.107 | 2.77x | 3.8 | 3.9 | Rust faster | Synthetic GeoTIFF surface spawn validator |
| validate-cave-connectivity | 0.274 | 0.053 | 5.17x | 3.9 | 3.8 | Rust faster | Synthetic cave connectivity validator |
| photo-carrier-remap-sim-crop | 0.379 | 0.038 | 9.97x | 4.1 | 3.9 | Rust faster | Single 16x16 carrier remap simulation crop |
| mca-topdown-render-7x6 | 121.106 | 63.682 | 1.90x | 5.0 | 3.9 | Rust faster | 7x6 MCA region mosaic, visible mode |
| linear-topdown-render-7x6 | N/A | 63.178 | N/A | N/A | 3.9 | Rust-only | 7x6 valid Linear V2 region mosaic converted from the MCA fixture, visible mode |
| benchmark-region-writers | N/A | 2.526 | N/A | N/A | 3.8 | Rust-only | Rust MCA/Linear writer microbenchmark, 3 iterations |
| benchmark-surface-regions | 14.125 | 12.674 | 1.11x | 3.8 | 3.9 | Rust faster | Six-region surface benchmark command to amortize cold-start overhead |
| benchmark-survival-regions | 5.238 | 3.288 | 1.59x | 4.2 | 3.9 | Rust faster | One-region survival benchmark command |
| benchmark-survival-regions-parallel | 4.954 | 4.746 | 1.04x | 4.2 | 3.9 | Rust faster | One-region parallel survival benchmark command |
| compare-mca-linear-region-payloads | 12.776 | 0.075 | 170.35x | 4.1 | 3.5 | Rust faster | Flat MCA/Linear payload comparison smoke |
| convert-mca-world-to-linear | 0.616 | 0.135 | 4.56x | 3.4 | 4.1 | Rust faster | One-region MCA world conversion to Linear V2 |
| describe-earth-grid | 0.195 | 0.042 | 4.64x | 3.7 | 3.8 | Rust faster | Synthetic GeoTIFF earth-grid description |
| dynmap-tile-mosaic-base | 0.422 | 0.044 | 9.59x | 4.0 | 3.9 | Rust faster | Small Dynmap base-level tile mosaic |
| extract-osm-region-mask-full-scan | 0.238 | 0.058 | 4.10x | 3.5 | 3.9 | Rust faster | Tiny PBF full-scan mask with progress file |
| full-1-1000-korea-3x3-existing-evidence | 353.995 | 33.650 | 10.52x | N/A | N/A | Rust faster | Existing 1:1000 Korea 3x3 Linear cold-start production run with TrueMarble surface raster. |
| generate-nation-war-readiness-report | 0.262 | 0.041 | 6.39x | 3.8 | 3.9 | Rust faster | Synthetic GeoTIFF readiness report |
| generate-production-alias-1-region | 29.441 | 4.760 | 6.19x | 4.5 | 4.8 | Rust faster | Production generate alias, one MCA region using TrueMarble VRT |
| generate-survival-region | 5.208 | 3.297 | 1.58x | 4.3 | 4.1 | Rust faster | One survival alias MCA region |
| generate-survival-region-osm-pbf | 5.111 | 3.268 | 1.56x | 4.2 | 3.8 | Rust faster | Tiny PBF OSM overlay survival MCA region |
| generate-survival-regions-parallel | 5.338 | 4.785 | 1.12x | 3.8 | 3.8 | Rust faster | One survival parallel MCA region |
| vanilla-delegated-plan-parallel | 27.273 | 4.761 | 5.73x | 4.4 | 3.9 | Rust faster | One planned MCA region using TrueMarble VRT |
| write-vanilla-finalization-commands | 0.205 | 0.044 | 4.66x | 3.9 | 3.8 | Rust faster | One-region vanilla finalization command file |

Raw process stdout/stderr and output artifacts are recorded under the artifact root above; JSON details are in `docs/benchmarks/rust-only-performance-2026-06-06.json`.
