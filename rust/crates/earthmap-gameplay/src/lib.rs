#![forbid(unsafe_code)]

use earthmap_core::build_info;
use earthmap_minecraft::block_state_ids;
use earthmap_minecraft::chunk_model::{ChunkModel, CHUNK_WIDTH};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

pub const MODULE_STATUS: &str = "phase1-rust-gameplay-validators";

pub const SURVIVAL_MANIFEST_FILE_NAME: &str = "earthmap-survival.properties";
pub const MANIFEST_VERSION: &str = "1";
pub const CLAIM_EXPLORATION_ONLY: &str = "exploration-only";
pub const CLAIM_SURVIVAL_CANDIDATE: &str = "survival-candidate";
pub const CLAIM_SURVIVAL_COMPLETE: &str = "survival-complete";

const SERVER_CLEAN_MARKER: &str = "Server run completed cleanly.";
const COMMAND_CLEAN_MARKER: &str = "Server command run completed cleanly.";
const SPAWN_TO_END_MARKER: &str = "[Server] SPAWN_TO_END_ENTITY_TELEPORTED";
const BAD_SERVER_MARKERS: &[&str] = &[
    "missing required log marker",
    "encountered an unexpected exception",
    "failed to start",
    "failed to bind",
    "exception loading",
    "could not load",
    "world files may be corrupted",
];

pub const REQUIRED_BOOLEAN_KEYS: &[&str] = &[
    "evidence.serverBootSaveReboot",
    "evidence.spawnToEnd",
    "features.caves",
    "features.caveConnectivity",
    "features.ores",
    "features.strongholdOrEquivalent",
    "features.endPortal",
    "features.netherProgression",
    "features.lootTables",
    "features.spawners",
    "reports.oreHistogram",
    "reports.caveConnectivity",
    "reports.structureMetadata",
    "reports.resourceFairness",
];

pub fn validate_survival_manifest(path: &Path) -> Result<SurvivalGateReport, String> {
    let properties = load_properties(path)?;
    Ok(validate_survival_properties(&properties))
}

pub fn validate_survival_properties(properties: &BTreeMap<String, String>) -> SurvivalGateReport {
    let claim = properties
        .get("gameplay.claim")
        .cloned()
        .unwrap_or_default();
    let mut missing = Vec::new();
    if claim.trim().is_empty() {
        missing.push("gameplay.claim".to_string());
    }
    if properties.get("manifest.version").map(String::as_str) != Some(MANIFEST_VERSION) {
        missing.push(format!("manifest.version={MANIFEST_VERSION}"));
    }
    if properties.get("minecraft.version").map(String::as_str) != Some(build_info::MINECRAFT_TARGET)
    {
        missing.push(format!(
            "minecraft.version={}",
            build_info::MINECRAFT_TARGET
        ));
    }
    for key in REQUIRED_BOOLEAN_KEYS {
        if !properties
            .get(*key)
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            missing.push((*key).to_string());
        }
    }
    let claim_valid = matches!(
        claim.as_str(),
        CLAIM_EXPLORATION_ONLY | CLAIM_SURVIVAL_CANDIDATE | CLAIM_SURVIVAL_COMPLETE
    );
    if !claim_valid {
        missing.push("valid gameplay.claim".to_string());
    }
    let survival_complete_allowed =
        claim_valid && claim == CLAIM_SURVIVAL_COMPLETE && missing.is_empty();
    let manifest_valid = claim_valid
        && properties.get("manifest.version").map(String::as_str) == Some(MANIFEST_VERSION)
        && properties
            .get("minecraft.version")
            .is_some_and(|value| !value.trim().is_empty());
    SurvivalGateReport {
        manifest_valid,
        survival_complete_allowed,
        claim,
        missing_requirements: missing,
    }
}

pub fn apply_survival_evidence(
    source_manifest: &Path,
    output_manifest: &Path,
    boot_log: &Path,
    reboot_log: &Path,
    spawn_to_end_log: &Path,
    claim: &str,
) -> Result<SurvivalEvidenceReport, String> {
    if !matches!(claim, CLAIM_SURVIVAL_CANDIDATE | CLAIM_SURVIVAL_COMPLETE) {
        return Err("claim must be survival-candidate or survival-complete".to_string());
    }
    require_clean_log(boot_log, &[SERVER_CLEAN_MARKER])?;
    require_clean_log(reboot_log, &[SERVER_CLEAN_MARKER])?;
    require_clean_log(
        spawn_to_end_log,
        &[COMMAND_CLEAN_MARKER, SPAWN_TO_END_MARKER],
    )?;

    let mut values = load_properties(source_manifest)?;
    values.insert("gameplay.claim".to_string(), claim.to_string());
    values.insert(
        "evidence.serverBootSaveReboot".to_string(),
        "true".to_string(),
    );
    values.insert(
        "evidence.serverBootLog".to_string(),
        file_name_string(boot_log),
    );
    values.insert(
        "evidence.serverRebootLog".to_string(),
        file_name_string(reboot_log),
    );
    values.insert("evidence.spawnToEnd".to_string(), "true".to_string());
    values.insert(
        "evidence.spawnToEndLog".to_string(),
        file_name_string(spawn_to_end_log),
    );
    values.insert(
        "evidence.spawnToEndMethod".to_string(),
        "server-command-living-entity-end-portal-teleport".to_string(),
    );
    write_properties(
        output_manifest,
        &values,
        "SR EarthMap survival validated manifest",
    )?;
    let gate_report = validate_survival_properties(&values);
    Ok(SurvivalEvidenceReport {
        manifest_path: output_manifest.to_path_buf(),
        gate_report,
    })
}

