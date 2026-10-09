use super::*;

fn intel_hybrid(cpu_budget: usize) -> CpuTopology {
    let mut records = String::new();
    for logical in 0..16 {
        records.push_str(&format!("0,{logical},{},1\n", logical / 2));
    }
    for logical in 16..24 {
        records.push_str(&format!("0,{logical},{},0\n", logical - 8));
    }
    parse_windows_topology(&records, cpu_budget).unwrap()
}

#[test]
fn intel_hybrid_keeps_physical_cores_smt_and_performance_classes_separate() {
    let topology = intel_hybrid(24);
    assert_eq!(topology.physical_cores, Some(16));
    assert_eq!(topology.logical_processors, 24);
    assert_eq!(topology.levels[0].physical_cores, 8);
    assert_eq!(topology.levels[0].logical_processors, 16);
    assert_eq!(topology.levels[1].physical_cores, 8);
    assert_eq!(topology.preferred_threads(), 8);
    assert_eq!(topology.thread_candidates(24), vec![8, 16, 24]);
    assert_eq!(topology.thread_candidates(6), vec![6]);
    assert_eq!(intel_hybrid(4).thread_candidates(24), vec![4]);
}

#[test]
fn apple_three_levels_use_os_order_and_do_not_require_p_e_names() {
    let topology = parse_macos_topology(
        "hw.nperflevels: 3\nhw.physicalcpu: 12\nhw.logicalcpu: 12\n\
         hw.perflevel0.name: Highest\nhw.perflevel0.physicalcpu: 2\nhw.perflevel0.logicalcpu: 2\n\
         hw.perflevel1.name: Performance\nhw.perflevel1.physicalcpu: 6\nhw.perflevel1.logicalcpu: 6\n\
         hw.perflevel2.name: Efficiency\nhw.perflevel2.physicalcpu: 4\nhw.perflevel2.logicalcpu: 4\n",
        12,
    ).unwrap();
    assert_eq!(topology.levels.len(), 3);
    assert_eq!(topology.levels[0].name, "Highest");
    assert_eq!(topology.preferred_threads(), 2);
    assert_eq!(topology.thread_candidates(12), vec![2, 8, 12]);
    let constrained = parse_macos_topology("hw.physicalcpu: 8\nhw.logicalcpu: 16", 3).unwrap();
    assert_eq!(constrained.thread_candidates(16), vec![3]);
}

#[test]
fn mac_intel_and_missing_or_inconsistent_levels_keep_known_physical_counts() {
    for extra in [
        "",
        "\nhw.nperflevels: 2\nhw.perflevel0.physicalcpu: 6\nhw.perflevel0.logicalcpu: 12",
        "\nhw.nperflevels: 1\nhw.perflevel0.physicalcpu: 7\nhw.perflevel0.logicalcpu: 12",
    ] {
        let topology =
            parse_macos_topology(&format!("hw.physicalcpu: 6\nhw.logicalcpu: 12{extra}"), 12)
                .unwrap();
        assert!(topology.levels.is_empty());
        assert_eq!(topology.thread_candidates(12), vec![6, 12]);
    }
    assert!(parse_macos_topology("hw.physicalcpu: 12\nhw.logicalcpu: 6", 12).is_none());
}

#[test]
fn processor_groups_do_not_merge_cores_with_equal_core_indices() {
    let topology = parse_windows_topology("0,0,0,0\n0,1,0,0\n1,0,0,0\n1,1,0,0", 4).unwrap();
    assert_eq!(topology.physical_cores, Some(2));
    assert_eq!(topology.thread_candidates(4), vec![2, 4]);
}

#[test]
fn incomplete_class_information_does_not_invent_efficiency_cores() {
    let topology = from_processors(
        &[
            Processor {
                group: 0,
                logical: 0,
                core: 0,
                rank: Some(1024),
            },
            Processor {
                group: 0,
                logical: 1,
                core: 1,
                rank: None,
            },
        ],
        2,
        "fixture",
        |rank| rank.to_string(),
    )
    .unwrap();
    assert!(topology.levels.is_empty());
    assert_eq!(topology.physical_cores, Some(2));
}

#[test]
fn duplicate_or_inconsistent_windows_records_are_rejected() {
    for output in ["0,0,0,1\n0,0,0,1", "0,0,0,1\n0,1,0,0", "0,1,2", "bad", ""] {
        assert!(parse_windows_topology(output, 4).is_none(), "{output}");
    }
}

