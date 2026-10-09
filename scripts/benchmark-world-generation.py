#!/usr/bin/env python3
"""Compare release binaries on real land, requiring identical region files."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time


def parse_case(value):
    try:
        name, *numbers = value.split(",")
        scale, x, z, cols, rows = map(int, numbers)
        if not name or not name.replace("-", "").replace("_", "").isalnum():
            raise ValueError("case name must use letters, numbers, '-' or '_'")
        if min(scale, cols, rows) < 1:
            raise ValueError("scale, cols and rows must be positive")
        return dict(name=name, scale=scale, x=x, z=z, cols=cols, rows=rows)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "expected NAME,SCALE,REGION_X,REGION_Z,COLS,ROWS: " + str(error)
        ) from error


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read_optional(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def run(binary, case, label, root, args, env):
    world = root / label
    # Each process owns a separate fresh world; their combined region coverage
    # equals the single-process case. The caller must match total CPU budgets.
    partitions = [case] if args.processes == 1 else [
        dict(case, x=case["x"] + x, z=case["z"] + z, cols=1, rows=1)
        for z in range(case["rows"]) for x in range(case["cols"])
    ]
    commands, logs = [], []
    for index, partition in enumerate(partitions):
        output_world = world if args.processes == 1 else world / str(index)
        logs.append(root / (label + ("" if args.processes == 1 else f"-{index}") + ".jsonl"))
        command = [
            str(binary), "generate", env["EARTHMAP_HEIGHTMAP"], str(output_world),
            str(case["scale"]), str(partition["x"]), str(partition["z"]),
            str(partition["cols"]), str(partition["rows"]), args.format, str(args.threads),
            "surface", "surfaceRaster=" + env["EARTHMAP_SURFACE_RASTER"],
            "verticalScale=auto", "compressionLevel=" + ("6" if args.format == "mca" else "4"),
            "workerAutotune=false", "prefetch=" + args.prefetch,
        ]
        if args.prefetch_workers is not None:
            command.append("prefetchWorkers=" + str(args.prefetch_workers))
        commands.append(command)
    peak_rss_kib = 0
    started = time.perf_counter()
    active, outputs, next_job = [], [], 0
    try:
        while active or next_job < len(commands):
            while len(active) < args.processes and next_job < len(commands):
                output = logs[next_job].open("w", encoding="utf-8")
                outputs.append(output)
                process = subprocess.Popen(commands[next_job], env=env, stdout=output,
                                           stderr=subprocess.STDOUT)
                active.append((process, logs[next_job], output))
                next_job += 1
            resident_kib = 0
            remaining = []
            for process, log, output in active:
                # Read actual children, not cumulative RUSAGE_CHILDREN. In
                # process mode the sum of per-child high waters is an upper
                # bound on the simultaneously resident generation workers.
                status = read_optional(f"/proc/{process.pid}/status") or ""
                for line in status.splitlines():
                    if line.startswith("VmHWM:"):
                        resident_kib += int(line.split()[1])
                code = process.poll()
                if code is None:
                    remaining.append((process, log, output))
                elif code:
                    raise RuntimeError(f"generation exited {code}; see {log}")
                else:
                    output.close()
            peak_rss_kib = max(peak_rss_kib, resident_kib)
            active = remaining
            if time.perf_counter() - started > args.timeout:
                raise RuntimeError(f"generation timed out; see {logs}")
            if active:
                time.sleep(0.02)
    finally:
        for process, _, _ in active:
            if process.poll() is None:
                process.kill()
            process.wait()
        for output in outputs:
            output.close()
    elapsed = time.perf_counter() - started
    events = [json.loads(line[6:]) for log in logs
              for line in log.read_text(encoding="utf-8").splitlines() if line.startswith("event\t")]
    generated = [event for event in events if event["type"] == "regionGenerated"]
    expected = case["cols"] * case["rows"]
    if len(generated) != expected or any(event["chunks"] != 1024 for event in generated):
        raise RuntimeError(f"expected {expected} freshly generated, complete regions; see {logs}")
    land = sum(event["landColumns"] for event in generated)
    water = sum(event["waterColumns"] for event in generated)
    if land / (land + water) < args.min_land_fraction:
        raise RuntimeError(f"{case['name']} does not meet the land coverage requirement")
    files = {path.name: digest(path) for path in sorted(world.rglob("*." + args.format))}
    if len(files) != expected:
        raise RuntimeError(f"expected {expected} region files in {world}")
    return dict(commands=commands, elapsed_seconds=elapsed, peak_rss_kib=peak_rss_kib or None,
                land_columns=land, water_columns=water, min_ground_y=min(e["minGroundY"] for e in generated),
                max_ground_y=max(e["maxGroundY"] for e in generated), sha256=files,
                regions=generated, world=str(world), logs=list(map(str, logs)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--case", type=parse_case, action="append", required=True)
    parser.add_argument("--output", type=Path, help="new, empty result directory")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--processes", type=int, default=1,
                        help="parallel one-region processes; use threads=1, rayon=1 for a fair CPU comparison")
    parser.add_argument("--rayon", type=int, help="override RAYON_NUM_THREADS equally for both binaries")
    parser.add_argument("--format", choices=["linear", "mca"], default="linear")
    parser.add_argument("--prefetch", choices=["true", "false"], default="false")
    parser.add_argument("--prefetch-workers", type=int, help="set producer count equally for both binaries")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--target", type=float, default=4.0)
    parser.add_argument("--min-land-fraction", type=float, default=0.95)
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--keep-worlds", action="store_true", help="retain all worlds; default keeps the first measured pair per case")
    args = parser.parse_args()
    if min(args.threads, args.processes, args.repeats, args.target, args.timeout) <= 0 or args.warmups < 0:
        parser.error("threads, processes, repeats, target and timeout must be positive; warmups must be nonnegative")
    if args.rayon is not None and args.rayon < 1:
        parser.error("rayon must be positive")
    if args.prefetch_workers is not None and args.prefetch_workers < 1:
        parser.error("prefetch-workers must be positive")
    if not 0 <= args.min_land_fraction <= 1:
        parser.error("min-land-fraction must be between 0 and 1")
    if len({case["name"] for case in args.case}) != len(args.case):
        parser.error("case names must be unique")
    env = dict(os.environ)
    for name in ["EARTHMAP_HEIGHTMAP", "EARTHMAP_SURFACE_RASTER"]:
        if not env.get(name):
            parser.error(f"set {name} to the actual dataset")
    # Detailed per-column clocks affect the workload; use normal production timing.
    env.pop("EARTHMAP_SURFACE_PHASE_DETAIL", None)
    env.setdefault("EARTHMAP_HEIGHTMAP_CACHE_ROWS", "64")
    env.setdefault("EARTHMAP_SURFACE_TILE_CACHE_ENTRIES", "32")
    if args.rayon is not None:
        env["RAYON_NUM_THREADS"] = str(args.rayon)
    binaries = {"before": args.before.resolve(), "after": args.after.resolve()}
    if any(not binary.is_file() for binary in binaries.values()):
        parser.error("both binaries must exist; build both with the same release profile")
    binary_hashes = {name: digest(binary) for name, binary in binaries.items()}
    if binary_hashes["before"] == binary_hashes["after"]:
        parser.error("before and after must be different binaries")
    root = args.output or Path(env.get("EARTHMAP_OUTPUT_ROOT", "output")) / (
        "generation-benchmark-" + time.strftime("%Y%m%d-%H%M%S")
    )
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    report = dict(target_speedup=args.target, format=args.format, processes=args.processes,
                  threads_per_process=args.threads, prefetch=args.prefetch,
                  prefetch_workers=args.prefetch_workers,
                  memory_measurement="VmHWM" if args.processes == 1 else "sum of active children's VmHWM (upper bound)",
                  binary_sha256=binary_hashes,
                  environment={k: env.get(k) for k in ["EARTHMAP_DATA_ROOT", "EARTHMAP_TIF_ROOT",
                      "EARTHMAP_HEIGHTMAP", "EARTHMAP_SURFACE_RASTER", "EARTHMAP_HEIGHTMAP_CACHE_ROWS",
                      "EARTHMAP_SURFACE_TILE_CACHE_ENTRIES", "RAYON_NUM_THREADS"]},
                  cpu_quota=read_optional("/sys/fs/cgroup/cpu.max"),
                  memory_limit=read_optional("/sys/fs/cgroup/memory.max"), cases=[])
    report_path = root / "report.json"
    for case in args.case:
        results = dict(case=case, runs=[], timings={"before": [], "after": []})
        expected_files = None
        for repeat in range(-args.warmups, args.repeats):
            # Alternate AB/BA to avoid crediting the second binary for warm input.
            order = ["before", "after"] if repeat % 2 == 0 else ["after", "before"]
            for variant in order:
                label = f"{case['name']}-{repeat}-{variant}"
                result = run(binaries[variant], case, label, root, args, env)
                if variant == "before" and expected_files is None:
                    expected_files = result["sha256"]
                results["runs"].append(dict(variant=variant, repeat=repeat, **result))
                if repeat >= 0:
                    results["timings"][variant].append(result["elapsed_seconds"])
                print(f"{label}: {result['elapsed_seconds']:.3f}s", flush=True)
                if not args.keep_worlds and repeat != 0:
                    shutil.rmtree(result["world"])
            # Also check baseline repeatability, independently of execution order.
            for result in results["runs"][-2:]:
                if result["sha256"] != expected_files:
                    report["cases"].append(results)
                    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
                    raise RuntimeError(f"region bytes differ in {case['name']}; see {report_path}")
        results["before_median_seconds"] = statistics.median(results["timings"]["before"])
        results["after_median_seconds"] = statistics.median(results["timings"]["after"])
        results["speedup"] = results["before_median_seconds"] / results["after_median_seconds"]
        results["exact_region_file_match"] = True
        results["target_met"] = results["speedup"] >= args.target
        report["cases"].append(results)
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"{case['name']}: {results['speedup']:.2f}x, exact region file match", flush=True)
    print(report_path)
    return 0 if all(case["target_met"] for case in report["cases"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
