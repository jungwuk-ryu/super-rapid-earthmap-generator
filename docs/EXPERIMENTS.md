# Experiments

Newest first. Keep this file short; long logs belong under `D:\earthmap\agent-runs` or `D:\earthmap\archive`.

## v123 production canopy shadow hook

- Path: `D:\earthmap\quality\photo-parity\v123-production-canopy-shadow-hook`
- Status: rejected as release evidence.
- Reason: skipped thresholds, partial/timeout history, missing complete same-candidate pass.

## v122 Standard remap parity

- Path: `D:\earthmap\quality\photo-parity\v122-standard-remap-parity`
- Status: accepted as toolchain evidence only.
- Result: Java Standard remap matches ImageMagick remap on the required crops.
- Limitation: does not prove generated-world visual quality.

## v121 selective canopy harness

- Path: `D:\earthmap\quality\photo-parity\v121-selective-canopy-harness`
- Status: useful hypothesis, not production proof.
- Result: source parity improved on the five land crops.
- Limitation: Standard-target metrics regressed, and the candidate was harness-only.