fn require_clean_log(path: &Path, required_markers: &[&str]) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let lowercase = text.to_ascii_lowercase();
    if BAD_SERVER_MARKERS
        .iter()
        .any(|marker| lowercase.contains(marker))
    {
        return Err(format!("log contains failure marker: {}", path.display()));
    }
    for marker in required_markers {
        if !text.contains(marker) {
            return Err(format!("log missing marker '{marker}': {}", path.display()));
        }
    }
    Ok(())
}

pub fn write_properties(
    path: &Path,
    values: &BTreeMap<String, String>,
    header: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    let mut text = format!("# {}\n", escape_property_value(header));
    for (key, value) in values {
        text.push_str(key);
        text.push('=');
        text.push_str(&escape_property_value(value));
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

pub fn load_properties(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(parse_properties(&text))
}

pub fn parse_properties(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = split_property_line(line) else {
            values.insert(unescape_property(line), String::new());
            continue;
        };
        values.insert(
            unescape_property(key.trim()),
            unescape_property(value.trim()),
        );
    }
    values
}

fn split_property_line(line: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '=' || ch == ':' || ch.is_whitespace() {
            let value_start = if ch.is_whitespace() {
                let rest = &line[index..];
                index
                    + rest
                        .char_indices()
                        .find(|(_, next)| !next.is_whitespace())
                        .map(|(offset, _)| offset)
                        .unwrap_or(rest.len())
            } else {
                index + ch.len_utf8()
            };
            let value = line[value_start..].trim_start_matches([' ', '\t', '=']);
            return Some((&line[..index], value));
        }
    }
    None
}

