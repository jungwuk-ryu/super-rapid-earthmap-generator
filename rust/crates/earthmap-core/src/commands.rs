#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Implemented,
    ImplementedProbeOnly,
    ImplementedShellOnly,
    NotPortedYet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub status: CommandStatus,
    pub note: &'static str,
}

pub const INITIAL_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "generate",
        status: CommandStatus::Implemented,
        note: "production alias for vanilla-delegated parallel generation",
    },
    CommandSpec {
        name: "generate-height-region",
        status: CommandStatus::Implemented,
        note: "height-only region generation",
    },
    CommandSpec {
        name: "generate-surface-region",
        status: CommandStatus::Implemented,
        note: "surface region generation bootstrap",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-region",
        status: CommandStatus::ImplementedProbeOnly,
        note: "single vanilla-delegated region parity probe; payload parity is not green",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-regions-parallel",
        status: CommandStatus::Implemented,
        note: "parallel vanilla-delegated region generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-plan-parallel",
        status: CommandStatus::Implemented,
        note: "bounded/resumable vanilla-delegated plan generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-region-plan-parallel",
        status: CommandStatus::Implemented,
        note: "Java-compatible alias for vanilla-delegated plan generation",
    },
    CommandSpec {
        name: "generate-survival-region",
        status: CommandStatus::NotPortedYet,
        note: "single vanilla-delegated survival compatibility workflow",
    },
    CommandSpec {
        name: "generate-survival-regions-parallel",
        status: CommandStatus::NotPortedYet,
        note: "parallel vanilla-delegated survival compatibility workflow",
    },
    CommandSpec {
        name: "quality-production-sample-batch",
        status: CommandStatus::Implemented,
        note: "Rust quality gate production sample batch",
    },
    CommandSpec {
        name: "photo-parity-crop",
        status: CommandStatus::Implemented,
        note: "PNG crop parity artifact generator",
    },
    CommandSpec {
        name: "photo-compare-crop",
        status: CommandStatus::Implemented,
        note: "PNG crop metric comparator",
    },
    CommandSpec {
        name: "photo-parity-metric-crop",
        status: CommandStatus::Implemented,
        note: "PNG crop photo metric reporter",
    },
    CommandSpec {
        name: "photo-parity-metric-batch",
        status: CommandStatus::Implemented,
        note: "parallel PNG crop photo metric batch",
    },
    CommandSpec {
        name: "photo-standard-remap-parity-crop",
        status: CommandStatus::Implemented,
        note: "Rust Standard palette remap parity crop",
    },
    CommandSpec {
        name: "photo-standard-remap-parity-batch",
        status: CommandStatus::Implemented,
        note: "parallel Rust Standard palette remap parity batch",
    },
    CommandSpec {
        name: "photo-production-candidate-diff-crop",
        status: CommandStatus::Implemented,
        note: "PNG production candidate diff reporter",
    },
    CommandSpec {
        name: "photo-carrier-remap-sim-crop",
        status: CommandStatus::Implemented,
        note: "PNG carrier remap simulation reporter",
    },
    CommandSpec {
        name: "dynmap-tile-mosaic",
        status: CommandStatus::Implemented,
        note: "Dynmap tile mosaic builder",
    },
    CommandSpec {
        name: "mca-topdown-render",
        status: CommandStatus::Implemented,
        note: "MCA top-down render",
    },
    CommandSpec {
        name: "linear-topdown-render",
        status: CommandStatus::Implemented,
        note: "Linear V2 top-down render",
    },
    CommandSpec {
        name: "validate-mca-region",
        status: CommandStatus::Implemented,
        note: "MCA region validator",
    },
    CommandSpec {
        name: "validate-linear-region",
        status: CommandStatus::Implemented,
        note: "Linear V2 region validator",
    },
    CommandSpec {
        name: "compare-mca-linear-region-payloads",
        status: CommandStatus::Implemented,
        note: "MCA/Linear payload comparator",
    },
    CommandSpec {
        name: "convert-mca-region-to-linear",
        status: CommandStatus::Implemented,
        note: "single MCA region to Linear V2 converter",
    },
    CommandSpec {
        name: "convert-mca-world-to-linear",
        status: CommandStatus::Implemented,
        note: "MCA world to Linear V2 converter",
    },
    CommandSpec {
        name: "inspect-mca-palettes",
        status: CommandStatus::Implemented,
        note: "MCA block palette scanner",
    },
    CommandSpec {
        name: "validate-mca-survival-palette",
        status: CommandStatus::Implemented,
        note: "MCA survival palette validator",
    },
    CommandSpec {
        name: "inspect-linear-palettes",
        status: CommandStatus::Implemented,
        note: "Linear block palette scanner",
    },
    CommandSpec {
        name: "validate-linear-survival-palette",
        status: CommandStatus::Implemented,
        note: "Linear survival palette validator",
    },
    CommandSpec {
        name: "inspect-mca-biomes",
        status: CommandStatus::Implemented,
        note: "MCA biome palette scanner",
    },
    CommandSpec {
        name: "inspect-linear-biomes",
        status: CommandStatus::Implemented,
        note: "Linear biome palette scanner",
    },
    CommandSpec {
        name: "inspect-mca-statuses",
        status: CommandStatus::Implemented,
        note: "MCA chunk status scanner",
    },
    CommandSpec {
        name: "inspect-linear-statuses",
        status: CommandStatus::Implemented,
        note: "Linear chunk status scanner",
    },
    CommandSpec {
        name: "inspect-mca-post-final-integrity",
        status: CommandStatus::Implemented,
        note: "MCA post-final integrity scanner",
    },
    CommandSpec {
        name: "rewrite-mca-status",
        status: CommandStatus::Implemented,
        note: "MCA chunk status rewriter",
    },
    CommandSpec {
        name: "repair-mca-post-final-water",
        status: CommandStatus::Implemented,
        note: "MCA post-final underwater air repairer",
    },
    CommandSpec {
        name: "inspect-linear-post-final-integrity",
        status: CommandStatus::Implemented,
        note: "Linear post-final integrity scanner",
    },
    CommandSpec {
        name: "repair-linear-sandlike-surfaces",
        status: CommandStatus::Implemented,
        note: "Linear sandlike surface repairer",
    },
];