#[test]
fn linux_masks_exclude_offline_and_disallowed_cpus_before_counting_classes() {
    let root = tempfile::tempdir().unwrap();
    let cpu_root = root.path().join("cpu");
    let pmu_root = root.path().join("pmu");
    std::fs::create_dir_all(&cpu_root).unwrap();
    for cpu in 0..6 {
        let topology = cpu_root.join(format!("cpu{cpu}/topology"));
        std::fs::create_dir_all(&topology).unwrap();
        let siblings = match cpu {
            0 | 1 => "0-1",
            2 | 3 => "2-3",
            4 => "4",
            _ => "5",
        };
        std::fs::write(topology.join("thread_siblings_list"), siblings).unwrap();
    }
    std::fs::write(cpu_root.join("online"), "0-4").unwrap();
    for (pmu, cpus) in [("cpu_core", "0-3"), ("cpu_atom", "4-5")] {
        std::fs::create_dir_all(pmu_root.join(pmu)).unwrap();
        std::fs::write(pmu_root.join(pmu).join("cpus"), cpus).unwrap();
    }
    let mixed = linux_topology(&cpu_root, &pmu_root, "Cpus_allowed_list:\t1-5", 4).unwrap();
    assert_eq!(mixed.logical_processors, 4);
    assert_eq!(mixed.physical_cores, Some(3));
    assert_eq!(mixed.levels[0].physical_cores, 2);
    assert_eq!(mixed.levels[0].logical_processors, 3);
    assert_eq!(mixed.thread_candidates(4), vec![2, 3, 4]);
    let efficiency_only =
        linux_topology(&cpu_root, &pmu_root, "Cpus_allowed_list:\t4-5", 4).unwrap();
    assert_eq!(efficiency_only.logical_processors, 1);
    assert_eq!(efficiency_only.levels.len(), 1);
    assert_eq!(efficiency_only.levels[0].name, "efficiency");
    assert_eq!(efficiency_only.thread_candidates(4), vec![1]);
    assert!(linux_topology(&cpu_root, &pmu_root, "no affinity data", 4).is_none());
}

#[test]
fn arm_capacity_supports_more_than_two_performance_levels() {
    let topology = from_processors(
        &[
            Processor {
                group: 0,
                logical: 0,
                core: 0,
                rank: Some(1024),
            },
            Processor {
                group: 0,
                logical: 1,
                core: 1,
                rank: Some(768),
            },
            Processor {
                group: 0,
                logical: 2,
                core: 2,
                rank: Some(512),
            },
            Processor {
                group: 0,
                logical: 3,
                core: 3,
                rank: Some(512),
            },
        ],
        4,
        "fixture",
        |rank| rank.to_string(),
    )
    .unwrap();
    assert_eq!(topology.levels.len(), 3);
    assert_eq!(topology.thread_candidates(4), vec![1, 2, 4]);
}

#[test]
fn cpu_list_ranges_are_bounded_and_validated() {
    assert_eq!(
        parse_cpu_list(" 0-2,5,8-9\n").unwrap(),
        BTreeSet::from([0, 1, 2, 5, 8, 9])
    );
    for text in ["", "3-1", "0-999999999", "-1", "0,", "x"] {
        assert!(parse_cpu_list(text).is_none());
    }
}

#[test]
fn fallback_never_exceeds_the_reported_cpu_budget() {
    let topology = CpuTopology::fallback(4);
    assert_eq!(topology.preferred_threads(), 4);
    assert_eq!(topology.thread_candidates(128), vec![4]);
    assert_eq!(CpuTopology::fallback(0).thread_candidates(0), vec![1]);
}

#[test]
fn process_budget_can_be_reduced_but_never_expanded_by_environment() {
    assert_eq!(limited_cpu_budget(4, Some("2")), 2);
    assert_eq!(limited_cpu_budget(4, Some("100")), 4);
    for setting in [None, Some("0"), Some("wrong"), Some("-1")] {
        assert_eq!(limited_cpu_budget(4, setting), 4);
    }
}

#[test]
fn current_topology_and_diagnostic_are_coherent() {
    let topology = current();
    assert!(topology.cpu_budget > 0);
    assert!(topology.cpu_budget <= topology.logical_processors);
    assert!(topology
        .physical_cores
        .is_none_or(|count| count <= topology.logical_processors));
    let diagnostic = topology.to_json();
    assert_eq!(diagnostic["placement"], "os-scheduler");
    assert_eq!(diagnostic["cpuBudget"], topology.cpu_budget);
}

#[test]
fn command_probe_can_read_short_metadata_without_a_shell() {
    #[cfg(unix)]
    let output = command_stdout(std::process::Command::new("printf").arg("hw.physicalcpu: 4"));
    #[cfg(windows)]
    let output = command_stdout(std::process::Command::new("powershell.exe").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "'hw.physicalcpu: 4'",
    ]));
    #[cfg(any(unix, windows))]
    assert_eq!(output.unwrap().trim(), "hw.physicalcpu: 4");
}