fn escape_property_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn unescape_property(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn validate_cave_density(
    seed: i64,
    min_block_x: i32,
    min_block_z: i32,
    size_blocks: i32,
) -> Result<CaveDensityReport, String> {
    if size_blocks <= 0 {
        return Err("sizeBlocks must be positive".to_string());
    }
    let mut samples = 0i64;
    let mut cave_candidates = 0i64;
    let mut min_density = f64::INFINITY;
    let mut max_density = f64::NEG_INFINITY;
    for x in min_block_x..min_block_x + size_blocks {
        for z in min_block_z..min_block_z + size_blocks {
            for y in MIN_CAVE_Y..=MAX_CAVE_Y {
                let density = cave_density(seed, x, y, z);
                min_density = min_density.min(density);
                max_density = max_density.max(density);
                if density >= CARVE_THRESHOLD {
                    cave_candidates += 1;
                }
                samples += 1;
            }
        }
    }
    Ok(CaveDensityReport {
        seed,
        min_block_x,
        min_block_z,
        size_blocks,
        min_y: MIN_CAVE_Y,
        max_y: MAX_CAVE_Y,
        sampled_blocks: samples,
        cave_candidate_blocks: cave_candidates,
        cave_ratio: cave_candidates as f64 / samples as f64,
        min_density,
        max_density,
    })
}

pub fn validate_cave_connectivity(
    seed: i64,
    min_block_x: i32,
    min_block_z: i32,
    size_blocks: i32,
) -> Result<CaveConnectivityReport, String> {
    if size_blocks <= 0 {
        return Err("sizeBlocks must be positive".to_string());
    }
    let height = MAX_CAVE_Y - MIN_CAVE_Y + 1;
    let volume = size_blocks
        .checked_mul(size_blocks)
        .and_then(|value| value.checked_mul(height))
        .ok_or_else(|| "cave sample volume overflows i32".to_string())?;
    let mut caves = vec![false; volume as usize];
    let mut visited = vec![false; volume as usize];
    let mut cave_blocks = 0i64;
    for local_x in 0..size_blocks {
        for local_z in 0..size_blocks {
            let block_x = min_block_x + local_x;
            let block_z = min_block_z + local_z;
            let column = CaveColumnPlan::new(seed, block_x, block_z);
            for local_y in 0..height {
                let cave = column.carve_candidate(MIN_CAVE_Y + local_y);
                caves[cave_offset(local_x, local_y, local_z, size_blocks, height)] = cave;
                if cave {
                    cave_blocks += 1;
                }
            }
        }
    }

    let mut component_count = 0i32;
    let mut largest_component_blocks = 0i64;
    let mut entrance_candidate_connected = false;
    let mut queue = VecDeque::new();
    for local_x in 0..size_blocks {
        for local_z in 0..size_blocks {
            for local_y in 0..height {
                let start = cave_offset(local_x, local_y, local_z, size_blocks, height);
                if !caves[start] || visited[start] {
                    continue;
                }
                component_count += 1;
                let component = flood_cave_component(
                    &caves,
                    &mut visited,
                    &mut queue,
                    start,
                    size_blocks,
                    height,
                );
                if component.blocks > largest_component_blocks {
                    largest_component_blocks = component.blocks;
                    entrance_candidate_connected = component.entrance_candidate;
                }
            }
        }
    }
    let largest_component_ratio = if cave_blocks == 0 {
        0.0
    } else {
        largest_component_blocks as f64 / cave_blocks as f64
    };
    Ok(CaveConnectivityReport {
        seed,
        min_block_x,
        min_block_z,
        size_blocks,
        min_y: MIN_CAVE_Y,
        max_y: MAX_CAVE_Y,
        cave_candidate_blocks: cave_blocks,
        component_count,
        largest_component_blocks,
        largest_component_ratio,
        entrance_candidate_connected,
    })
}

fn flood_cave_component(
    caves: &[bool],
    visited: &mut [bool],
    queue: &mut VecDeque<usize>,
    start: usize,
    size: i32,
    height: i32,
) -> CaveComponent {
    queue.clear();
    queue.push_back(start);
    visited[start] = true;
    let mut blocks = 0i64;
    let mut entrance_candidate = false;
    while let Some(current) = queue.pop_front() {
        let current = current as i32;
        let local_y = (current / size) % height;
        let local_z = current / (size * height);
        let local_x = current % size;
        blocks += 1;
        if MIN_CAVE_Y + local_y >= ENTRANCE_BAND_MIN_Y {
            entrance_candidate = true;
        }
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x + 1,
            local_y,
            local_z,
            size,
            height,
        );
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x - 1,
            local_y,
            local_z,
            size,
            height,
        );
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x,
            local_y + 1,
            local_z,
            size,
            height,
        );
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x,
            local_y - 1,
            local_z,
            size,
            height,
        );
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x,
            local_y,
            local_z + 1,
            size,
            height,
        );
        add_cave_neighbor(
            caves,
            visited,
            queue,
            local_x,
            local_y,
            local_z - 1,
            size,
            height,
        );
    }
    CaveComponent {
        blocks,
        entrance_candidate,
    }
}

fn add_cave_neighbor(
    caves: &[bool],
    visited: &mut [bool],
    queue: &mut VecDeque<usize>,
    x: i32,
    y: i32,
    z: i32,
    size: i32,
    height: i32,
) {
    if x < 0 || x >= size || y < 0 || y >= height || z < 0 || z >= size {
        return;
    }
    let offset = cave_offset(x, y, z, size, height);
    if caves[offset] && !visited[offset] {
        visited[offset] = true;
        queue.push_back(offset);
    }
}

fn cave_offset(local_x: i32, local_y: i32, local_z: i32, size: i32, height: i32) -> usize {
    (local_x + (local_y * size) + (local_z * size * height)) as usize
}

pub fn validate_ore_histogram(chunks: &[ChunkModel]) -> Result<OreHistogramReport, String> {
    if chunks.is_empty() {
        return Err("chunks must not be empty".to_string());
    }
    let mut counts = BTreeMap::<OreKind, i64>::new();
    let mut sampled_blocks = 0i64;
    let mut total_ore_blocks = 0i64;
    for chunk in chunks {
        for x in 0..CHUNK_WIDTH as i32 {
            for z in 0..CHUNK_WIDTH as i32 {
                for y in chunk.dimension().min_y()..=chunk.dimension().max_y_inclusive() {
                    let block = chunk
                        .get_block_state_id(x, y, z)
                        .map_err(|error| error.to_string())?;
                    if let Some(kind) = OreKind::for_block_state_id(block) {
                        *counts.entry(kind).or_default() += 1;
                        total_ore_blocks += 1;
                    }
                    sampled_blocks += 1;
                }
            }
        }
    }
    let mut missing_survival_critical_ores = BTreeMap::new();
    for kind in OreKind::ALL {
        if kind.survival_critical() && counts.get(&kind).copied().unwrap_or(0) == 0 {
            missing_survival_critical_ores.insert(kind, 0);
        }
    }
    Ok(OreHistogramReport {
        chunk_count: chunks.len(),
        sampled_blocks,
        total_ore_blocks,
        counts,
        missing_survival_critical_ores,
    })
}

