# Survival Manifest Schema

Format: Java `.properties`.

The manifest is an operator-facing contract. It prevents a generated Earth world from being labeled `survival-complete` unless every progression-critical system has evidence.

## Required Keys

```properties
manifest.version=1
minecraft.version=1.21.11
gameplay.claim=exploration-only
evidence.serverBootSaveReboot=false
evidence.spawnToEnd=false
features.caves=false
features.caveConnectivity=false
features.ores=false
features.strongholdOrEquivalent=false
features.endPortal=false
features.netherProgression=false
features.lootTables=false
features.spawners=false
reports.oreHistogram=false
reports.caveConnectivity=false
reports.structureMetadata=false
reports.resourceFairness=false
```

## Claim Values

- `exploration-only`: terrain may be useful for viewing/testing, but survival completion is not claimed.
- `survival-candidate`: major systems exist but at least one required validator or server test is still missing.
- `survival-complete`: only allowed when all required boolean keys are `true` and `minecraft.version` equals the pinned target.

## Gate Rule

`validate-survival-manifest <path>` prints `survivalCompleteAllowed=true` only when:

- `gameplay.claim=survival-complete`.
- `minecraft.version=1.21.11`.
- Every required evidence, feature, and report key is `true`.

Any missing or false key blocks the survival-complete claim.

## Delegated Surface Progression

Prewritten `minecraft:surface` and `minecraft:carvers` chunks cannot delegate vanilla `structure_starts` or
`structure_references`, because those statuses are earlier in the Minecraft generation pipeline. Delegated EarthMap
worlds therefore use `generation.progressionStrategy=delegated-surface-direct-stronghold-equivalent`: the generator
places a land-safe underground End portal, stronghold loot chest, and blaze spawner directly, then encodes that chunk
as `minecraft:carvers` before server finalization. In this mode `generation.directStructures=false` still means
vanilla structure starts were not prewritten; `generation.directProgressionStructures=true` identifies the bounded
EarthMap progression-room insertion.

This preserves the photo surface contract because the room is below the selected land surface. It is still not enough
for `survival-complete` by itself; `reports.structureMetadata`, `evidence.serverBootSaveReboot`, and
`evidence.spawnToEnd` must be produced by validation before the manifest can pass the gate.