pub const SHELL_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "doctor",
        status: CommandStatus::ImplementedShellOnly,
        note: "print Rust runtime assumptions",
    },
    CommandSpec {
        name: "capabilities",
        status: CommandStatus::ImplementedShellOnly,
        note: "print Rust runtime capability status",
    },
    CommandSpec {
        name: "inspect-heightmap",
        status: CommandStatus::ImplementedShellOnly,
        note: "inspect Java-compatible GeoTIFF heightmap metadata",
    },
    CommandSpec {
        name: "locate-heightmap-point",
        status: CommandStatus::ImplementedShellOnly,
        note: "locate longitude/latitude in the Java-compatible Earth grid",
    },
    CommandSpec {
        name: "classify-surface-point",
        status: CommandStatus::ImplementedShellOnly,
        note: "classify a heightmap-backed Java-compatible surface point",
    },
    CommandSpec {
        name: "sample-vrt-rgb",
        status: CommandStatus::ImplementedShellOnly,
        note: "sample a Java-compatible VRT RGB mosaic",
    },
];

pub fn find_initial_command(name: &str) -> Option<CommandSpec> {
    INITIAL_COMMANDS
        .iter()
        .copied()
        .find(|command| command.name == name)
}

pub fn all_command_names() -> impl Iterator<Item = &'static str> {
    SHELL_COMMANDS
        .iter()
        .chain(INITIAL_COMMANDS.iter())
        .map(|command| command.name)
}