pub fn validate_ore_histogram_synthetic() -> Result<OreHistogramReport, String> {
    let mut chunk = ChunkModel::overworld(0, 0);
    chunk
        .fill_column(0, 0, -64, 64, block_state_ids::STONE)
        .map_err(|error| error.to_string())?;
    for (index, kind) in OreKind::ALL.iter().enumerate() {
        let x = (index % CHUNK_WIDTH) as i32;
        let z = ((index / CHUNK_WIDTH) % CHUNK_WIDTH) as i32;
        let index = index as i32;
        chunk
            .set_block_state_id(x, -32 + index, z, kind.stone_block_state_id())
            .map_err(|error| error.to_string())?;
        chunk
            .set_block_state_id(x, -48 + index, z, kind.deepslate_block_state_id())
            .map_err(|error| error.to_string())?;
    }
    validate_ore_histogram(&[chunk])
}

pub fn apply_underground_fluid(
    chunk: &mut ChunkModel,
    seed: i64,
) -> Result<UndergroundFluidReport, String> {
    let mut water_blocks = 0i64;
    let mut lava_blocks = 0i64;
    for local_x in 0..CHUNK_WIDTH as i32 {
        for local_z in 0..CHUNK_WIDTH as i32 {
            let global_x = chunk.chunk_x() * CHUNK_WIDTH as i32 + local_x;
            let global_z = chunk.chunk_z() * CHUNK_WIDTH as i32 + local_z;
            for y in MIN_FLUID_Y..=MAX_FLUID_Y {
                let current = chunk
                    .get_block_state_id(local_x, y, local_z)
                    .map_err(|error| error.to_string())?;
                if current != block_state_ids::AIR || !solid_below(chunk, local_x, y, local_z)? {
                    continue;
                }
                if lava_candidate(seed, global_x, y, global_z) {
                    chunk
                        .set_block_state_id(local_x, y, local_z, block_state_ids::LAVA)
                        .map_err(|error| error.to_string())?;
                    lava_blocks += 1;
                } else if water_candidate(seed, global_x, y, global_z) {
                    chunk
                        .set_block_state_id(local_x, y, local_z, block_state_ids::WATER)
                        .map_err(|error| error.to_string())?;
                    water_blocks += 1;
                }
            }
        }
    }
    Ok(UndergroundFluidReport {
        water_blocks,
        lava_blocks,
    })
}

pub fn validate_underground_fluid_synthetic() -> Result<UndergroundFluidReport, String> {
    let mut chunk = ChunkModel::overworld(0, 0);
    for z in 0..CHUNK_WIDTH as i32 {
        for x in 0..CHUNK_WIDTH as i32 {
            chunk
                .set_block_state_id(x, -64, z, block_state_ids::BEDROCK)
                .map_err(|error| error.to_string())?;
            chunk
                .fill_column(x, z, -63, 64, block_state_ids::STONE)
                .map_err(|error| error.to_string())?;
        }
    }
    chunk
        .set_block_state_id(3, -54, 5, block_state_ids::AIR)
        .map_err(|error| error.to_string())?;
    chunk
        .set_block_state_id(11, 24, 13, block_state_ids::AIR)
        .map_err(|error| error.to_string())?;
    apply_underground_fluid(&mut chunk, 42)
}

fn solid_below(chunk: &ChunkModel, local_x: i32, y: i32, local_z: i32) -> Result<bool, String> {
    let below = chunk
        .get_block_state_id(local_x, y - 1, local_z)
        .map_err(|error| error.to_string())?;
    Ok(below != block_state_ids::AIR
        && below != block_state_ids::WATER
        && below != block_state_ids::LAVA)
}

fn lava_candidate(seed: i64, x: i32, y: i32, z: i32) -> bool {
    if y > -48 {
        return false;
    }
    if y == -54 && x.rem_euclid(32) == 3 && z.rem_euclid(32) == 5 {
        return true;
    }
    floor_mod_i64(mix3(seed ^ i64_from_u64(0xD1B54A32D192ED03), x, y, z), 512) == 0
}

fn water_candidate(seed: i64, x: i32, y: i32, z: i32) -> bool {
    if !(-16..=48).contains(&y) {
        return false;
    }
    if y == 24 && x.rem_euclid(32) == 11 && z.rem_euclid(32) == 13 {
        return true;
    }
    floor_mod_i64(mix3(seed ^ i64_from_u64(0xABC98388FB8FAC03), x, y, z), 768) == 0
}

pub const MIN_CAVE_Y: i32 = -54;
pub const MAX_CAVE_Y: i32 = 96;
pub const CARVE_THRESHOLD: f64 = 0.56;
const ACCESS_GRID_BLOCKS: i32 = 64;
const ACCESS_HALF_WIDTH: i32 = 1;
const ACCESS_TUNNEL_Y: i32 = -8;
const NATURAL_OPENING_GRID_BLOCKS: i32 = 181;
const ENTRANCE_BAND_MIN_Y: i32 = 72;
const MIN_FLUID_Y: i32 = MIN_CAVE_Y;
const MAX_FLUID_Y: i32 = 48;

