#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Implemented,
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
        status: CommandStatus::NotPortedYet,
        note: "production alias for vanilla-delegated parallel generation",
    },
    CommandSpec {
        name: "generate-height-region",
        status: CommandStatus::Implemented,
        note: "height-only region generation",
    },
    CommandSpec {
        name: "generate-surface-region",
        status: CommandStatus::NotPortedYet,
        note: "surface region generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-region",
        status: CommandStatus::NotPortedYet,
        note: "single vanilla-delegated region generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-regions-parallel",
        status: CommandStatus::NotPortedYet,
        note: "parallel vanilla-delegated region generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-plan-parallel",
        status: CommandStatus::NotPortedYet,
        note: "bounded/resumable vanilla-delegated plan generation",
    },
    CommandSpec {
        name: "generate-vanilla-delegated-region-plan-parallel",
        status: CommandStatus::NotPortedYet,
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
        status: CommandStatus::NotPortedYet,
        note: "quality gate sample batch",
    },
    CommandSpec {
        name: "mca-topdown-render",
        status: CommandStatus::NotPortedYet,
        note: "MCA top-down render",
    },
    CommandSpec {
        name: "validate-mca-region",
        status: CommandStatus::NotPortedYet,
        note: "MCA region validator",
    },
    CommandSpec {
        name: "validate-linear-region",
        status: CommandStatus::NotPortedYet,
        note: "Linear V2 region validator",
    },
    CommandSpec {
        name: "compare-mca-linear-region-payloads",
        status: CommandStatus::NotPortedYet,
        note: "MCA/Linear payload parity comparator",
    },
    CommandSpec {
        name: "inspect-mca-palettes",
        status: CommandStatus::NotPortedYet,
        note: "MCA block palette scanner",
    },
    CommandSpec {
        name: "inspect-linear-palettes",
        status: CommandStatus::NotPortedYet,
        note: "Linear block palette scanner",
    },
    CommandSpec {
        name: "inspect-mca-statuses",
        status: CommandStatus::NotPortedYet,
        note: "MCA chunk status scanner",
    },
    CommandSpec {
        name: "inspect-linear-statuses",
        status: CommandStatus::NotPortedYet,
        note: "Linear chunk status scanner",
    },
];

pub const SHELL_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "doctor",
        status: CommandStatus::ImplementedShellOnly,
        note: "print Rust-port runtime assumptions",
    },
    CommandSpec {
        name: "capabilities",
        status: CommandStatus::ImplementedShellOnly,
        note: "print Rust-port capability status",
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
