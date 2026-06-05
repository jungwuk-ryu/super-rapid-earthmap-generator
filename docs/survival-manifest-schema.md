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
`structure_references`, because those statuses are earlier in the Minecraft generation pipeline. EarthMap therefore
does not claim or inject stronghold-equivalent progression content during surface/world writing.

Generated exploration worlds must report:

```properties
generation.progressionStrategy=none
generation.directProgressionStructures=false
features.strongholdOrEquivalent=false
features.endPortal=false
features.netherProgression=false
features.lootTables=false
features.spawners=false
```

Any future survival-complete workflow must use a separate, explicit progression plan instead of hidden End portal,
stronghold loot, or spawner insertion inside the EarthMap generator.