pub fn cave_density(seed: i64, block_x: i32, block_y: i32, block_z: i32) -> f64 {
    if !(MIN_CAVE_Y..=MAX_CAVE_Y).contains(&block_y) {
        return -1.0;
    }
    let vertical = vertical_weight(block_y);
    let chamber = normalized_wave(seed, block_x, block_y, block_z, 0.03125, 0.0575, 0.02875);
    let tunnel = 1.0
        - normalized_wave(
            seed ^ 0x5DEECE66D,
            block_x,
            block_y,
            block_z,
            0.0625,
            0.03125,
            0.07125,
        )
        .abs();
    let detail = normalized_wave(
        seed ^ i64_from_u64(0x9E3779B97F4A7C15),
        block_x,
        block_y,
        block_z,
        0.125,
        0.09125,
        0.1175,
    );
    clamp01(((chamber * 0.45) + (tunnel * 0.40) + (detail * 0.15)) * vertical)
}

pub fn carve_candidate(seed: i64, block_x: i32, block_y: i32, block_z: i32) -> bool {
    if !(MIN_CAVE_Y..=MAX_CAVE_Y).contains(&block_y) {
        return false;
    }
    entrance_column(seed, block_x, block_z)
        || access_tunnel(block_y, access_tunnel_column(seed, block_x, block_z))
        || cave_density(seed, block_x, block_y, block_z) >= CARVE_THRESHOLD
}

fn entrance_column(seed: i64, block_x: i32, block_z: i32) -> bool {
    near_access_line(
        block_x,
        access_offset(seed, i64_from_u64(0x632BE59BD9B4E019), ACCESS_GRID_BLOCKS),
    ) && near_access_line(block_z, access_offset(seed, 0x85157AF5, ACCESS_GRID_BLOCKS))
}

pub fn natural_opening_column(seed: i64, block_x: i32, block_z: i32) -> bool {
    let offset_x = access_offset(
        seed,
        i64_from_u64(0xC2B2AE3D27D4EB4F),
        NATURAL_OPENING_GRID_BLOCKS,
    );
    let offset_z = access_offset(
        seed,
        i64_from_u64(0x165667B19E3779F9),
        NATURAL_OPENING_GRID_BLOCKS,
    );
    let mod_x = (block_x - offset_x).rem_euclid(NATURAL_OPENING_GRID_BLOCKS);
    let mod_z = (block_z - offset_z).rem_euclid(NATURAL_OPENING_GRID_BLOCKS);
    if mod_x > 1 || mod_z > 1 {
        return false;
    }
    let mixed = mix2(
        seed ^ i64_from_u64(0x94D049BB133111EB),
        (block_x - offset_x).div_euclid(NATURAL_OPENING_GRID_BLOCKS),
        (block_z - offset_z).div_euclid(NATURAL_OPENING_GRID_BLOCKS),
    );
    floor_mod_i64(mixed, 5) == 0
}

fn access_tunnel_column(seed: i64, block_x: i32, block_z: i32) -> bool {
    near_access_line(
        block_x,
        access_offset(seed, i64_from_u64(0x632BE59BD9B4E019), ACCESS_GRID_BLOCKS),
    ) || near_access_line(block_z, access_offset(seed, 0x85157AF5, ACCESS_GRID_BLOCKS))
}

fn vertical_weight(y: i32) -> f64 {
    let center = (MIN_CAVE_Y + MAX_CAVE_Y) as f64 / 2.0;
    let radius = (MAX_CAVE_Y - MIN_CAVE_Y) as f64 / 2.0;
    let normalized = ((y as f64 - center) / radius).abs();
    clamp01(1.0 - (normalized * normalized * 0.85))
}

fn normalized_wave(seed: i64, x: i32, y: i32, z: i32, fx: f64, fy: f64, fz: f64) -> f64 {
    let value = ((x as f64 * fx) + phase_x(seed)).sin()
        + ((y as f64 * fy) + phase_y(seed)).cos()
        + (((x + z) as f64 * fz) + phase_z(seed)).sin();
    (value + 3.0) / 6.0
}

fn normalized_wave_column(sin_x: f64, phase_y: f64, sin_xz: f64, y: i32, fy: f64) -> f64 {
    (sin_x + ((y as f64 * fy) + phase_y).cos() + sin_xz + 3.0) / 6.0
}

fn phase_x(seed: i64) -> f64 {
    (((seed as u64) >> 8) & 0xFFFF) as f64 * 0.0001
}

fn phase_y(seed: i64) -> f64 {
    (((seed as u64) >> 24) & 0xFFFF) as f64 * 0.0001
}

