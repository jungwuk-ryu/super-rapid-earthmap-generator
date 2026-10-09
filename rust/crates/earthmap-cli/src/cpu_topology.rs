//! CPU topology supplies thread-count hypotheses; real generation selects the winner.
//! Counts do not bind Rayon threads to particular processors. Placement belongs to the OS.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde_json::{json, Value};

pub(crate) const CPU_BUDGET_ENV: &str = "EARTHMAP_CPU_BUDGET";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PerformanceLevel {
    pub name: String,
    /// Larger values identify faster cores, not a throughput multiplier.
    pub rank: Option<u32>,
    pub physical_cores: usize,
    pub logical_processors: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CpuTopology {
    pub source: &'static str,
    /// Process-visible processors, before the CPU time quota is applied.
    pub logical_processors: usize,
    pub physical_cores: Option<usize>,
    pub cpu_budget: usize,
    /// Ordered from fastest to slowest; an unknown topology has no levels.
    pub levels: Vec<PerformanceLevel>,
}

impl CpuTopology {
    fn fallback(cpu_budget: usize) -> Self {
        Self {
            source: "available-parallelism",
            logical_processors: cpu_budget.max(1),
            physical_cores: None,
            cpu_budget: cpu_budget.max(1),
            levels: Vec::new(),
        }
    }

    pub(crate) fn preferred_threads(&self) -> usize {
        // Avoid counting an SMT sibling as an additional full-speed core. On a
        // heterogeneous CPU, start with the fastest level's physical-core count.
        let preferred = if self.levels.len() > 1 {
            self.levels[0].physical_cores
        } else {
            self.physical_cores.unwrap_or(self.cpu_budget)
        };
        preferred.clamp(1, self.cpu_budget.max(1))
    }

    pub(crate) fn thread_candidates(&self, ceiling: usize) -> Vec<usize> {
        let ceiling = ceiling.max(1).min(self.cpu_budget.max(1));
        let mut candidates = BTreeSet::from([self.preferred_threads().min(ceiling), ceiling]);
        if let Some(physical) = self.physical_cores {
            candidates.insert(physical.clamp(1, ceiling));
        }
        let mut physical = 0usize;
        let mut logical = 0usize;
        for level in &self.levels {
            physical = physical.saturating_add(level.physical_cores);
            logical = logical.saturating_add(level.logical_processors);
            candidates.insert(physical.clamp(1, ceiling));
            candidates.insert(logical.clamp(1, ceiling));
        }
        candidates.into_iter().collect()
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "logicalProcessors": self.logical_processors,
            "physicalCores": self.physical_cores,
            "cpuBudget": self.cpu_budget,
            "heterogeneous": self.levels.len() > 1,
            "preferredComputeThreads": self.preferred_threads(),
            "computeThreadCandidates": self.thread_candidates(self.cpu_budget),
            "placement": "os-scheduler",
            "performanceLevels": self.levels.iter().map(|level| json!({
                "name": level.name,
                "rank": level.rank,
                "physicalCores": level.physical_cores,
                "logicalProcessors": level.logical_processors,
            })).collect::<Vec<_>>(),
        })
    }
}

pub(crate) fn current() -> &'static CpuTopology {
    static TOPOLOGY: OnceLock<CpuTopology> = OnceLock::new();
    TOPOLOGY.get_or_init(|| {
        let budget = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        let budget = limited_cpu_budget(budget, std::env::var(CPU_BUDGET_ENV).ok().as_deref());
        detect(budget).unwrap_or_else(|| CpuTopology::fallback(budget))
    })
}

fn limited_cpu_budget(available: usize, setting: Option<&str>) -> usize {
    let requested = setting
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(available.max(1));
    requested.min(available.max(1))
}

#[cfg(any(target_os = "linux", windows, test))]
#[derive(Clone, Copy, Debug)]
struct Processor {
    group: u32,
    logical: u32,
    core: u32,
    rank: Option<u32>,
}

