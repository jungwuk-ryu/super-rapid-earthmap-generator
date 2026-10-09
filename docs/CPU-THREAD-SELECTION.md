# Automatic generation threads

The parallel `generate` grid and plan commands accept `auto` in the thread
argument. New GUI settings enable **Threads → Auto** by default. Existing saved
GUI settings and projects with numeric thread settings retain manual mode.

```powershell
.\scripts\run.ps1 cpu-info
.\scripts\run.ps1 generate $env:EARTHMAP_HEIGHTMAP $env:EARTHMAP_OUTPUT_ROOT `
  1000 26 -10 3 3 linear auto surface surfaceRaster=auto
```

`cpu-info` requires no raster data. It reports the detected physical and logical
CPU counts, performance levels, effective CPU budget, initial compute-thread
count, and candidate worker/pool combinations. Generation progress includes
the same topology in `workerTuningStarted` and `workerTuning`.

## Detection

| Platform | Source | What is considered |
| --- | --- | --- |
| Windows | `GetSystemCpuSetInformation`, queried for the generator process | Processor groups, physical-core indices, SMT, every reported `EfficiencyClass`, default process CPU sets, exclusive allocation, and a partial single-group affinity mask |
| macOS | `sysctl hw.nperflevels`, `hw.perflevelN.*`, `hw.physicalcpu`, `hw.logicalcpu` | Every reported performance level in OS performance order; no fixed assumption of two levels or specific P/E names |
| Linux | Online/allowed CPU lists, sysfs SMT siblings, Intel `cpu_core`/`cpu_atom` PMU CPU lists, or `cpu_capacity` | Process affinity, online CPUs, physical cores, SMT, Intel hybrid classes when exposed, and multiple ARM capacity levels |
| Other/unsupported environment | Rust `available_parallelism()` | Available CPU count with conservative fallback |

Automatic counts are bounded by Rust's `available_parallelism()` and the
process-visible CPU topology. Rust accounts for supported CPU affinity and
container CPU quotas. `EARTHMAP_CPU_BUDGET=N` can further reduce this budget;
it cannot increase it. In automatic mode, GUI shard processes divide this
budget between the configured processes, with a minimum of one thread per
process. The process count remains the user's setting; more processes than
available CPUs necessarily share CPUs.

The Windows probe uses PowerShell with profiles disabled and managed interop;
Rust remains under `#![forbid(unsafe_code)]`. Both Windows and macOS probes have
a five-second limit per invocation. Missing commands, restricted PowerShell,
unavailable APIs, or incomplete metadata fall back to known counts. Partial
class data never silently labels unknown cores as efficiency cores. On Linux,
a kernel that does not expose Intel hybrid PMUs may provide physical/SMT
counts without P/E identification.

## Selection

1. Start with the fastest detected level's physical-core count on a hybrid CPU,
   or all physical cores on a homogeneous CPU. Unknown topology uses the CPU
   budget. All counts stay within the automatic compute ceiling.
2. Build compute-thread hypotheses from cumulative performance levels, all
   physical cores, and all available logical CPUs. Include both physical and
   SMT counts; class ranks are ordering information, not speed multipliers.
3. Combine those counts with region-worker candidates, including the legacy
   one/two/four-worker choices and detected physical-core boundaries. A numeric
   thread argument caps region workers and limits automatic compute threads to
   at most twice that number. `auto` permits the full effective CPU budget.
4. With `workerAutotune=true`, the existing startup tuner generates complete
   sample regions, including land/ocean/mixed samples when available, warms up,
   and confirms the best candidates. It picks the smallest configuration within
   5% of the fastest measured time per region.

For an illustrative **8 P cores with SMT + 8 E cores**, the topology contributes
compute counts **8, 16, 24**. For a reported three-level **2 + 6 + 4** CPU without
SMT, it contributes **2, 8, 12**. Balanced region/compute combinations add smaller
counts. A four-CPU quota caps either example at four automatic compute threads.
These are thread-count hypotheses, not CPU placement guarantees.

The tuner limits region coordinators to 16 so its maximum 32-region sample can
exercise at least two regions per coordinator when the submitted batch is large
enough. This does not impose a 16-thread compute cap: nested column work can use
the whole detected CPU budget. The former universal 16-thread compute limit is
removed.

Batches below eight regions and requests for one region worker skip startup
tuning. `workerAutotune=false` also skips it; these paths use the initial
topology-based count. Tuning adds startup work, so include that time when
benchmarking end-to-end generation. Selection happens once per run; it does not
continuously adapt to temperature, battery/power modes, or other applications.

## Overrides and scheduling

`RAYON_NUM_THREADS=N` fixes the compute pool at exactly N threads, including
every tuning candidate. Region-worker counts never enlarge that pool and are
bounded by it. This explicit override may exceed the automatically detected
budget. For a controlled four-thread run:

```powershell
$env:RAYON_NUM_THREADS = "4"
.\scripts\run.ps1 generate $env:EARTHMAP_HEIGHTMAP $env:EARTHMAP_OUTPUT_ROOT `
  1000 26 -10 3 3 linear 4 surface surfaceRaster=auto workerAutotune=false
```

CPU placement remains with the OS scheduler. This change does not pin workers
to P cores, E cores, or a particular Apple cluster. Rayon distributes queued
column work dynamically through work stealing. Frequency, cache sharing,
memory bandwidth, and unequal core throughput are reflected in measured
candidate performance rather than fixed vendor/model lookup tables.

World sampling, terrain/material decisions, region encoding, and compression
settings remain the same. Thread mode is recorded as `generation.threadMode`
in project metadata and is excluded from the world-content resume fingerprint.

## Validation scope

Topology and policy regression fixtures cover Intel P/E plus SMT, Windows
processor groups, Linux allowed/offline CPUs, three capacity/performance levels,
Apple/Intel macOS metadata, incomplete metadata, quota caps, and exact compute
overrides. GUI tests cover automatic/manual commands, old-settings compatibility,
project restore, and shard budgets. The embedded Windows probe also compiles
under PowerShell's managed C# compiler.

All 403 workspace tests, including the previously ignored generation fixtures,
pass. Workspace Clippy passes with warnings denied. The CPU module type-checks
for `aarch64-apple-darwin` and `x86_64-pc-windows-gnu`; these checks do not link or
run the full application on those platforms.

On the Linux four-CPU quota, `auto` selected four region workers and four compute
threads. All 64 complete compressed Linear files matched the previous build by
SHA-256. An eight-region CSV-plan run with `RAYON_NUM_THREADS=2` kept two compute
threads in every initial/confirmation candidate and in production; its files
also matched. The 64-region automatic run took 23.82 seconds **including startup
tuning**, a single diagnostic run rather than a speedup measurement. See the
[validation record](benchmarks/cpu-thread-selection-2026-10-10.json).

Native hybrid Intel/Windows and Apple Silicon execution and performance have
not been measured in this Linux workspace. Use `cpu-info` and the generation
tuning progress on each target machine to inspect detected levels and selected
counts. The earlier [3.03× measurements](CONTINENT-GENERATION-PERFORMANCE.md)
describe the previous four-CPU Linux benchmark; they do not establish a speedup
for these other CPUs or for this topology follow-up.