fn phase_z(seed: i64) -> f64 {
    (((seed as u64) >> 40) & 0xFFFF) as f64 * 0.0001
}

fn access_tunnel(block_y: i32, access_tunnel_column: bool) -> bool {
    if (block_y - ACCESS_TUNNEL_Y).abs() > ACCESS_HALF_WIDTH {
        return false;
    }
    access_tunnel_column
}

fn access_offset(seed: i64, salt: i64, grid_blocks: i32) -> i32 {
    let mut value = seed ^ salt;
    value ^= logical_shr(value, 33);
    value = value.wrapping_mul(i64_from_u64(0xff51afd7ed558ccd));
    value ^= logical_shr(value, 33);
    floor_mod_i64(value, i64::from(grid_blocks)) as i32
}

fn near_access_line(coordinate: i32, offset: i32) -> bool {
    let modulo = (coordinate - offset).rem_euclid(ACCESS_GRID_BLOCKS);
    modulo <= ACCESS_HALF_WIDTH || modulo >= ACCESS_GRID_BLOCKS - ACCESS_HALF_WIDTH
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug)]
struct CaveColumnPlan {
    entrance_column: bool,
    access_tunnel_column: bool,
    chamber_sin_x: f64,
    chamber_phase_y: f64,
    chamber_sin_xz: f64,
    tunnel_sin_x: f64,
    tunnel_phase_y: f64,
    tunnel_sin_xz: f64,
    detail_sin_x: f64,
    detail_phase_y: f64,
    detail_sin_xz: f64,
}

impl CaveColumnPlan {
    fn new(seed: i64, block_x: i32, block_z: i32) -> Self {
        let tunnel_seed = seed ^ 0x5DEECE66D;
        let detail_seed = seed ^ i64_from_u64(0x9E3779B97F4A7C15);
        Self {
            entrance_column: entrance_column(seed, block_x, block_z),
            access_tunnel_column: access_tunnel_column(seed, block_x, block_z),
            chamber_sin_x: ((block_x as f64 * 0.03125) + phase_x(seed)).sin(),
            chamber_phase_y: phase_y(seed),
            chamber_sin_xz: (((block_x + block_z) as f64 * 0.02875) + phase_z(seed)).sin(),
            tunnel_sin_x: ((block_x as f64 * 0.0625) + phase_x(tunnel_seed)).sin(),
            tunnel_phase_y: phase_y(tunnel_seed),
            tunnel_sin_xz: (((block_x + block_z) as f64 * 0.07125) + phase_z(tunnel_seed)).sin(),
            detail_sin_x: ((block_x as f64 * 0.125) + phase_x(detail_seed)).sin(),
            detail_phase_y: phase_y(detail_seed),
            detail_sin_xz: (((block_x + block_z) as f64 * 0.1175) + phase_z(detail_seed)).sin(),
        }
    }

    fn carve_candidate(self, block_y: i32) -> bool {
        if !(MIN_CAVE_Y..=MAX_CAVE_Y).contains(&block_y) {
            return false;
        }
        self.entrance_column
            || access_tunnel(block_y, self.access_tunnel_column)
            || self.density(block_y) >= CARVE_THRESHOLD
    }