#[cfg(any(target_os = "linux", windows, test))]
fn from_processors(
    processors: &[Processor],
    cpu_budget: usize,
    source: &'static str,
    level_name: impl Fn(u32) -> String,
) -> Option<CpuTopology> {
    if processors.is_empty() {
        return None;
    }
    let mut logical_ids = BTreeSet::new();
    let mut cores = BTreeMap::<(u32, u32), Option<u32>>::new();
    for processor in processors {
        if !logical_ids.insert((processor.group, processor.logical)) {
            return None;
        }
        if let Some(previous) = cores.insert((processor.group, processor.core), processor.rank) {
            if previous != processor.rank {
                return None;
            }
        }
    }
    let mut levels = Vec::new();
    // Partial class information must not silently turn unclassified CPUs into E cores.
    if processors.iter().all(|processor| processor.rank.is_some()) {
        let mut classes = BTreeMap::<u32, (BTreeSet<(u32, u32)>, usize)>::new();
        for processor in processors {
            let entry = classes.entry(processor.rank?).or_default();
            entry.0.insert((processor.group, processor.core));
            entry.1 += 1;
        }
        for (rank, (cores, logical)) in classes.into_iter().rev() {
            levels.push(PerformanceLevel {
                name: level_name(rank),
                rank: Some(rank),
                physical_cores: cores.len(),
                logical_processors: logical,
            });
        }
    }
    Some(CpuTopology {
        source,
        logical_processors: processors.len(),
        physical_cores: Some(cores.len()),
        cpu_budget: cpu_budget.max(1).min(processors.len()),
        levels,
    })
}

#[cfg(target_os = "linux")]
fn detect(cpu_budget: usize) -> Option<CpuTopology> {
    linux_topology(
        std::path::Path::new("/sys/devices/system/cpu"),
        std::path::Path::new("/sys/bus/event_source/devices"),
        &std::fs::read_to_string("/proc/self/status").ok()?,
        cpu_budget,
    )
}