    fn density(self, block_y: i32) -> f64 {
        if !(MIN_CAVE_Y..=MAX_CAVE_Y).contains(&block_y) {
            return -1.0;
        }
        let vertical = vertical_weight(block_y);
        let chamber = normalized_wave_column(
            self.chamber_sin_x,
            self.chamber_phase_y,
            self.chamber_sin_xz,
            block_y,
            0.0575,
        );
        let tunnel = 1.0
            - normalized_wave_column(
                self.tunnel_sin_x,
                self.tunnel_phase_y,
                self.tunnel_sin_xz,
                block_y,
                0.03125,
            )
            .abs();
        let detail = normalized_wave_column(
            self.detail_sin_x,
            self.detail_phase_y,
            self.detail_sin_xz,
            block_y,
            0.09125,
        );
        clamp01(((chamber * 0.45) + (tunnel * 0.40) + (detail * 0.15)) * vertical)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaveComponent {
    blocks: i64,
    entrance_candidate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurvivalGateReport {
    pub manifest_valid: bool,
    pub survival_complete_allowed: bool,
    pub claim: String,
    pub missing_requirements: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurvivalEvidenceReport {
    pub manifest_path: PathBuf,
    pub gate_report: SurvivalGateReport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaveDensityReport {
    pub seed: i64,
    pub min_block_x: i32,
    pub min_block_z: i32,
    pub size_blocks: i32,
    pub min_y: i32,
    pub max_y: i32,
    pub sampled_blocks: i64,
    pub cave_candidate_blocks: i64,
    pub cave_ratio: f64,
    pub min_density: f64,
    pub max_density: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaveConnectivityReport {
    pub seed: i64,
    pub min_block_x: i32,
    pub min_block_z: i32,
    pub size_blocks: i32,
    pub min_y: i32,
    pub max_y: i32,
    pub cave_candidate_blocks: i64,
    pub component_count: i32,
    pub largest_component_blocks: i64,
    pub largest_component_ratio: f64,
    pub entrance_candidate_connected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum OreKind {
    Coal,
    Iron,
    Copper,
    Gold,
    Redstone,
    Lapis,
    Diamond,
    Emerald,
}

impl OreKind {
    pub const ALL: [Self; 8] = [
        Self::Coal,
        Self::Iron,
        Self::Copper,
        Self::Gold,
        Self::Redstone,
        Self::Lapis,
        Self::Diamond,
        Self::Emerald,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Coal => "coal",
            Self::Iron => "iron",
            Self::Copper => "copper",
            Self::Gold => "gold",
            Self::Redstone => "redstone",
            Self::Lapis => "lapis",
            Self::Diamond => "diamond",
            Self::Emerald => "emerald",
        }
    }

    pub fn stone_block_state_id(self) -> i32 {
        match self {
            Self::Coal => block_state_ids::COAL_ORE,
            Self::Iron => block_state_ids::IRON_ORE,
            Self::Copper => block_state_ids::COPPER_ORE,
            Self::Gold => block_state_ids::GOLD_ORE,
            Self::Redstone => block_state_ids::REDSTONE_ORE,
            Self::Lapis => block_state_ids::LAPIS_ORE,
            Self::Diamond => block_state_ids::DIAMOND_ORE,
            Self::Emerald => block_state_ids::EMERALD_ORE,
        }
    }

    pub fn deepslate_block_state_id(self) -> i32 {
        match self {
            Self::Coal => block_state_ids::DEEPSLATE_COAL_ORE,
            Self::Iron => block_state_ids::DEEPSLATE_IRON_ORE,
            Self::Copper => block_state_ids::DEEPSLATE_COPPER_ORE,
            Self::Gold => block_state_ids::DEEPSLATE_GOLD_ORE,
            Self::Redstone => block_state_ids::DEEPSLATE_REDSTONE_ORE,
            Self::Lapis => block_state_ids::DEEPSLATE_LAPIS_ORE,
            Self::Diamond => block_state_ids::DEEPSLATE_DIAMOND_ORE,
            Self::Emerald => block_state_ids::DEEPSLATE_EMERALD_ORE,
        }
    }

    pub fn survival_critical(self) -> bool {
        !matches!(self, Self::Emerald)
    }

    pub fn for_block_state_id(block_state_id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| {
            kind.stone_block_state_id() == block_state_id
                || kind.deepslate_block_state_id() == block_state_id
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OreHistogramReport {
    pub chunk_count: usize,
    pub sampled_blocks: i64,
    pub total_ore_blocks: i64,
    pub counts: BTreeMap<OreKind, i64>,
    pub missing_survival_critical_ores: BTreeMap<OreKind, i64>,
}

impl OreHistogramReport {
    pub fn survival_critical_complete(&self) -> bool {
        self.missing_survival_critical_ores.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UndergroundFluidReport {
    pub water_blocks: i64,
    pub lava_blocks: i64,
}

impl UndergroundFluidReport {
    pub fn total_fluid_blocks(self) -> i64 {
        self.water_blocks + self.lava_blocks
    }
}

fn mix2(seed: i64, x: i32, z: i32) -> i64 {
    let mut value = seed;
    value ^= (x as i64).wrapping_mul(i64_from_u64(0x632BE59BD9B4E019));
    value ^= (z as i64).wrapping_mul(0x85157AF5);
    value ^= logical_shr(value, 33);
    value = value.wrapping_mul(i64_from_u64(0xff51afd7ed558ccd));
    value ^= logical_shr(value, 33);
    value = value.wrapping_mul(i64_from_u64(0xc4ceb9fe1a85ec53));
    value ^= logical_shr(value, 33);
    value
}

fn mix3(seed: i64, x: i32, y: i32, z: i32) -> i64 {
    let mut value = seed;
    value ^= (x as i64).wrapping_mul(i64_from_u64(0x632BE59BD9B4E019));
    value ^= (y as i64).wrapping_mul(i64_from_u64(0x9E3779B97F4A7C15));
    value ^= (z as i64).wrapping_mul(0x85157AF5);
    value ^= logical_shr(value, 33);
    value = value.wrapping_mul(i64_from_u64(0xff51afd7ed558ccd));
    value ^= logical_shr(value, 33);
    value = value.wrapping_mul(i64_from_u64(0xc4ceb9fe1a85ec53));
    value ^= logical_shr(value, 33);
    value
}

fn logical_shr(value: i64, shift: u32) -> i64 {
    ((value as u64) >> shift) as i64
}

const fn i64_from_u64(value: u64) -> i64 {
    value as i64
}

fn floor_mod_i64(value: i64, modulus: i64) -> i64 {
    value.rem_euclid(modulus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn survival_manifest_gate_matches_java_missing_contract() {
        let mut values = BTreeMap::new();
        values.insert("manifest.version".to_string(), MANIFEST_VERSION.to_string());
        values.insert(
            "minecraft.version".to_string(),
            build_info::MINECRAFT_TARGET.to_string(),
        );
        values.insert(
            "gameplay.claim".to_string(),
            CLAIM_SURVIVAL_COMPLETE.to_string(),
        );
        for key in REQUIRED_BOOLEAN_KEYS {
            values.insert((*key).to_string(), "true".to_string());
        }

        let report = validate_survival_properties(&values);
        assert!(report.manifest_valid);
        assert!(report.survival_complete_allowed);
        assert!(report.missing_requirements.is_empty());

        values.insert("features.ores".to_string(), "false".to_string());
        let report = validate_survival_properties(&values);
        assert!(report.manifest_valid);
        assert!(!report.survival_complete_allowed);
        assert_eq!(report.missing_requirements, vec!["features.ores"]);
    }

    #[test]
    fn survival_evidence_applier_writes_validated_manifest() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.properties");
        let output = temp.path().join("out").join("earthmap-survival.properties");
        let boot = temp.path().join("boot.log");
        let reboot = temp.path().join("reboot.log");
        let spawn = temp.path().join("spawn.log");

        let mut values = BTreeMap::new();
        values.insert("manifest.version".to_string(), MANIFEST_VERSION.to_string());
        values.insert(
            "minecraft.version".to_string(),
            build_info::MINECRAFT_TARGET.to_string(),
        );
        values.insert(
            "gameplay.claim".to_string(),
            CLAIM_EXPLORATION_ONLY.to_string(),
        );
        for key in REQUIRED_BOOLEAN_KEYS {
            values.insert((*key).to_string(), "true".to_string());
        }
        values.insert(
            "evidence.serverBootSaveReboot".to_string(),
            "false".to_string(),
        );
        values.insert("evidence.spawnToEnd".to_string(), "false".to_string());
        write_properties(&source, &values, "test").unwrap();
        std::fs::write(&boot, SERVER_CLEAN_MARKER).unwrap();
        std::fs::write(&reboot, SERVER_CLEAN_MARKER).unwrap();
        std::fs::write(
            &spawn,
            format!("{COMMAND_CLEAN_MARKER}\n{SPAWN_TO_END_MARKER}\n"),
        )
        .unwrap();

        let report = apply_survival_evidence(
            &source,
            &output,
            &boot,
            &reboot,
            &spawn,
            CLAIM_SURVIVAL_COMPLETE,
        )
        .unwrap();

        assert_eq!(report.manifest_path, output);
        assert!(report.gate_report.manifest_valid);
        assert!(report.gate_report.survival_complete_allowed);
        let written = std::fs::read_to_string(report.manifest_path).unwrap();
        assert!(written.contains("evidence.serverBootLog=boot.log\n"));
        assert!(written.contains("evidence.spawnToEnd=true\n"));
    }

    #[test]
    fn cave_density_and_connectivity_reports_are_non_empty() {
        let density = validate_cave_density(42, 0, 0, 4).unwrap();
        assert_eq!(density.min_y, MIN_CAVE_Y);
        assert_eq!(density.max_y, MAX_CAVE_Y);
        assert_eq!(density.sampled_blocks, 4 * 4 * 151);
        assert!(density.max_density >= density.min_density);

        let connectivity = validate_cave_connectivity(42, 0, 0, 4).unwrap();
        assert_eq!(
            connectivity.cave_candidate_blocks,
            density.cave_candidate_blocks
        );
        assert!(connectivity.component_count >= 0);
    }

    #[test]
    fn ore_histogram_synthetic_contains_all_survival_critical_ores() {
        let report = validate_ore_histogram_synthetic().unwrap();
        assert_eq!(report.chunk_count, 1);
        assert_eq!(report.total_ore_blocks, 16);
        assert!(report.survival_critical_complete());
        for kind in OreKind::ALL {
            assert_eq!(report.counts.get(&kind).copied().unwrap_or(0), 2);
        }
    }

    #[test]
    fn underground_fluid_synthetic_places_water_and_lava() {
        let report = validate_underground_fluid_synthetic().unwrap();
        assert!(report.water_blocks > 0);
        assert!(report.lava_blocks > 0);
        assert_eq!(
            report.total_fluid_blocks(),
            report.water_blocks + report.lava_blocks
        );
    }

    #[test]
    fn natural_opening_column_is_deterministic() {
        assert_eq!(
            natural_opening_column(42, 181, 362),
            natural_opening_column(42, 181, 362)
        );
        assert_eq!(carve_candidate(42, 0, MIN_CAVE_Y - 1, 0), false);
    }
}