#[cfg(any(target_os = "linux", test))]
fn linux_topology(
    cpu_root: &std::path::Path,
    pmu_root: &std::path::Path,
    process_status: &str,
    cpu_budget: usize,
) -> Option<CpuTopology> {
    use std::fs;

    let allowed = parse_cpu_list(
        process_status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:").map(str::trim))?,
    )?;
    let online = parse_cpu_list(&fs::read_to_string(cpu_root.join("online")).ok()?)?;
    let visible = allowed
        .intersection(&online)
        .copied()
        .collect::<BTreeSet<_>>();
    if visible.is_empty() {
        return None;
    }
    let performance = fs::read_to_string(pmu_root.join("cpu_core/cpus"))
        .ok()
        .and_then(|value| parse_cpu_list(&value));
    let efficiency = fs::read_to_string(pmu_root.join("cpu_atom/cpus"))
        .ok()
        .and_then(|value| parse_cpu_list(&value));
    let intel_hybrid = performance.as_ref().zip(efficiency.as_ref());
    let mut processors = Vec::with_capacity(visible.len());
    for cpu in &visible {
        let root = cpu_root.join(format!("cpu{cpu}"));
        let siblings =
            parse_cpu_list(&fs::read_to_string(root.join("topology/thread_siblings_list")).ok()?)?;
        if !siblings.contains(cpu) {
            return None;
        }
        let core = siblings.intersection(&visible).next().copied()?;
        let rank = if let Some((performance, efficiency)) = intel_hybrid {
            match (performance.contains(cpu), efficiency.contains(cpu)) {
                (true, false) => Some(2),
                (false, true) => Some(1),
                _ => None,
            }
        } else {
            fs::read_to_string(root.join("cpu_capacity"))
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .filter(|value| *value > 0)
        };
        processors.push(Processor {
            group: 0,
            logical: *cpu,
            core,
            rank,
        });
    }
    from_processors(&processors, cpu_budget, "linux-sysfs", |rank| {
        if intel_hybrid.is_some() {
            if rank == 2 {
                "performance"
            } else {
                "efficiency"
            }
            .to_string()
        } else {
            format!("capacity-{rank}")
        }
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_list(value: &str) -> Option<BTreeSet<u32>> {
    let mut cpus = BTreeSet::new();
    for range in value.trim().split(',') {
        let (start, end) = match range.split_once('-') {
            Some((start, end)) => (start.trim().parse::<u32>().ok()?, end.trim().parse().ok()?),
            None => {
                let cpu = range.trim().parse::<u32>().ok()?;
                (cpu, cpu)
            }
        };
        if start > end || end >= 65_536 {
            return None;
        }
        cpus.extend(start..=end);
    }
    (!cpus.is_empty()).then_some(cpus)
}

#[cfg(target_os = "macos")]
fn detect(cpu_budget: usize) -> Option<CpuTopology> {
    use std::process::Command;

    let base = command_stdout(Command::new("/usr/sbin/sysctl").args([
        "hw.nperflevels",
        "hw.physicalcpu",
        "hw.logicalcpu",
    ]))?;
    let values = parse_sysctl(&base);
    let count = values
        .get("hw.nperflevels")
        .and_then(|value| value.parse::<usize>().ok());
    let mut output = base;
    if let Some(count @ 1..=32) = count {
        let mut command = Command::new("/usr/sbin/sysctl");
        for level in 0..count {
            for field in ["name", "physicalcpu", "logicalcpu"] {
                command.arg(format!("hw.perflevel{level}.{field}"));
            }
        }
        output.push('\n');
        if let Some(levels) = command_stdout(&mut command) {
            output.push_str(&levels);
        }
    }
    parse_macos_topology(&output, cpu_budget)
}

#[cfg(any(target_os = "macos", test))]
fn parse_sysctl(output: &str) -> BTreeMap<&str, &str> {
    output
        .lines()
        .filter_map(|line| {
            line.split_once(':')
                .map(|(key, value)| (key.trim(), value.trim()))
        })
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_topology(output: &str, cpu_budget: usize) -> Option<CpuTopology> {
    let values = parse_sysctl(output);
    let number = |key: &str| {
        values
            .get(key)?
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
    };
    let logical = number("hw.logicalcpu")?;
    let physical = number("hw.physicalcpu")?;
    if physical > logical {
        return None;
    }
    let count = number("hw.nperflevels").filter(|count| *count <= 32);
    let mut levels = Vec::new();
    if let Some(count) = count {
        for index in 0..count {
            let physical = number(&format!("hw.perflevel{index}.physicalcpu"));
            let logical = number(&format!("hw.perflevel{index}.logicalcpu"));
            let (Some(physical), Some(logical)) = (physical, logical) else {
                levels.clear();
                break;
            };
            if physical > logical {
                levels.clear();
                break;
            }
            levels.push(PerformanceLevel {
                name: values
                    .get(format!("hw.perflevel{index}.name").as_str())
                    .map(|name| (*name).to_string())
                    .unwrap_or_else(|| format!("perflevel-{index}")),
                rank: Some((count - index) as u32),
                physical_cores: physical,
                logical_processors: logical,
            });
        }
        if levels
            .iter()
            .map(|level| level.physical_cores)
            .sum::<usize>()
            != physical
            || levels
                .iter()
                .map(|level| level.logical_processors)
                .sum::<usize>()
                != logical
        {
            levels.clear();
        }
    }
    Some(CpuTopology {
        source: "macos-sysctl",
        logical_processors: logical,
        physical_cores: Some(physical),
        cpu_budget: cpu_budget.max(1).min(logical),
        levels,
    })
}

#[cfg(windows)]
fn detect(cpu_budget: usize) -> Option<CpuTopology> {
    use std::os::windows::process::CommandExt;

    let script = format!(
        "$EarthmapCpuProcessId = {};\n{}",
        std::process::id(),
        include_str!("cpu_topology/windows_cpu_sets.ps1"),
    );
    let output = command_stdout(
        std::process::Command::new("powershell.exe")
            .creation_flags(0x0800_0000)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ]),
    )?;
    parse_windows_topology(&output, cpu_budget)
}

#[cfg(any(windows, test))]
fn parse_windows_topology(output: &str, cpu_budget: usize) -> Option<CpuTopology> {
    let mut processors = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line
            .trim()
            .split(',')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let [group, logical, core, rank] = fields.as_slice() else {
            return None;
        };
        processors.push(Processor {
            group: *group,
            logical: *logical,
            core: *core,
            rank: Some(*rank),
        });
    }
    from_processors(&processors, cpu_budget, "windows-cpu-sets", |rank| {
        format!("efficiency-class-{rank}")
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn detect(_cpu_budget: usize) -> Option<CpuTopology> {
    None
}

#[cfg(any(target_os = "macos", windows, test))]
fn command_stdout(command: &mut std::process::Command) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = String::new();
        let result = stdout
            .take(256 * 1024)
            .read_to_string(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // sysctl exits nonzero if an optional key is absent. Still accept
                // its readable keys; the parsers validate the result independently.
                let output = receiver
                    .recv_timeout(Duration::from_millis(100))
                    .ok()?
                    .ok()?;
                return (status.success() || !output.is_empty()).then_some(output);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests;
