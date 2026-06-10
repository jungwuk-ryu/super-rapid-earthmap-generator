#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use earthmap_geo::{EarthScaleMapping, GeoTiffHeightmapReader};
use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const HEIGHTMAP_PATH_ENV: &str = "EARTHMAP_HEIGHTMAP";
const DATA_ROOT_ENV: &str = "EARTHMAP_DATA_ROOT";
const TIF_ROOT_ENV: &str = "EARTHMAP_TIF_ROOT";
const SURFACE_RASTER_ENV: &str = "EARTHMAP_SURFACE_RASTER";
const OUTPUT_ROOT_ENV: &str = "EARTHMAP_OUTPUT_ROOT";
const DEFAULT_WORLD_DIR_NAME: &str = "earthmap-gui-world";
const SURVIVAL_MANIFEST_FILE_NAME: &str = "earthmap-survival.properties";
const VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME: &str = "earthmap-vanilla-delegated-resume.ndjson";
const DEFAULT_CACHE_ROWS: &str = "auto";
const DEFAULT_SURFACE_TILE_CACHE_ENTRIES: &str = "auto";
const DEFAULT_VERTICAL_SCALE: &str = "auto";
const REGION_SIZE_BLOCKS: i32 = 512;
const FULL_EARTH_MIN_LATITUDE: f64 = -90.0;
const FULL_EARTH_MAX_LATITUDE: f64 = 90.0;
const WORKER_EVENT_CHANNEL_CAPACITY: usize = 4096;
const REGION_STATE_MISSING: u8 = 0;
const REGION_STATE_QUEUED: u8 = 1;
const REGION_STATE_RUNNING: u8 = 2;
const REGION_STATE_SKIPPED: u8 = 3;
const REGION_STATE_GENERATED: u8 = 4;
const REGION_STATE_FAILED: u8 = 5;
const ROLLING_SPEED_WINDOW: Duration = Duration::from_secs(60);
const WORLD_MAP_BACKGROUND_PNG: &[u8] =
    include_bytes!("../assets/world-background-truemarble-2048.png");
const MAX_STATUS_TEXTURE_DIMENSION: usize = 2048;
const STATUS_TEXTURE_UPLOAD_INTERVAL: Duration = Duration::from_millis(250);
const GUI_OPTIONS_STORAGE_KEY: &str = "earthmap-gui.generation-options.v1";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--cli" || arg == "cli")
    {
        std::process::exit(earthmap_cli::run(args.into_iter().skip(1)));
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        print_help();
        return;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--gui" || arg == "gui")
        && args.len() > 1
    {
        eprintln!("earthmap-gui: --gui does not accept extra arguments");
        std::process::exit(2);
    }
    if let Err(error) = run_gui() {
        eprintln!("earthmap-gui: {error}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!("Usage:");
    println!("  earthmap-gui");
    println!("  earthmap-gui --gui");
    println!("  earthmap-gui --cli <earthmap-rs command args...>");
}

fn run_gui() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "EarthMap Generator",
        options,
        Box::new(|creation_context| Ok(Box::new(EarthMapGuiApp::new(creation_context)))),
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum OutputFormatChoice {
    Linear,
    Mca,
}

impl OutputFormatChoice {
    fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Mca => "mca",
        }
    }

    fn region_extension(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Mca => "mca",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ChunkStatusChoice {
    Surface,
    Carvers,
}

impl ChunkStatusChoice {
    fn as_cli_arg(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Carvers => "carvers",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ExtentMode {
    WholeEarth,
    Preset,
    Bounds,
    RegionGrid,
}

impl ExtentMode {
    fn label(self) -> &'static str {
        match self {
            Self::WholeEarth => "Whole Earth",
            Self::Preset => "Preset area",
            Self::Bounds => "Latitude/longitude box",
            Self::RegionGrid => "Advanced region grid",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum AreaPreset {
    Korea,
    Australia,
    Europe,
    Japan,
    UnitedStates,
}

impl AreaPreset {
    fn label(self) -> &'static str {
        match self {
            Self::Korea => "Korean Peninsula + Jeju",
            Self::Australia => "Australia + Tasmania",
            Self::Europe => "Europe",
            Self::Japan => "Japan",
            Self::UnitedStates => "Contiguous United States",
        }
    }

    fn bounds(self) -> GeoBounds {
        match self {
            Self::Korea => GeoBounds::new(124.0, 132.0, 43.5, 33.0),
            Self::Australia => GeoBounds::new(112.0, 154.0, -9.0, -45.0),
            Self::Europe => GeoBounds::new(-11.0, 32.0, 72.0, 35.0),
            Self::Japan => GeoBounds::new(128.0, 146.0, 46.0, 30.0),
            Self::UnitedStates => GeoBounds::new(-125.0, -66.0, 49.5, 24.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
struct GeoBounds {
    west_longitude: f64,
    east_longitude: f64,
    north_latitude: f64,
    south_latitude: f64,
}

impl GeoBounds {
    fn new(
        west_longitude: f64,
        east_longitude: f64,
        north_latitude: f64,
        south_latitude: f64,
    ) -> Self {
        Self {
            west_longitude,
            east_longitude,
            north_latitude,
            south_latitude,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RegionGrid {
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GenerationOptions {
    heightmap_path: String,
    tif_root: String,
    true_marble_path: String,
    world_dir: String,
    scale: i32,
    extent_mode: ExtentMode,
    preset: AreaPreset,
    bounds: GeoBounds,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    format: OutputFormatChoice,
    linear_compression_level: i32,
    mca_compression_level: u32,
    threads: usize,
    #[serde(default = "default_shard_processes")]
    shard_processes: usize,
    status: ChunkStatusChoice,
    vertical_scale: String,
    cache_rows: String,
    surface_tile_cache_entries: String,
    rayon_threads: String,
    #[serde(default = "default_worker_tuning_enabled")]
    worker_tuning_enabled: bool,
    #[serde(default)]
    prefetch_enabled: bool,
    #[serde(default = "default_auto_text")]
    prefetch_memory_gb: String,
    #[serde(default = "default_auto_text")]
    prefetch_queue_regions: String,
    #[serde(default = "default_auto_text")]
    prefetch_workers: String,
}

fn default_worker_tuning_enabled() -> bool {
    true
}

fn default_shard_processes() -> usize {
    1
}

fn default_auto_text() -> String {
    "auto".to_string()
}

impl Default for GenerationOptions {
    fn default() -> Self {
        let heightmap_path = default_heightmap_path();
        let tif_root = default_tif_root();
        let true_marble_path = default_true_marble_path(&tif_root);
        let world_dir = default_world_dir();
        Self {
            heightmap_path,
            tif_root,
            true_marble_path,
            world_dir,
            scale: 1000,
            extent_mode: ExtentMode::Preset,
            preset: AreaPreset::Australia,
            bounds: AreaPreset::Australia.bounds(),
            start_region_x: 26,
            start_region_z: -10,
            cols: 3,
            rows: 3,
            format: OutputFormatChoice::Linear,
            linear_compression_level: 4,
            mca_compression_level: 6,
            threads: 8,
            shard_processes: default_shard_processes(),
            status: ChunkStatusChoice::Surface,
            vertical_scale: DEFAULT_VERTICAL_SCALE.to_string(),
            cache_rows: DEFAULT_CACHE_ROWS.to_string(),
            surface_tile_cache_entries: DEFAULT_SURFACE_TILE_CACHE_ENTRIES.to_string(),
            rayon_threads: String::new(),
            worker_tuning_enabled: default_worker_tuning_enabled(),
            prefetch_enabled: false,
            prefetch_memory_gb: default_auto_text(),
            prefetch_queue_regions: default_auto_text(),
            prefetch_workers: default_auto_text(),
        }
    }
}

impl GenerationOptions {
    fn region_count(&self) -> usize {
        let grid = self.resolved_region_grid();
        let cols = grid.cols.max(1) as usize;
        let rows = grid.rows.max(1) as usize;
        cols.saturating_mul(rows)
    }

    fn normalized_surface_raster(&self) -> String {
        let trimmed = self.true_marble_path.trim();
        if trimmed.is_empty() {
            "surfaceRaster=auto".to_string()
        } else if trimmed.contains('=') {
            trimmed.to_string()
        } else {
            format!("surfaceRaster={trimmed}")
        }
    }

    fn compression_option(&self) -> String {
        match self.format {
            OutputFormatChoice::Linear => {
                format!(
                    "linearCompression={}",
                    self.linear_compression_level.clamp(1, 22)
                )
            }
            OutputFormatChoice::Mca => {
                format!("mcaCompression={}", self.mca_compression_level.min(9))
            }
        }
    }

    fn vertical_scale_option(&self) -> String {
        let trimmed = self.vertical_scale.trim();
        if trimmed.is_empty() {
            "verticalScale=auto".to_string()
        } else {
            format!("verticalScale={trimmed}")
        }
    }

    fn prefetch_options(&self) -> Vec<String> {
        if !self.prefetch_enabled {
            return Vec::new();
        }
        let mut options = vec!["prefetch=true".to_string()];
        let memory = self.prefetch_memory_gb.trim();
        if !memory.is_empty() && !memory.eq_ignore_ascii_case("auto") {
            options.push(format!("prefetchMemoryGB={memory}"));
        }
        let queue_regions = self.prefetch_queue_regions.trim();
        if !queue_regions.is_empty() && !queue_regions.eq_ignore_ascii_case("auto") {
            options.push(format!("prefetchRegions={queue_regions}"));
        }
        let workers = self.prefetch_workers.trim();
        if !workers.is_empty() && !workers.eq_ignore_ascii_case("auto") {
            options.push(format!("prefetchWorkers={workers}"));
        }
        options
    }

    fn resolved_region_grid(&self) -> RegionGrid {
        match self.extent_mode {
            ExtentMode::WholeEarth => {
                full_earth_region_grid_for_heightmap(self.scale, &self.heightmap_path)
                    .unwrap_or_else(|| full_earth_region_grid(self.scale))
            }
            ExtentMode::Preset => region_grid_for_bounds(self.scale, self.preset.bounds())
                .unwrap_or_else(|| manual_region_grid(self)),
            ExtentMode::Bounds => region_grid_for_bounds(self.scale, self.bounds)
                .unwrap_or_else(|| manual_region_grid(self)),
            ExtentMode::RegionGrid => manual_region_grid(self),
        }
    }

    fn apply_environment_defaults(&mut self) {
        self.heightmap_path = default_heightmap_path();
        self.tif_root = default_tif_root();
        self.true_marble_path = default_true_marble_path(&self.tif_root);
        self.world_dir = default_world_dir();
    }

    fn apply_tif_root(&mut self) {
        self.true_marble_path = true_marble_from_tif_root(&self.tif_root);
    }

    fn validation_error(&self) -> Option<String> {
        if self.heightmap_path.trim().is_empty() {
            return Some(format!(
                "Select a HeightMap GeoTIFF or set {HEIGHTMAP_PATH_ENV}."
            ));
        }
        if self.world_dir.trim().is_empty() {
            return Some(format!(
                "Select an output world directory or set {OUTPUT_ROOT_ENV}."
            ));
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessTag {
    index: usize,
    total: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GenerationCommand {
    tag: Option<ProcessTag>,
    grid: RegionGrid,
    args: Vec<String>,
}

fn build_generation_args(options: &GenerationOptions) -> Vec<String> {
    build_generation_args_for_grid(options, options.resolved_region_grid())
}

fn build_generation_args_for_grid(options: &GenerationOptions, grid: RegionGrid) -> Vec<String> {
    let mut args = vec![
        "generate-vanilla-delegated-regions-parallel".to_string(),
        options.heightmap_path.clone(),
        options.world_dir.clone(),
        options.scale.max(1).to_string(),
        grid.start_region_x.to_string(),
        grid.start_region_z.to_string(),
        grid.cols.max(1).to_string(),
        grid.rows.max(1).to_string(),
        options.format.as_cli_arg().to_string(),
        options.threads.max(1).to_string(),
        options.status.as_cli_arg().to_string(),
        options.normalized_surface_raster(),
        options.vertical_scale_option(),
        options.compression_option(),
        format!("workerAutotune={}", options.worker_tuning_enabled),
    ];
    args.extend(options.prefetch_options());
    args
}

fn effective_shard_processes(options: &GenerationOptions) -> usize {
    let grid = options.resolved_region_grid();
    effective_shard_processes_for_grid(options.shard_processes, grid)
}

fn effective_shard_processes_for_grid(requested_shards: usize, grid: RegionGrid) -> usize {
    requested_shards.max(1).min(grid.cols.max(1) as usize)
}

fn build_shard_region_grids(grid: RegionGrid, requested_shards: usize) -> Vec<RegionGrid> {
    let shard_count = effective_shard_processes_for_grid(requested_shards, grid);
    if shard_count <= 1 {
        return vec![grid];
    }
    let total_cols = grid.cols.max(1) as usize;
    let base_cols = total_cols / shard_count;
    let extra_cols = total_cols % shard_count;
    let mut grids = Vec::with_capacity(shard_count);
    let mut start_region_x = grid.start_region_x;
    for shard_index in 0..shard_count {
        let cols = base_cols + usize::from(shard_index < extra_cols);
        if cols == 0 {
            continue;
        }
        grids.push(RegionGrid {
            start_region_x,
            start_region_z: grid.start_region_z,
            cols: i32::try_from(cols).unwrap_or(i32::MAX),
            rows: grid.rows.max(1),
        });
        start_region_x = start_region_x.saturating_add(i32::try_from(cols).unwrap_or(i32::MAX));
    }
    grids
}

fn build_generation_commands(options: &GenerationOptions) -> Vec<GenerationCommand> {
    let grid = options.resolved_region_grid();
    let shards = build_shard_region_grids(grid, options.shard_processes);
    let total = shards.len();
    shards
        .into_iter()
        .enumerate()
        .map(|(index, shard_grid)| GenerationCommand {
            tag: (total > 1).then_some(ProcessTag { index, total }),
            grid: shard_grid,
            args: {
                let mut args = build_generation_args_for_grid(options, shard_grid);
                if total > 1 {
                    args.push(format!(
                        "projectGrid={},{},{},{}",
                        grid.start_region_x, grid.start_region_z, grid.cols, grid.rows
                    ));
                }
                args
            },
        })
        .collect()
}

fn manual_region_grid(options: &GenerationOptions) -> RegionGrid {
    RegionGrid {
        start_region_x: options.start_region_x,
        start_region_z: options.start_region_z,
        cols: options.cols.max(1),
        rows: options.rows.max(1),
    }
}

fn full_earth_region_grid_for_heightmap(scale: i32, heightmap_path: &str) -> Option<RegionGrid> {
    let trimmed = heightmap_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let reader = GeoTiffHeightmapReader::open(Path::new(trimmed)).ok()?;
    let metadata = reader.metadata();
    let mapping = EarthScaleMapping::for_denominator(
        scale.max(1),
        metadata.top_left_latitude - (f64::from(metadata.height) * metadata.pixel_height_degrees),
        metadata.top_left_latitude,
    )
    .ok()?;
    Some(region_grid_for_mapping(&mapping))
}

fn full_earth_region_grid(scale: i32) -> RegionGrid {
    let Ok(mapping) = EarthScaleMapping::for_denominator(
        scale.max(1),
        FULL_EARTH_MIN_LATITUDE,
        FULL_EARTH_MAX_LATITUDE,
    ) else {
        return RegionGrid {
            start_region_x: -40,
            start_region_z: -20,
            cols: 80,
            rows: 40,
        };
    };
    region_grid_for_mapping(&mapping)
}

fn region_grid_for_mapping(mapping: &EarthScaleMapping) -> RegionGrid {
    let start_x = floor_div_i32(-(mapping.width_blocks / 2), REGION_SIZE_BLOCKS);
    let end_x = floor_div_i32(
        mapping.width_blocks - 1 - (mapping.width_blocks / 2),
        REGION_SIZE_BLOCKS,
    );
    let start_z = floor_div_i32(-(mapping.height_blocks / 2), REGION_SIZE_BLOCKS);
    let end_z = floor_div_i32(
        mapping.height_blocks - 1 - (mapping.height_blocks / 2),
        REGION_SIZE_BLOCKS,
    );
    RegionGrid {
        start_region_x: start_x,
        start_region_z: start_z,
        cols: (end_x - start_x + 1).max(1),
        rows: (end_z - start_z + 1).max(1),
    }
}

fn region_grid_for_bounds(scale: i32, bounds: GeoBounds) -> Option<RegionGrid> {
    let mapping = EarthScaleMapping::for_denominator(
        scale.max(1),
        FULL_EARTH_MIN_LATITUDE,
        FULL_EARTH_MAX_LATITUDE,
    )
    .ok()?;
    let west = bounds.west_longitude.clamp(-180.0, 179.999_999);
    let east = bounds.east_longitude.clamp(-180.0, 180.0);
    let north = bounds
        .north_latitude
        .clamp(FULL_EARTH_MIN_LATITUDE, FULL_EARTH_MAX_LATITUDE);
    let south = bounds
        .south_latitude
        .clamp(FULL_EARTH_MIN_LATITUDE, FULL_EARTH_MAX_LATITUDE);
    if west >= east || south >= north {
        return None;
    }
    let start_map_x = mapping.block_x_for_longitude(west).ok()?;
    let end_map_x = if east >= 180.0 {
        mapping.width_blocks - 1
    } else {
        mapping.block_x_for_longitude(east).ok()?
    };
    let start_map_z = mapping.block_z_for_latitude(north).ok()?;
    let end_map_z = mapping.block_z_for_latitude(south).ok()?;
    let start_x = floor_div_i32(start_map_x - (mapping.width_blocks / 2), REGION_SIZE_BLOCKS);
    let end_x = floor_div_i32(end_map_x - (mapping.width_blocks / 2), REGION_SIZE_BLOCKS);
    let start_z = floor_div_i32(
        start_map_z - (mapping.height_blocks / 2),
        REGION_SIZE_BLOCKS,
    );
    let end_z = floor_div_i32(end_map_z - (mapping.height_blocks / 2), REGION_SIZE_BLOCKS);
    Some(RegionGrid {
        start_region_x: start_x,
        start_region_z: start_z,
        cols: (end_x - start_x + 1).max(1),
        rows: (end_z - start_z + 1).max(1),
    })
}

fn floor_div_i32(value: i32, divisor: i32) -> i32 {
    value.div_euclid(divisor)
}

fn configured_path_text(name: &str) -> Option<String> {
    std::env::var_os(name).and_then(|value| {
        let text = value.to_string_lossy().trim().to_string();
        (!text.is_empty()).then_some(text)
    })
}

fn default_heightmap_path() -> String {
    configured_path_text(HEIGHTMAP_PATH_ENV).unwrap_or_default()
}

fn default_tif_root() -> String {
    if let Some(root) = configured_path_text(TIF_ROOT_ENV) {
        return root;
    }
    configured_path_text(DATA_ROOT_ENV)
        .map(|root| {
            let root = Path::new(&root);
            let tif_root = if root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("TifFiles"))
            {
                root.to_path_buf()
            } else {
                root.join("TifFiles")
            };
            tif_root.display().to_string()
        })
        .unwrap_or_default()
}

fn default_true_marble_path(tif_root: &str) -> String {
    configured_path_text(SURFACE_RASTER_ENV).unwrap_or_else(|| {
        if tif_root.trim().is_empty() {
            String::new()
        } else {
            true_marble_from_tif_root(tif_root)
        }
    })
}

fn default_world_dir() -> String {
    configured_path_text(OUTPUT_ROOT_ENV).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(DEFAULT_WORLD_DIR_NAME)
            .display()
            .to_string()
    })
}

fn true_marble_from_tif_root(root: &str) -> String {
    Path::new(root)
        .join("terrain")
        .join("TrueMarble.vrt")
        .display()
        .to_string()
}

fn path_status(label: &str, path: &str, file: bool) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return format!("{label} status: empty");
    }
    let path = Path::new(trimmed);
    let ok = if file { path.is_file() } else { path.is_dir() };
    if ok {
        format!("{label} status: found")
    } else {
        format!("{label} status: not found")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ExistingProjectSettingsPatch {
    heightmap_path: Option<String>,
    tif_root: Option<String>,
    true_marble_path: Option<String>,
    scale: Option<i32>,
    grid: Option<RegionGrid>,
    format: Option<OutputFormatChoice>,
    linear_compression_level: Option<i32>,
    mca_compression_level: Option<u32>,
    threads: Option<usize>,
    status: Option<ChunkStatusChoice>,
    vertical_scale: Option<String>,
    loaded_manifest: bool,
    loaded_resume_fingerprint: bool,
    unsupported_plan: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ExistingProjectSettingsReport {
    loaded_manifest: bool,
    loaded_resume_fingerprint: bool,
    grid: RegionGrid,
}

fn apply_existing_project_settings(
    options: &mut GenerationOptions,
    world_dir: &Path,
) -> Result<Option<ExistingProjectSettingsReport>, String> {
    let Some(patch) = load_existing_project_settings_patch(world_dir)? else {
        return Ok(None);
    };
    if patch.unsupported_plan && patch.grid.is_none() {
        return Err("plan-based project metadata was found, but the GUI can resume region-grid projects only".to_string());
    }
    let Some(grid) = patch.grid else {
        return Err("project metadata does not include a resumable region grid".to_string());
    };

    options.world_dir = world_dir.display().to_string();
    if let Some(value) = patch.heightmap_path {
        options.heightmap_path = value;
    }
    if let Some(value) = patch.true_marble_path {
        options.true_marble_path = value;
    }
    if let Some(value) = patch.tif_root {
        options.tif_root = value;
    } else if let Some(value) = infer_tif_root_from_surface_raster(&options.true_marble_path) {
        options.tif_root = value;
    }
    if let Some(value) = patch.scale {
        options.scale = value.max(1);
    }
    options.extent_mode = ExtentMode::RegionGrid;
    options.start_region_x = grid.start_region_x;
    options.start_region_z = grid.start_region_z;
    options.cols = grid.cols.max(1);
    options.rows = grid.rows.max(1);
    if let Some(value) = patch.format {
        options.format = value;
    }
    if let Some(value) = patch.linear_compression_level {
        options.linear_compression_level = value.clamp(1, 22);
    }
    if let Some(value) = patch.mca_compression_level {
        options.mca_compression_level = value.min(9);
    }
    if let Some(value) = patch.threads {
        options.threads = value.max(1);
    }
    if let Some(value) = patch.status {
        options.status = value;
    }
    if let Some(value) = patch.vertical_scale {
        options.vertical_scale = value;
    }

    Ok(Some(ExistingProjectSettingsReport {
        loaded_manifest: patch.loaded_manifest,
        loaded_resume_fingerprint: patch.loaded_resume_fingerprint,
        grid,
    }))
}

fn load_existing_project_settings_patch(
    world_dir: &Path,
) -> Result<Option<ExistingProjectSettingsPatch>, String> {
    let manifest_patch = load_manifest_project_settings(world_dir)?;
    let resume_patch = load_resume_fingerprint_project_settings(world_dir)?;
    if manifest_patch.is_none() && resume_patch.is_none() {
        return Ok(None);
    }
    let mut merged = manifest_patch.unwrap_or_default();
    if let Some(resume_patch) = resume_patch {
        merged.merge_prefer_new(resume_patch);
    }
    Ok(Some(merged))
}

impl ExistingProjectSettingsPatch {
    fn merge_prefer_new(&mut self, other: Self) {
        self.heightmap_path = other.heightmap_path.or(self.heightmap_path.take());
        self.tif_root = other.tif_root.or(self.tif_root.take());
        self.true_marble_path = other.true_marble_path.or(self.true_marble_path.take());
        self.scale = other.scale.or(self.scale);
        self.grid = other.grid.or(self.grid);
        self.format = other.format.or(self.format);
        self.linear_compression_level = other
            .linear_compression_level
            .or(self.linear_compression_level);
        self.mca_compression_level = other.mca_compression_level.or(self.mca_compression_level);
        self.threads = other.threads.or(self.threads);
        self.status = other.status.or(self.status);
        self.vertical_scale = other.vertical_scale.or(self.vertical_scale.take());
        self.loaded_manifest |= other.loaded_manifest;
        self.loaded_resume_fingerprint |= other.loaded_resume_fingerprint;
        self.unsupported_plan |= other.unsupported_plan;
    }
}

fn load_manifest_project_settings(
    world_dir: &Path,
) -> Result<Option<ExistingProjectSettingsPatch>, String> {
    let manifest_path = world_dir.join(SURVIVAL_MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let values = read_manifest_properties(&manifest_path)?;
    let mut patch = ExistingProjectSettingsPatch {
        loaded_manifest: true,
        ..ExistingProjectSettingsPatch::default()
    };
    patch.heightmap_path = manifest_string(&values, "generation.heightmapPath");
    patch.true_marble_path = manifest_string(&values, "generation.surfaceMaterialPath");
    patch.tif_root = patch
        .true_marble_path
        .as_deref()
        .and_then(infer_tif_root_from_surface_raster);
    patch.scale = manifest_i32(&values, "generation.scaleDenominator");
    patch.format = manifest_string(&values, "generation.format")
        .as_deref()
        .and_then(parse_output_format_choice);
    patch.threads = manifest_usize(&values, "generation.threads");
    patch.status = manifest_string(&values, "generation.chunkStatus")
        .as_deref()
        .and_then(parse_chunk_status_choice);
    patch.vertical_scale = manifest_vertical_scale(
        values
            .get("generation.verticalScaleMode")
            .map(String::as_str),
        values.get("generation.verticalScale").map(String::as_str),
    );
    patch.linear_compression_level = manifest_i32(&values, "generation.linearCompressionLevel")
        .or_else(|| manifest_i32(&values, "generation.linearCompression"));
    patch.mca_compression_level = manifest_u32(&values, "generation.mcaCompressionLevel")
        .or_else(|| manifest_u32(&values, "generation.mcaCompression"));
    patch.grid = match (
        manifest_i32(&values, "generation.startRegionX"),
        manifest_i32(&values, "generation.startRegionZ"),
        manifest_i32(&values, "generation.regionCols"),
        manifest_i32(&values, "generation.regionRows"),
    ) {
        (Some(start_region_x), Some(start_region_z), Some(cols), Some(rows)) => Some(RegionGrid {
            start_region_x,
            start_region_z,
            cols: cols.max(1),
            rows: rows.max(1),
        }),
        _ => None,
    };
    patch.unsupported_plan = values.contains_key("generation.planCsv") && patch.grid.is_none();
    Ok(Some(patch))
}

fn load_resume_fingerprint_project_settings(
    world_dir: &Path,
) -> Result<Option<ExistingProjectSettingsPatch>, String> {
    let journal_path = world_dir.join(VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME);
    if !journal_path.is_file() {
        return Ok(None);
    }
    let file = std::fs::File::open(&journal_path)
        .map_err(|error| format!("failed to open resume journal: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| format!("failed to read resume journal: {error}"))?;
    if line.trim().is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|error| format!("failed to parse resume journal fingerprint: {error}"))?;
    let fingerprint = value.get("fingerprint").unwrap_or(&value);
    let mut patch = ExistingProjectSettingsPatch {
        loaded_resume_fingerprint: true,
        ..ExistingProjectSettingsPatch::default()
    };
    let command = fingerprint
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    patch.unsupported_plan = command.contains("plan-parallel");
    patch.heightmap_path = json_path_string(fingerprint, "heightmap");
    patch.true_marble_path = json_path_string(fingerprint, "surfaceMaterial");
    patch.tif_root = patch
        .true_marble_path
        .as_deref()
        .and_then(infer_tif_root_from_surface_raster);
    patch.scale = json_i32(fingerprint, "scale");
    patch.format = fingerprint
        .get("format")
        .and_then(Value::as_str)
        .and_then(parse_output_format_choice);
    patch.status = fingerprint
        .get("chunkStatus")
        .and_then(Value::as_str)
        .and_then(parse_chunk_status_choice);
    patch.vertical_scale = manifest_vertical_scale(
        fingerprint.get("verticalScaleMode").and_then(Value::as_str),
        fingerprint
            .get("verticalScale")
            .map(json_number_or_string_text)
            .as_deref(),
    );
    if let Some(compression) = fingerprint.get("compression") {
        patch.linear_compression_level = json_i32(compression, "linear");
        patch.mca_compression_level =
            json_u64(compression, "mca").and_then(|value| u32::try_from(value).ok());
    }
    patch.grid = match (
        json_i32(fingerprint, "startRegionX"),
        json_i32(fingerprint, "startRegionZ"),
        json_i32(fingerprint, "regionCols"),
        json_i32(fingerprint, "regionRows"),
    ) {
        (Some(start_region_x), Some(start_region_z), Some(cols), Some(rows)) => Some(RegionGrid {
            start_region_x,
            start_region_z,
            cols: cols.max(1),
            rows: rows.max(1),
        }),
        _ => None,
    };
    Ok(Some(patch))
}

fn read_manifest_properties(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        values.insert(
            key.trim().to_string(),
            unescape_manifest_value(value.trim()),
        );
    }
    Ok(values)
}

fn unescape_manifest_value(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => output.push('\\'),
            Some('r') => output.push('\r'),
            Some('n') => output.push('\n'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

fn manifest_string(values: &BTreeMap<String, String>, key: &str) -> Option<String> {
    values
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("default"))
        .map(ToOwned::to_owned)
}

fn manifest_i32(values: &BTreeMap<String, String>, key: &str) -> Option<i32> {
    manifest_string(values, key)?.parse().ok()
}

fn manifest_u32(values: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    manifest_string(values, key)?.parse().ok()
}

fn manifest_usize(values: &BTreeMap<String, String>, key: &str) -> Option<usize> {
    manifest_string(values, key)?.parse().ok()
}

fn manifest_vertical_scale(mode: Option<&str>, value: Option<&str>) -> Option<String> {
    let mode = mode.map(str::trim).filter(|mode| !mode.is_empty());
    match mode {
        Some(mode) if mode.eq_ignore_ascii_case("auto") => Some("auto".to_string()),
        Some(mode) if mode.eq_ignore_ascii_case("legacy") => Some("legacy".to_string()),
        Some(mode) if mode.eq_ignore_ascii_case("explicit") => value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        Some(mode) => Some(mode.to_string()),
        None => value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    }
}

fn parse_output_format_choice(value: &str) -> Option<OutputFormatChoice> {
    let normalized = value.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "linear" | "linear_v2" | "linearv2") {
        Some(OutputFormatChoice::Linear)
    } else if matches!(normalized.as_str(), "mca" | "anvil") {
        Some(OutputFormatChoice::Mca)
    } else {
        None
    }
}

fn parse_chunk_status_choice(value: &str) -> Option<ChunkStatusChoice> {
    let normalized = value
        .trim()
        .strip_prefix("minecraft:")
        .unwrap_or_else(|| value.trim())
        .to_ascii_lowercase();
    match normalized.as_str() {
        "surface" => Some(ChunkStatusChoice::Surface),
        "carvers" => Some(ChunkStatusChoice::Carvers),
        _ => None,
    }
}

fn infer_tif_root_from_surface_raster(path: &str) -> Option<String> {
    let path = Path::new(path.trim());
    let file_name = path.file_name()?.to_str()?;
    if !file_name.eq_ignore_ascii_case("TrueMarble.vrt") {
        return None;
    }
    let terrain_dir = path.parent()?;
    if terrain_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("terrain"))
    {
        terrain_dir.parent().map(|root| root.display().to_string())
    } else {
        None
    }
}

fn json_path_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .get("path")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn json_number_or_string_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn scan_existing_region_files(
    world_dir: &Path,
    extension: &str,
    grid: RegionGrid,
) -> Vec<(i32, i32)> {
    let region_dir = world_dir.join("region");
    let Ok(entries) = std::fs::read_dir(region_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| parse_region_file_name(&entry.file_name().to_string_lossy(), extension))
        .filter(|(region_x, region_z)| region_in_grid(*region_x, *region_z, grid))
        .collect()
}

fn parse_region_file_name(file_name: &str, extension: &str) -> Option<(i32, i32)> {
    let parts = file_name.split('.').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "r" || !parts[3].eq_ignore_ascii_case(extension) {
        return None;
    }
    let region_x = parts[1].parse::<i32>().ok()?;
    let region_z = parts[2].parse::<i32>().ok()?;
    Some((region_x, region_z))
}

fn region_in_grid(region_x: i32, region_z: i32, grid: RegionGrid) -> bool {
    let end_x = grid.start_region_x.saturating_add(grid.cols.max(1));
    let end_z = grid.start_region_z.saturating_add(grid.rows.max(1));
    region_x >= grid.start_region_x
        && region_x < end_x
        && region_z >= grid.start_region_z
        && region_z < end_z
}

#[derive(Clone, Debug)]
struct RegionProgressGrid {
    grid: RegionGrid,
    states: Vec<u8>,
    completed_regions: usize,
    failed_regions: usize,
}

impl RegionProgressGrid {
    fn new(grid: RegionGrid) -> Self {
        debug_assert_eq!(REGION_STATE_MISSING, 0);
        let cols = grid.cols.max(1) as usize;
        let rows = grid.rows.max(1) as usize;
        Self {
            grid,
            states: vec![REGION_STATE_QUEUED; cols.saturating_mul(rows)],
            completed_regions: 0,
            failed_regions: 0,
        }
    }

    fn index(&self, region_x: i32, region_z: i32) -> Option<usize> {
        if !region_in_grid(region_x, region_z, self.grid) {
            return None;
        }
        let col = usize::try_from(region_x - self.grid.start_region_x).ok()?;
        let row = usize::try_from(region_z - self.grid.start_region_z).ok()?;
        Some(col + row * self.grid.cols.max(1) as usize)
    }

    fn mark(&mut self, region_x: i32, region_z: i32, state: u8) -> bool {
        let Some(index) = self.index(region_x, region_z) else {
            return false;
        };
        let previous = self.states[index];
        if previous == state {
            return false;
        }
        if is_completed_region_state(previous) {
            self.completed_regions = self.completed_regions.saturating_sub(1);
        }
        if previous == REGION_STATE_FAILED {
            self.failed_regions = self.failed_regions.saturating_sub(1);
        }
        self.states[index] = state;
        if is_completed_region_state(state) {
            self.completed_regions = self.completed_regions.saturating_add(1);
        }
        if state == REGION_STATE_FAILED {
            self.failed_regions = self.failed_regions.saturating_add(1);
        }
        true
    }
}

fn is_completed_region_state(state: u8) -> bool {
    matches!(state, REGION_STATE_SKIPPED | REGION_STATE_GENERATED)
}

fn status_texture_layout(grid: RegionGrid) -> (usize, usize, usize) {
    let cols = grid.cols.max(1) as usize;
    let rows = grid.rows.max(1) as usize;
    let scale = cols
        .div_ceil(MAX_STATUS_TEXTURE_DIMENSION)
        .max(rows.div_ceil(MAX_STATUS_TEXTURE_DIMENSION))
        .max(1);
    (cols.div_ceil(scale), rows.div_ceil(scale), scale)
}

fn status_overlay_pixels(
    progress_grid: &RegionProgressGrid,
    width: usize,
    height: usize,
    scale: usize,
) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; width.saturating_mul(height)];
    let mut priorities = vec![0u8; pixels.len()];
    let cols = progress_grid.grid.cols.max(1) as usize;
    let rows = progress_grid.grid.rows.max(1) as usize;
    for row in 0..rows {
        for col in 0..cols {
            let source_index = col + row * cols;
            let Some(state) = progress_grid.states.get(source_index).copied() else {
                continue;
            };
            let target_col = (col / scale).min(width.saturating_sub(1));
            let target_row = (row / scale).min(height.saturating_sub(1));
            let target_index = target_col + target_row * width;
            let priority = region_state_priority(state);
            if priority >= priorities[target_index] {
                priorities[target_index] = priority;
                pixels[target_index] = region_state_color(state);
            }
        }
    }
    pixels
}

fn region_state_priority(state: u8) -> u8 {
    match state {
        REGION_STATE_FAILED => 5,
        REGION_STATE_RUNNING => 4,
        REGION_STATE_GENERATED | REGION_STATE_SKIPPED => 3,
        REGION_STATE_QUEUED => 1,
        _ => 0,
    }
}

fn region_state_color(state: u8) -> egui::Color32 {
    match state {
        REGION_STATE_RUNNING => egui::Color32::from_rgba_premultiplied(255, 210, 64, 210),
        REGION_STATE_SKIPPED => egui::Color32::from_rgba_premultiplied(80, 170, 255, 145),
        REGION_STATE_GENERATED => egui::Color32::from_rgba_premultiplied(64, 220, 120, 150),
        REGION_STATE_FAILED => egui::Color32::from_rgba_premultiplied(235, 60, 60, 220),
        _ => egui::Color32::TRANSPARENT,
    }
}

fn target_region_rect(parent: egui::Rect, scale: i32, grid: RegionGrid) -> egui::Rect {
    let full = full_earth_region_grid(scale.max(1));
    let full_cols = full.cols.max(1) as f32;
    let full_rows = full.rows.max(1) as f32;
    let x0 = ((grid.start_region_x - full.start_region_x) as f32 / full_cols).clamp(0.0, 1.0);
    let y0 = ((grid.start_region_z - full.start_region_z) as f32 / full_rows).clamp(0.0, 1.0);
    let x1 = ((grid.start_region_x + grid.cols.max(1) - full.start_region_x) as f32 / full_cols)
        .clamp(0.0, 1.0);
    let y1 = ((grid.start_region_z + grid.rows.max(1) - full.start_region_z) as f32 / full_rows)
        .clamp(0.0, 1.0);
    egui::Rect::from_min_max(
        egui::pos2(
            parent.left() + parent.width() * x0,
            parent.top() + parent.height() * y0,
        ),
        egui::pos2(
            parent.left() + parent.width() * x1.max(x0),
            parent.top() + parent.height() * y1.max(y0),
        ),
    )
}

fn draw_rect_outline(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    painter.line_segment([rect.left_top(), rect.right_top()], stroke);
    painter.line_segment([rect.right_top(), rect.right_bottom()], stroke);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()], stroke);
    painter.line_segment([rect.left_bottom(), rect.left_top()], stroke);
}

fn world_map_background_image() -> egui::ColorImage {
    let decoder = png::Decoder::new(Cursor::new(WORLD_MAP_BACKGROUND_PNG));
    let mut reader = decoder
        .read_info()
        .expect("embedded world map PNG metadata must decode");
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .expect("embedded world map PNG pixels must decode");
    let bytes = &buffer[..info.buffer_size()];
    let pixels = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgb, png::BitDepth::Eight) => bytes
            .chunks_exact(3)
            .map(|pixel| egui::Color32::from_rgb(pixel[0], pixel[1], pixel[2]))
            .collect(),
        (png::ColorType::Rgba, png::BitDepth::Eight) => bytes
            .chunks_exact(4)
            .map(|pixel| {
                egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3])
            })
            .collect(),
        _ => panic!("embedded world map PNG must be 8-bit RGB or RGBA"),
    };
    egui::ColorImage::new([info.width as usize, info.height as usize], pixels)
}

#[derive(Debug)]
enum GeneratorProgressEvent {
    WorkerTuningStarted {
        enabled: bool,
        requested_threads: Option<usize>,
        submitted_regions: Option<usize>,
        max_samples: Option<usize>,
    },
    WorkerTuningFinished {
        result: WorkerTuningResult,
    },
    BatchStarted {
        grid: RegionGrid,
        total_regions: usize,
        resume_fingerprint_matched: bool,
        resume_journal_regions: usize,
    },
    RegionStarted {
        region_x: i32,
        region_z: i32,
    },
    RegionSkipped {
        region_x: i32,
        region_z: i32,
        elapsed_millis: Option<u64>,
    },
    RegionGenerated {
        region_x: i32,
        region_z: i32,
        elapsed_millis: Option<u64>,
    },
    RegionFailed {
        region_x: i32,
        region_z: i32,
        message: String,
    },
    BatchSummary {
        elapsed_millis: Option<u64>,
        regions_per_hour: Option<f64>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
struct WorkerTuningResult {
    mode: String,
    selected_worker_threads: Option<usize>,
    selected_rayon_threads: Option<usize>,
    parallel_column_sampling: Option<bool>,
    sample_count: Option<usize>,
    land_samples: Option<usize>,
    ocean_samples: Option<usize>,
    mixed_samples: Option<usize>,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyRegionStatus {
    Generated,
    Skipped,
}

#[derive(Debug)]
enum WorkerEvent {
    Line(String),
    Progress {
        tag: Option<ProcessTag>,
        event: GeneratorProgressEvent,
    },
    RegionFinished {
        status: LegacyRegionStatus,
        region_x: i32,
        region_z: i32,
        elapsed_millis: Option<u64>,
    },
    Summary {
        elapsed_millis: Option<u64>,
        regions_per_hour: Option<f64>,
    },
    Finished {
        code: Option<i32>,
        elapsed: Duration,
    },
    Failed(String),
}

struct GenerationRun {
    receiver: Receiver<WorkerEvent>,
    cancel: Arc<AtomicBool>,
    children: Arc<Mutex<Vec<RunningChild>>>,
    started_at: Instant,
}

impl GenerationRun {
    fn request_stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.children.lock() {
            for child in guard.iter_mut() {
                if !child.finished {
                    let _ = child.child.kill();
                }
            }
        }
    }
}

struct RunningChild {
    tag: Option<ProcessTag>,
    child: Child,
    finished: bool,
    exit_code: Option<i32>,
}

#[derive(Default)]
struct ProgressState {
    completed_regions: usize,
    total_regions: usize,
    failed_regions: usize,
    completed_events: usize,
    last_region: String,
    elapsed_millis: Option<u64>,
    regions_per_hour: Option<f64>,
    rolling_regions_per_hour: Option<f64>,
    exit_code: Option<i32>,
    running: bool,
    failed: Option<String>,
    grid: Option<RegionProgressGrid>,
    recent_completions: VecDeque<Instant>,
    resume_fingerprint_matched: Option<bool>,
    resume_journal_regions: usize,
    worker_tuning_enabled: Option<bool>,
    worker_tuning_requested_threads: Option<usize>,
    worker_tuning_submitted_regions: Option<usize>,
    worker_tuning_max_samples: Option<usize>,
    worker_tuning_result: Option<WorkerTuningResult>,
}

struct MapOverlayState {
    enabled: bool,
    background_texture: Option<egui::TextureHandle>,
    status_texture: Option<egui::TextureHandle>,
    status_texture_size: [usize; 2],
    status_pixels: Vec<egui::Color32>,
    status_dirty: bool,
    last_status_upload: Option<Instant>,
}

impl Default for MapOverlayState {
    fn default() -> Self {
        Self {
            enabled: false,
            background_texture: None,
            status_texture: None,
            status_texture_size: [0, 0],
            status_pixels: Vec::new(),
            status_dirty: true,
            last_status_upload: None,
        }
    }
}

impl ProgressState {
    fn reset(&mut self, grid: RegionGrid) {
        *self = Self {
            total_regions: (grid.cols.max(1) as usize).saturating_mul(grid.rows.max(1) as usize),
            running: true,
            grid: Some(RegionProgressGrid::new(grid)),
            ..Default::default()
        };
    }

    fn reset_from_batch(&mut self, grid: RegionGrid, total_regions: usize) {
        self.total_regions = total_regions;
        self.grid = Some(RegionProgressGrid::new(grid));
        self.completed_regions = 0;
        self.failed_regions = 0;
    }

    fn mark_existing_regions(&mut self, regions: impl IntoIterator<Item = (i32, i32)>) {
        for (region_x, region_z) in regions {
            if let Some(grid) = &mut self.grid {
                grid.mark(region_x, region_z, REGION_STATE_GENERATED);
            }
        }
        self.refresh_counts_from_grid();
    }

    fn mark_region(&mut self, region_x: i32, region_z: i32, state: u8) {
        if let Some(grid) = &mut self.grid {
            let changed = grid.mark(region_x, region_z, state);
            if changed && is_completed_region_state(state) {
                self.record_completion();
            }
        } else if is_completed_region_state(state) {
            self.completed_regions = self.completed_regions.saturating_add(1);
            self.record_completion();
        }
        self.refresh_counts_from_grid();
        self.last_region = format!("r.{region_x}.{region_z}");
    }

    fn refresh_counts_from_grid(&mut self) {
        if let Some(grid) = &self.grid {
            self.completed_regions = grid.completed_regions.min(self.total_regions);
            self.failed_regions = grid.failed_regions;
        }
    }

    fn record_completion(&mut self) {
        let now = Instant::now();
        self.completed_events = self.completed_events.saturating_add(1);
        self.recent_completions.push_back(now);
        self.refresh_rolling_speed(now);
    }

    fn refresh_rolling_speed(&mut self, now: Instant) {
        while self
            .recent_completions
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > ROLLING_SPEED_WINDOW)
        {
            self.recent_completions.pop_front();
        }
        if self.recent_completions.is_empty() {
            self.rolling_regions_per_hour = None;
        } else {
            self.rolling_regions_per_hour = Some(
                (self.recent_completions.len() as f64) * 3600.0
                    / ROLLING_SPEED_WINDOW.as_secs_f64().max(1.0),
            );
        }
    }

    fn progress_fraction(&self) -> f32 {
        if self.total_regions == 0 {
            0.0
        } else {
            (self.completed_regions as f32 / self.total_regions as f32).clamp(0.0, 1.0)
        }
    }
}

struct EarthMapGuiApp {
    options: GenerationOptions,
    progress: ProgressState,
    map_overlay: MapOverlayState,
    run: Option<GenerationRun>,
    log_lines: Vec<String>,
    loaded_project_world_dir: String,
    project_settings_status: Option<String>,
}

impl Default for EarthMapGuiApp {
    fn default() -> Self {
        Self {
            options: GenerationOptions::default(),
            progress: ProgressState::default(),
            map_overlay: MapOverlayState::default(),
            run: None,
            log_lines: Vec::new(),
            loaded_project_world_dir: String::new(),
            project_settings_status: None,
        }
    }
}

impl EarthMapGuiApp {
    fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::default();
        if let Some(storage) = creation_context.storage {
            if let Some(options) = storage
                .get_string(GUI_OPTIONS_STORAGE_KEY)
                .and_then(|encoded| serde_json::from_str::<GenerationOptions>(&encoded).ok())
            {
                app.options = options;
                app.apply_existing_project_settings_for_world_dir();
            }
        }
        app
    }

    fn apply_existing_project_settings_for_world_dir(&mut self) {
        if self.run.is_some() {
            return;
        }
        let world_dir_text = self.options.world_dir.trim().to_string();
        if world_dir_text.is_empty() || self.loaded_project_world_dir == world_dir_text {
            return;
        }
        let world_dir = PathBuf::from(&world_dir_text);
        match apply_existing_project_settings(&mut self.options, &world_dir) {
            Ok(Some(report)) => {
                self.loaded_project_world_dir = self.options.world_dir.trim().to_string();
                let existing_regions = self.reload_existing_region_progress();
                let source = match (report.loaded_manifest, report.loaded_resume_fingerprint) {
                    (true, true) => "manifest and resume journal",
                    (true, false) => "manifest",
                    (false, true) => "resume journal",
                    (false, false) => "project metadata",
                };
                let total_regions = (report.grid.cols.max(1) as usize)
                    .saturating_mul(report.grid.rows.max(1) as usize);
                let message = format!(
                    "Loaded project settings from {source}; {existing_regions}/{total_regions} existing regions detected."
                );
                self.project_settings_status = Some(message.clone());
                self.push_log_line(message);
            }
            Ok(None) => {
                self.loaded_project_world_dir.clear();
                self.project_settings_status = None;
            }
            Err(error) => {
                self.loaded_project_world_dir = world_dir_text;
                self.project_settings_status = Some(format!("Project settings: {error}"));
            }
        }
    }

    fn reload_existing_region_progress(&mut self) -> usize {
        let grid = self.options.resolved_region_grid();
        let total_regions = (grid.cols.max(1) as usize).saturating_mul(grid.rows.max(1) as usize);
        self.progress = ProgressState::default();
        self.progress.reset_from_batch(grid, total_regions);
        let existing_regions = scan_existing_region_files(
            Path::new(self.options.world_dir.trim()),
            self.options.format.region_extension(),
            grid,
        );
        let existing_region_count = existing_regions.len();
        self.progress.mark_existing_regions(existing_regions);
        self.map_overlay.status_dirty = true;
        existing_region_count
    }

    fn start_generation(&mut self) {
        if self.run.is_some() {
            return;
        }
        self.apply_existing_project_settings_for_world_dir();
        if let Some(error) = self.options.validation_error() {
            self.log_lines.push(format!("Configuration error: {error}"));
            return;
        }
        let grid = self.options.resolved_region_grid();
        self.progress.reset(grid);
        let existing_regions = scan_existing_region_files(
            Path::new(self.options.world_dir.trim()),
            self.options.format.region_extension(),
            grid,
        );
        self.progress.mark_existing_regions(existing_regions);
        self.map_overlay.status_dirty = true;
        self.log_lines.clear();
        let (sender, receiver) = bounded(WORKER_EVENT_CHANNEL_CAPACITY);
        let cancel = Arc::new(AtomicBool::new(false));
        let children = Arc::new(Mutex::new(Vec::new()));
        let commands = build_generation_commands(&self.options);
        let options = self.options.clone();
        let cancel_for_thread = Arc::clone(&cancel);
        let children_for_thread = Arc::clone(&children);
        thread::spawn(move || {
            run_generation_processes(
                commands,
                options,
                sender,
                cancel_for_thread,
                children_for_thread,
            );
        });
        self.run = Some(GenerationRun {
            receiver,
            cancel,
            children,
            started_at: Instant::now(),
        });
    }

    fn stop_generation(&mut self) {
        if let Some(run) = &self.run {
            run.request_stop();
        }
    }

    fn drain_worker_events(&mut self) {
        let mut should_clear_run = false;
        let events = self
            .run
            .as_ref()
            .map(|run| run.receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            match event {
                WorkerEvent::Line(line) => self.push_log_line(line),
                WorkerEvent::Progress { tag, event } => self.apply_progress_event(tag, event),
                WorkerEvent::RegionFinished {
                    status,
                    region_x,
                    region_z,
                    elapsed_millis,
                } => {
                    let state = match status {
                        LegacyRegionStatus::Generated => REGION_STATE_GENERATED,
                        LegacyRegionStatus::Skipped => REGION_STATE_SKIPPED,
                    };
                    self.progress.mark_region(region_x, region_z, state);
                    self.map_overlay.status_dirty = true;
                    if let Some(elapsed_millis) = elapsed_millis {
                        self.push_log_line(format!(
                            "region {} completed in {:.3}s",
                            self.progress.last_region,
                            elapsed_millis as f64 / 1000.0
                        ));
                    }
                }
                WorkerEvent::Summary {
                    elapsed_millis,
                    regions_per_hour,
                } => {
                    self.progress.elapsed_millis = elapsed_millis;
                    self.progress.regions_per_hour = regions_per_hour;
                }
                WorkerEvent::Finished { code, elapsed } => {
                    self.progress.running = false;
                    self.progress.exit_code = code;
                    self.progress.elapsed_millis =
                        Some(elapsed.as_millis().try_into().unwrap_or(u64::MAX));
                    self.push_log_line(format!(
                        "process finished with code {} in {:.3}s",
                        code.map_or_else(|| "unknown".to_string(), |code| code.to_string()),
                        elapsed.as_secs_f64()
                    ));
                    should_clear_run = true;
                }
                WorkerEvent::Failed(message) => {
                    self.progress.running = false;
                    self.progress.failed = Some(message.clone());
                    self.push_log_line(format!("error: {message}"));
                    should_clear_run = true;
                }
            }
        }
        if should_clear_run {
            self.run = None;
        }
    }

    fn apply_progress_event(&mut self, tag: Option<ProcessTag>, event: GeneratorProgressEvent) {
        match event {
            GeneratorProgressEvent::WorkerTuningStarted {
                enabled,
                requested_threads,
                submitted_regions,
                max_samples,
            } => {
                self.progress.worker_tuning_enabled = Some(enabled);
                self.progress.worker_tuning_requested_threads = requested_threads;
                self.progress.worker_tuning_submitted_regions = submitted_regions;
                self.progress.worker_tuning_max_samples = max_samples;
                self.progress.worker_tuning_result = None;
                let prefix = process_log_prefix(tag);
                if enabled {
                    self.push_log_line(format!(
                        "{prefix}worker tuning started: requested={} regions={} maxSamples={}",
                        requested_threads
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                        submitted_regions
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                        max_samples
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    ));
                } else {
                    self.push_log_line(format!("{prefix}worker tuning disabled"));
                }
            }
            GeneratorProgressEvent::WorkerTuningFinished { result } => {
                self.push_log_line(format!(
                    "{}{}",
                    process_log_prefix(tag),
                    worker_tuning_log_line(&result)
                ));
                self.progress.worker_tuning_result = Some(result);
            }
            GeneratorProgressEvent::BatchStarted {
                grid,
                total_regions,
                resume_fingerprint_matched,
                resume_journal_regions,
            } => {
                if tag.is_none() {
                    self.progress.reset_from_batch(grid, total_regions);
                    self.progress.resume_fingerprint_matched = Some(resume_fingerprint_matched);
                    self.progress.resume_journal_regions = resume_journal_regions;
                    self.map_overlay.status_dirty = true;
                }
                self.push_log_line(format!(
                    "{}batch started: {} regions, start=({}, {}), size={}x{}, resume match={}, journal regions={}",
                    process_log_prefix(tag),
                    total_regions,
                    grid.start_region_x,
                    grid.start_region_z,
                    grid.cols,
                    grid.rows,
                    resume_fingerprint_matched,
                    resume_journal_regions
                ));
            }
            GeneratorProgressEvent::RegionStarted { region_x, region_z } => {
                self.progress
                    .mark_region(region_x, region_z, REGION_STATE_RUNNING);
                self.map_overlay.status_dirty = true;
            }
            GeneratorProgressEvent::RegionSkipped {
                region_x,
                region_z,
                elapsed_millis,
            } => {
                self.progress
                    .mark_region(region_x, region_z, REGION_STATE_SKIPPED);
                self.map_overlay.status_dirty = true;
                if let Some(elapsed_millis) = elapsed_millis {
                    self.push_log_line(format!(
                        "region r.{region_x}.{region_z} resumed in {:.3}s",
                        elapsed_millis as f64 / 1000.0
                    ));
                }
            }
            GeneratorProgressEvent::RegionGenerated {
                region_x,
                region_z,
                elapsed_millis,
            } => {
                self.progress
                    .mark_region(region_x, region_z, REGION_STATE_GENERATED);
                self.map_overlay.status_dirty = true;
                if let Some(elapsed_millis) = elapsed_millis {
                    self.push_log_line(format!(
                        "region r.{region_x}.{region_z} generated in {:.3}s",
                        elapsed_millis as f64 / 1000.0
                    ));
                }
            }
            GeneratorProgressEvent::RegionFailed {
                region_x,
                region_z,
                message,
            } => {
                self.progress
                    .mark_region(region_x, region_z, REGION_STATE_FAILED);
                self.map_overlay.status_dirty = true;
                self.progress.failed = Some(message.clone());
                self.push_log_line(format!("region r.{region_x}.{region_z} failed: {message}"));
            }
            GeneratorProgressEvent::BatchSummary {
                elapsed_millis,
                regions_per_hour,
            } => {
                if tag.is_none() {
                    self.progress.elapsed_millis = elapsed_millis;
                    self.progress.regions_per_hour = regions_per_hour;
                }
                if let Some(tag) = tag {
                    let speed = regions_per_hour
                        .map(|value| format!("{value:.2} regions/hour"))
                        .unwrap_or_else(|| "unknown speed".to_string());
                    self.push_log_line(format!(
                        "{}batch summary: {speed}",
                        process_log_prefix(Some(tag))
                    ));
                }
            }
        }
    }

    fn push_log_line(&mut self, line: String) {
        self.log_lines.push(line);
        if self.log_lines.len() > 400 {
            let drain_count = self.log_lines.len() - 400;
            self.log_lines.drain(0..drain_count);
        }
    }

    fn elapsed_for_display(&self) -> Duration {
        if let Some(run) = &self.run {
            run.started_at.elapsed()
        } else {
            self.progress
                .elapsed_millis
                .map(Duration::from_millis)
                .unwrap_or_default()
        }
    }

    fn live_regions_per_hour(&mut self) -> f64 {
        if let Some(run) = &self.run {
            self.progress.refresh_rolling_speed(Instant::now());
            if let Some(rolling_regions_per_hour) = self.progress.rolling_regions_per_hour {
                return rolling_regions_per_hour;
            }
            let elapsed_hours = run.started_at.elapsed().as_secs_f64() / 3600.0;
            if elapsed_hours > 0.0 && self.progress.completed_events > 0 {
                return self.progress.completed_events as f64 / elapsed_hours;
            }
            return 0.0;
        }
        if let Some(regions_per_hour) = self.progress.regions_per_hour {
            return regions_per_hour;
        }
        let elapsed_hours = self.elapsed_for_display().as_secs_f64() / 3600.0;
        if elapsed_hours <= 0.0 {
            0.0
        } else {
            self.progress.completed_events as f64 / elapsed_hours
        }
    }

    fn remaining_regions(&self) -> usize {
        self.progress
            .total_regions
            .saturating_sub(self.progress.completed_regions)
    }

    fn remaining_for_display(&self, regions_per_hour: f64) -> Option<Duration> {
        let remaining_regions = self.remaining_regions();
        if remaining_regions == 0 {
            return Some(Duration::ZERO);
        }
        if !regions_per_hour.is_finite() || regions_per_hour <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(
            remaining_regions as f64 * 3600.0 / regions_per_hour,
        ))
    }

    fn worker_tuning_text(&self) -> Option<String> {
        if let Some(result) = &self.progress.worker_tuning_result {
            return Some(worker_tuning_display_text(result));
        }
        match self.progress.worker_tuning_enabled {
            Some(true) => Some(format!(
                "Tuning: running (up to {} samples)",
                self.progress
                    .worker_tuning_max_samples
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string())
            )),
            Some(false) => Some("Tuning: disabled".to_string()),
            None => None,
        }
    }

    fn show_map_overlay(&mut self, ui: &mut egui::Ui) {
        if self.map_overlay.background_texture.is_none() {
            let image = world_map_background_image();
            self.map_overlay.background_texture = Some(ui.ctx().load_texture(
                "earthmap-world-background",
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        self.update_status_texture(ui.ctx());

        let available_width = ui.available_width().max(320.0);
        let desired_size = egui::vec2(available_width, (available_width * 0.5).clamp(180.0, 360.0));
        let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        let painter = ui.painter_at(rect);
        if let Some(texture) = &self.map_overlay.background_texture {
            painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
        }

        let selected_grid = self
            .progress
            .grid
            .as_ref()
            .map(|grid| grid.grid)
            .unwrap_or_else(|| self.options.resolved_region_grid());
        let target_rect = target_region_rect(rect, self.options.scale, selected_grid);
        if let Some(texture) = &self.map_overlay.status_texture {
            painter.image(texture.id(), target_rect, uv, egui::Color32::WHITE);
        }
        draw_rect_outline(
            &painter,
            target_rect,
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 190),
        );
    }

    fn update_status_texture(&mut self, ctx: &egui::Context) {
        let Some(progress_grid) = self.progress.grid.as_ref() else {
            return;
        };
        let now = Instant::now();
        if !self.map_overlay.status_dirty
            || self
                .map_overlay
                .last_status_upload
                .is_some_and(|last| now.duration_since(last) < STATUS_TEXTURE_UPLOAD_INTERVAL)
        {
            return;
        }
        let (width, height, scale) = status_texture_layout(progress_grid.grid);
        let pixels = status_overlay_pixels(progress_grid, width, height, scale);
        let image = egui::ColorImage::new([width, height], pixels.clone());
        if self.map_overlay.status_texture_size != [width, height] {
            self.map_overlay.status_texture = None;
            self.map_overlay.status_texture_size = [width, height];
        }
        if let Some(texture) = &mut self.map_overlay.status_texture {
            texture.set(image, egui::TextureOptions::NEAREST);
        } else {
            self.map_overlay.status_texture = Some(ctx.load_texture(
                "earthmap-region-status",
                image,
                egui::TextureOptions::NEAREST,
            ));
        }
        self.map_overlay.status_pixels = pixels;
        self.map_overlay.status_dirty = false;
        self.map_overlay.last_status_upload = Some(now);
    }
}

impl eframe::App for EarthMapGuiApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(encoded) = serde_json::to_string(&self.options) {
            storage.set_string(GUI_OPTIONS_STORAGE_KEY, encoded);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_worker_events();
        let ctx = ui.ctx().clone();
        if self.run.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::Panel::top("top_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("EarthMap Generator");
                ui.separator();
                let validation_error = self.options.validation_error();
                if ui
                    .add_enabled(
                        self.run.is_none() && validation_error.is_none(),
                        egui::Button::new("Start"),
                    )
                    .clicked()
                {
                    self.start_generation();
                }
                if ui
                    .add_enabled(self.run.is_some(), egui::Button::new("Stop"))
                    .clicked()
                {
                    self.stop_generation();
                }
                ui.separator();
                ui.checkbox(&mut self.map_overlay.enabled, "Map");
                if let Some(error) = validation_error {
                    ui.small(error);
                }
            });
        });

        egui::Panel::left("options_panel")
            .resizable(true)
            .default_size(430.0)
            .show_inside(ui, |ui| {
                ui.heading("Data");
                ui.separator();
                ui.label("HeightMap GeoTIFF");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.options.heightmap_path);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("GeoTIFF", &["tif", "tiff"])
                            .pick_file()
                        {
                            self.options.heightmap_path = path.display().to_string();
                        }
                    }
                });
                ui.small("Sets terrain height, coast shape, water/land, and ocean depth.");
                ui.small(path_status("HeightMap", &self.options.heightmap_path, true));
                ui.label("TifFiles root");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.options.tif_root);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.options.tif_root = path.display().to_string();
                            self.options.apply_tif_root();
                        }
                    }
                });
                ui.small("Expected: terrain/TrueMarble.vrt, climate.tif, vegetation/*.tif, bathymetry.tif, slope.tif.");
                ui.small(path_status("TifFiles root", &self.options.tif_root, false));
                ui.label("Satellite raster");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.options.true_marble_path);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("VRT", &["vrt"])
                            .pick_file()
                        {
                            self.options.true_marble_path = path.display().to_string();
                        }
                    }
                });
                ui.small("TrueMarble.vrt drives photo-like surface color and material selection.");
                if self.options.true_marble_path.trim().is_empty() {
                    ui.small("Satellite raster status: auto-detect at generation time.");
                } else {
                    ui.small(path_status(
                        "Satellite raster",
                        &self.options.true_marble_path,
                        true,
                    ));
                }
                ui.horizontal(|ui| {
                    if ui.button("Environment defaults").clicked() {
                        self.options.apply_environment_defaults();
                    }
                    if ui.button("Use auto raster").clicked() {
                        self.options.true_marble_path.clear();
                    }
                });

                ui.separator();
                ui.heading("World");
                ui.label("World directory");
                let mut world_dir_changed = false;
                ui.horizontal(|ui| {
                    let response = ui.text_edit_singleline(&mut self.options.world_dir);
                    world_dir_changed |= response.changed();
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.options.world_dir = path.display().to_string();
                            world_dir_changed = true;
                        }
                    }
                });
                if world_dir_changed {
                    self.loaded_project_world_dir.clear();
                    self.apply_existing_project_settings_for_world_dir();
                }
                if let Some(status) = &self.project_settings_status {
                    ui.small(status);
                }
                ui.horizontal(|ui| {
                    ui.label("Scale denominator");
                    ui.add(egui::DragValue::new(&mut self.options.scale).range(1..=100_000));
                });
                ui.small("1000 means about one Minecraft block per kilometer at the equator. Smaller numbers create larger worlds.");
                ui.horizontal(|ui| {
                    ui.label("Vertical scale");
                    ui.text_edit_singleline(&mut self.options.vertical_scale);
                });
                ui.small("auto follows map scale for detailed regional worlds while capping height within Minecraft limits.");

                ui.separator();
                ui.heading("Area");
                egui::ComboBox::from_label("Area mode")
                    .selected_text(self.options.extent_mode.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.options.extent_mode,
                            ExtentMode::WholeEarth,
                            ExtentMode::WholeEarth.label(),
                        );
                        ui.selectable_value(
                            &mut self.options.extent_mode,
                            ExtentMode::Preset,
                            ExtentMode::Preset.label(),
                        );
                        ui.selectable_value(
                            &mut self.options.extent_mode,
                            ExtentMode::Bounds,
                            ExtentMode::Bounds.label(),
                        );
                        ui.selectable_value(
                            &mut self.options.extent_mode,
                            ExtentMode::RegionGrid,
                            ExtentMode::RegionGrid.label(),
                        );
                    });
                match self.options.extent_mode {
                    ExtentMode::WholeEarth => {
                        ui.small("Covers the full available -180..180 longitude and -90..90 latitude extent.");
                    }
                    ExtentMode::Preset => {
                        egui::ComboBox::from_label("Preset")
                            .selected_text(self.options.preset.label())
                            .show_ui(ui, |ui| {
                                for preset in [
                                    AreaPreset::Australia,
                                    AreaPreset::Korea,
                                    AreaPreset::Europe,
                                    AreaPreset::Japan,
                                    AreaPreset::UnitedStates,
                                ] {
                                    ui.selectable_value(
                                        &mut self.options.preset,
                                        preset,
                                        preset.label(),
                                    );
                                }
                            });
                        self.options.bounds = self.options.preset.bounds();
                        ui.small("A safe starting point for people who do not know region coordinates.");
                    }
                    ExtentMode::Bounds => {
                        ui.horizontal(|ui| {
                            ui.label("West");
                            ui.add(egui::DragValue::new(
                                &mut self.options.bounds.west_longitude,
                            ));
                            ui.label("East");
                            ui.add(egui::DragValue::new(
                                &mut self.options.bounds.east_longitude,
                            ));
                        });
                        ui.horizontal(|ui| {
                            ui.label("North");
                            ui.add(egui::DragValue::new(
                                &mut self.options.bounds.north_latitude,
                            ));
                            ui.label("South");
                            ui.add(egui::DragValue::new(
                                &mut self.options.bounds.south_latitude,
                            ));
                        });
                        ui.small("Use decimal degrees. West/east are longitude, north/south are latitude.");
                    }
                    ExtentMode::RegionGrid => {
                        ui.small("Advanced mode. Use this only when you already know Minecraft region coordinates.");
                    }
                }
                let grid = self.options.resolved_region_grid();
                ui.label(format!(
                    "Resolved regions: start=({}, {}), size={} x {} ({} total)",
                    grid.start_region_x,
                    grid.start_region_z,
                    grid.cols,
                    grid.rows,
                    self.options.region_count()
                ));
                if self.options.extent_mode == ExtentMode::WholeEarth {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 130, 20),
                        "Whole Earth at 1:1000 is thousands of regions. Use Linear and a fast disk.",
                    );
                }
                if self.options.extent_mode == ExtentMode::RegionGrid {
                    ui.horizontal(|ui| {
                        ui.label("Start X");
                        ui.add(egui::DragValue::new(&mut self.options.start_region_x));
                        ui.label("Start Z");
                        ui.add(egui::DragValue::new(&mut self.options.start_region_z));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Cols");
                        ui.add(egui::DragValue::new(&mut self.options.cols).range(1..=512));
                        ui.label("Rows");
                        ui.add(egui::DragValue::new(&mut self.options.rows).range(1..=512));
                    });
                }

                ui.separator();
                ui.heading("Generation");
                ui.horizontal(|ui| {
                    ui.label("Threads");
                    ui.add(egui::DragValue::new(&mut self.options.threads).range(1..=256));
                });
                ui.horizontal(|ui| {
                    ui.label("Shard processes");
                    ui.add(
                        egui::DragValue::new(&mut self.options.shard_processes).range(1..=64),
                    );
                });
                let effective_shards = effective_shard_processes(&self.options);
                if effective_shards > 1 {
                    ui.small(format!(
                        "Runs {effective_shards} earthmap-rs processes over column shards. Each shard uses the same Threads, Rayon, and prefetch settings."
                    ));
                } else {
                    ui.small("1 keeps the normal single-process generator path.");
                }
                egui::ComboBox::from_label("Format")
                    .selected_text(self.options.format.as_cli_arg())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.options.format,
                            OutputFormatChoice::Linear,
                            "linear",
                        );
                        ui.selectable_value(
                            &mut self.options.format,
                            OutputFormatChoice::Mca,
                            "mca",
                        );
                    });
                egui::ComboBox::from_label("Chunk status")
                    .selected_text(self.options.status.as_cli_arg())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.options.status,
                            ChunkStatusChoice::Surface,
                            "surface",
                        );
                        ui.selectable_value(
                            &mut self.options.status,
                            ChunkStatusChoice::Carvers,
                            "carvers",
                        );
                    });
                ui.small("Linear is recommended for fast inspection. MCA is the classic Minecraft region format.");
                match self.options.format {
                    OutputFormatChoice::Linear => {
                        ui.horizontal(|ui| {
                            ui.label("Linear compression");
                            ui.add(
                                egui::DragValue::new(
                                    &mut self.options.linear_compression_level,
                                )
                                .range(1..=22),
                            );
                        });
                        ui.small("zstd level. 1 is fastest, 22 is smallest. Default is 4.");
                    }
                    OutputFormatChoice::Mca => {
                        ui.horizontal(|ui| {
                            ui.label("MCA compression");
                            ui.add(
                                egui::DragValue::new(&mut self.options.mca_compression_level)
                                    .range(0..=9),
                            );
                        });
                        ui.small("zlib level. 0 is fastest, 9 is smallest. Default is 6.");
                    }
                }
                ui.horizontal(|ui| {
                    ui.label("Height cache rows");
                    ui.text_edit_singleline(&mut self.options.cache_rows);
                });
                ui.horizontal(|ui| {
                    ui.label("Surface tile cache");
                    ui.text_edit_singleline(&mut self.options.surface_tile_cache_entries);
                });
                ui.small("Use auto to tune cache sizes from system memory and available companion rasters.");
                ui.horizontal(|ui| {
                    ui.label("Rayon threads");
                    ui.text_edit_singleline(&mut self.options.rayon_threads);
                });
                ui.small("Leave Rayon threads empty to use the tuned default.");
                ui.checkbox(&mut self.options.worker_tuning_enabled, "Worker autotune");
                ui.small("When disabled, Threads is used directly for region workers within safe system limits.");
                ui.checkbox(&mut self.options.prefetch_enabled, "Surface prefetch");
                if self.options.prefetch_enabled {
                    ui.horizontal(|ui| {
                        ui.label("Prefetch memory GB");
                        ui.text_edit_singleline(&mut self.options.prefetch_memory_gb);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Prefetch queue regions");
                        ui.text_edit_singleline(&mut self.options.prefetch_queue_regions);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Prefetch workers");
                        ui.text_edit_singleline(&mut self.options.prefetch_workers);
                    });
                    ui.small("Use auto for adaptive memory and queue sizing. Whole-Earth runs usually benefit from prefetch.");
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Progress");
            ui.add(
                egui::ProgressBar::new(self.progress.progress_fraction()).text(format!(
                    "{} / {} regions",
                    self.progress.completed_regions, self.progress.total_regions
                )),
            );
            let live_regions_per_hour = self.live_regions_per_hour();
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Elapsed: {:.1}s",
                    self.elapsed_for_display().as_secs_f64()
                ));
                ui.separator();
                ui.label(format!("Speed: {:.2} regions/hour", live_regions_per_hour));
                ui.separator();
                let remaining = self
                    .remaining_for_display(live_regions_per_hour)
                    .map(format_duration_compact)
                    .unwrap_or_else(|| "calculating".to_string());
                ui.label(format!("Remaining: {remaining}"));
            });
            if let Some(tuning_text) = self.worker_tuning_text() {
                ui.label(tuning_text);
            }
            if !self.progress.last_region.is_empty() {
                ui.label(format!("Last region: {}", self.progress.last_region));
            }
            if self.progress.failed_regions > 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 30, 30),
                    format!("Failed regions: {}", self.progress.failed_regions),
                );
            }
            if let Some(matched) = self.progress.resume_fingerprint_matched {
                ui.label(format!(
                    "Resume: {} ({} journal regions)",
                    if matched { "matched" } else { "fresh" },
                    self.progress.resume_journal_regions
                ));
            }
            if let Some(code) = self.progress.exit_code {
                ui.label(format!("Exit code: {code}"));
            }
            if let Some(error) = &self.progress.failed {
                ui.colored_label(egui::Color32::from_rgb(180, 30, 30), error);
            }
            ui.separator();
            if self.map_overlay.enabled {
                self.show_map_overlay(ui);
                ui.separator();
            }
            ui.heading("Resolved Command");
            let commands = build_generation_commands(&self.options);
            if commands.len() <= 1 {
                let args = commands
                    .first()
                    .map(|command| command.args.clone())
                    .unwrap_or_else(|| build_generation_args(&self.options));
                ui.monospace(format!("earthmap-rs {}", args.join(" ")));
            } else {
                ui.monospace(format!("{} shard processes", commands.len()));
                for command in commands.iter().take(8) {
                    ui.monospace(format!(
                        "{}earthmap-rs {}",
                        process_log_prefix(command.tag),
                        command.args.join(" ")
                    ));
                }
                if commands.len() > 8 {
                    ui.monospace(format!("... {} more shards", commands.len() - 8));
                }
            }
            let env_preview = generation_env_preview(&self.options);
            if !env_preview.is_empty() {
                ui.heading("Resolved Environment");
                for (name, value) in env_preview {
                    ui.monospace(format!("{name}={value}"));
                }
            }
            ui.separator();
            ui.heading("Log");
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log_lines {
                        ui.monospace(line);
                    }
                });
        });
    }
}

fn worker_tuning_display_text(result: &WorkerTuningResult) -> String {
    let worker_threads = result
        .selected_worker_threads
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let rayon_threads = result
        .selected_rayon_threads
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".to_string());
    let samples = result
        .sample_count
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".to_string());
    let mut text = format!(
        "Tuning: {} -> {} region workers / {} Rayon threads, {} samples",
        result.mode, worker_threads, rayon_threads, samples
    );
    if let (Some(land), Some(mixed), Some(ocean)) = (
        result.land_samples,
        result.mixed_samples,
        result.ocean_samples,
    ) {
        text.push_str(&format!(" ({land} land, {mixed} mixed, {ocean} ocean)"));
    }
    if let Some(message) = &result.message {
        if !message.is_empty() {
            text.push_str(&format!("; {message}"));
        }
    }
    text
}

fn worker_tuning_log_line(result: &WorkerTuningResult) -> String {
    worker_tuning_display_text(result)
}

fn format_duration_compact(duration: Duration) -> String {
    let mut seconds = duration.as_secs();
    let days = seconds / 86_400;
    seconds %= 86_400;
    let hours = seconds / 3_600;
    seconds %= 3_600;
    let minutes = seconds / 60;
    seconds %= 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn process_log_prefix(tag: Option<ProcessTag>) -> String {
    tag.map(|tag| format!("shard {}/{}: ", tag.index + 1, tag.total))
        .unwrap_or_default()
}

fn run_generation_processes(
    commands: Vec<GenerationCommand>,
    options: GenerationOptions,
    sender: Sender<WorkerEvent>,
    cancel: Arc<AtomicBool>,
    children_slot: Arc<Mutex<Vec<RunningChild>>>,
) {
    let started_at = Instant::now();
    let executable = earthmap_cli_executable();
    let command_count = commands.len().max(1);
    for command_spec in commands {
        if cancel.load(Ordering::SeqCst) {
            let _ = sender.send(WorkerEvent::Finished {
                code: None,
                elapsed: started_at.elapsed(),
            });
            return;
        }
        let mut command = Command::new(&executable);
        command
            .args(&command_spec.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_generation_env(&mut command, &options);

        let _ = sender.send(WorkerEvent::Line(format!(
            "{}starting earthmap-rs {}",
            process_log_prefix(command_spec.tag),
            command_spec.args.join(" ")
        )));

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                kill_running_children(&children_slot);
                let _ = sender.send(WorkerEvent::Failed(format!(
                    "failed to start {}: {error}",
                    executable.display()
                )));
                return;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if let Ok(mut guard) = children_slot.lock() {
            guard.push(RunningChild {
                tag: command_spec.tag,
                child,
                finished: false,
                exit_code: None,
            });
        }

        if let Some(stdout) = stdout {
            spawn_output_reader(stdout, sender.clone(), false, command_spec.tag);
        }
        if let Some(stderr) = stderr {
            spawn_output_reader(stderr, sender.clone(), true, command_spec.tag);
        }
    }

    loop {
        if cancel.load(Ordering::SeqCst) {
            kill_running_children(&children_slot);
        }
        let mut finished_events = Vec::new();
        let wait_result = match children_slot.lock() {
            Ok(mut guard) => {
                let mut all_finished = guard.len() >= command_count;
                for child in guard.iter_mut() {
                    if child.finished {
                        continue;
                    }
                    match child.child.try_wait() {
                        Ok(Some(status)) => {
                            child.finished = true;
                            child.exit_code = status.code();
                            finished_events.push((child.tag, status.code()));
                        }
                        Ok(None) => {
                            all_finished = false;
                        }
                        Err(error) => {
                            let _ = sender.send(WorkerEvent::Failed(format!(
                                "{}failed to wait for generator: {error}",
                                process_log_prefix(child.tag)
                            )));
                            return;
                        }
                    }
                }
                if guard.len() < command_count {
                    all_finished = false;
                }
                let exit_code = if all_finished {
                    guard
                        .iter()
                        .filter_map(|child| child.exit_code)
                        .find(|code| *code != 0)
                        .or(Some(0))
                } else {
                    None
                };
                Ok((all_finished, exit_code))
            }
            Err(_) => Err(()),
        };
        let (all_finished, exit_code) = match wait_result {
            Ok(value) => value,
            Err(_) => {
                let _ = sender.send(WorkerEvent::Failed(
                    "generator process state was poisoned".to_string(),
                ));
                return;
            }
        };
        for (tag, code) in finished_events {
            let _ = sender.send(WorkerEvent::Line(format!(
                "{}process finished with code {}",
                process_log_prefix(tag),
                code.map_or_else(|| "unknown".to_string(), |code| code.to_string())
            )));
        }
        if all_finished {
            let _ = sender.send(WorkerEvent::Finished {
                code: exit_code,
                elapsed: started_at.elapsed(),
            });
            if let Ok(mut guard) = children_slot.lock() {
                guard.clear();
            }
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn kill_running_children(children_slot: &Arc<Mutex<Vec<RunningChild>>>) {
    if let Ok(mut guard) = children_slot.lock() {
        for child in guard.iter_mut() {
            if !child.finished {
                let _ = child.child.kill();
            }
        }
    }
}

fn spawn_output_reader<R>(
    reader: R,
    sender: Sender<WorkerEvent>,
    is_stderr: bool,
    tag: Option<ProcessTag>,
) where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line_result in reader.lines() {
            let Ok(line) = line_result else {
                break;
            };
            if is_stderr {
                let _ = sender.send(WorkerEvent::Line(format!(
                    "{}stderr: {line}",
                    process_log_prefix(tag)
                )));
            } else {
                send_progress_events(&sender, &line, tag);
            }
        }
    });
}

fn send_progress_events(sender: &Sender<WorkerEvent>, line: &str, tag: Option<ProcessTag>) {
    if let Some(event) = parse_progress_event_line(line) {
        let _ = sender.send(WorkerEvent::Progress { tag, event });
        return;
    }
    if let Some((status, region_x, region_z, elapsed_millis)) = parse_region_line(line) {
        let _ = sender.send(WorkerEvent::RegionFinished {
            status,
            region_x,
            region_z,
            elapsed_millis,
        });
        return;
    }
    let _ = sender.send(WorkerEvent::Line(format!(
        "{}{line}",
        process_log_prefix(tag)
    )));
    if line.starts_with("elapsedMillis=") || line.starts_with("generatedRegionsPerHour=") {
        let elapsed_millis = line
            .strip_prefix("elapsedMillis=")
            .and_then(|value| value.trim().parse::<u64>().ok());
        let regions_per_hour = line
            .strip_prefix("generatedRegionsPerHour=")
            .and_then(|value| value.trim().parse::<f64>().ok());
        let _ = sender.send(WorkerEvent::Summary {
            elapsed_millis,
            regions_per_hour,
        });
    }
}

fn parse_progress_event_line(line: &str) -> Option<GeneratorProgressEvent> {
    let json = line.strip_prefix("event\t")?;
    let value = serde_json::from_str::<Value>(json).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "workerTuningStarted" => Some(GeneratorProgressEvent::WorkerTuningStarted {
            enabled: value
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            requested_threads: json_usize(&value, "requestedThreads"),
            submitted_regions: json_usize(&value, "submittedRegions"),
            max_samples: json_usize(&value, "maxSamples"),
        }),
        "workerTuningFinished" => Some(GeneratorProgressEvent::WorkerTuningFinished {
            result: parse_worker_tuning_result(value.get("workerTuning")?)?,
        }),
        "batchStarted" => {
            let start_region_x = json_i32(&value, "regionStartX")?;
            let start_region_z = json_i32(&value, "regionStartZ")?;
            let cols = json_i32(&value, "regionCols")?.max(1);
            let rows = json_i32(&value, "regionRows")?.max(1);
            let total_regions = value
                .get("regionCount")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_else(|| (cols as usize).saturating_mul(rows as usize));
            Some(GeneratorProgressEvent::BatchStarted {
                grid: RegionGrid {
                    start_region_x,
                    start_region_z,
                    cols,
                    rows,
                },
                total_regions,
                resume_fingerprint_matched: value
                    .get("resumeFingerprintMatched")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                resume_journal_regions: value
                    .get("resumeJournalRegions")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0),
            })
        }
        "regionStarted" => Some(GeneratorProgressEvent::RegionStarted {
            region_x: json_i32(&value, "regionX")?,
            region_z: json_i32(&value, "regionZ")?,
        }),
        "regionSkipped" => Some(GeneratorProgressEvent::RegionSkipped {
            region_x: json_i32(&value, "regionX")?,
            region_z: json_i32(&value, "regionZ")?,
            elapsed_millis: json_u64(&value, "elapsedMillis"),
        }),
        "regionGenerated" => Some(GeneratorProgressEvent::RegionGenerated {
            region_x: json_i32(&value, "regionX")?,
            region_z: json_i32(&value, "regionZ")?,
            elapsed_millis: json_u64(&value, "elapsedMillis"),
        }),
        "regionFailed" => Some(GeneratorProgressEvent::RegionFailed {
            region_x: json_i32(&value, "regionX")?,
            region_z: json_i32(&value, "regionZ")?,
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("region failed")
                .to_string(),
        }),
        "batchSummary" => Some(GeneratorProgressEvent::BatchSummary {
            elapsed_millis: json_u64(&value, "elapsedMillis"),
            regions_per_hour: value
                .get("completedRegionsPerHour")
                .or_else(|| value.get("generatedRegionsPerHour"))
                .and_then(Value::as_f64),
        }),
        _ => None,
    }
}

fn parse_worker_tuning_result(value: &Value) -> Option<WorkerTuningResult> {
    Some(WorkerTuningResult {
        mode: value.get("mode").and_then(Value::as_str)?.to_string(),
        selected_worker_threads: value
            .get("selectedRegionWorkerThreads")
            .or_else(|| value.get("selectedWorkerThreads"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        selected_rayon_threads: value
            .get("selectedRayonThreads")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        parallel_column_sampling: value.get("parallelColumnSampling").and_then(Value::as_bool),
        sample_count: value
            .get("sampleCount")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        land_samples: value
            .get("landSamples")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        ocean_samples: value
            .get("oceanSamples")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        mixed_samples: value
            .get("mixedSamples")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn json_i32(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn parse_region_line(line: &str) -> Option<(LegacyRegionStatus, i32, i32, Option<u64>)> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() < 8 || parts[0] != "region" {
        return None;
    }
    let status = match parts[1] {
        "generated" => LegacyRegionStatus::Generated,
        "skipped" => LegacyRegionStatus::Skipped,
        _ => return None,
    };
    let region_x = parts[2].parse::<i32>().ok()?;
    let region_z = parts[3].parse::<i32>().ok()?;
    let elapsed_millis = parts[4].parse::<u64>().ok();
    Some((status, region_x, region_z, elapsed_millis))
}

fn earthmap_cli_executable() -> PathBuf {
    let exe_name = if cfg!(windows) {
        "earthmap-rs.exe"
    } else {
        "earthmap-rs"
    };
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(directory) = current_exe.parent() {
            let sibling = directory.join(exe_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from(exe_name)
}

fn apply_generation_env(command: &mut Command, options: &GenerationOptions) {
    if !options.heightmap_path.trim().is_empty() {
        command.env(HEIGHTMAP_PATH_ENV, options.heightmap_path.trim());
    }
    if !options.world_dir.trim().is_empty() {
        command.env(OUTPUT_ROOT_ENV, options.world_dir.trim());
    }
    if !options.tif_root.trim().is_empty() {
        command.env(TIF_ROOT_ENV, options.tif_root.trim());
        let tif_root = Path::new(options.tif_root.trim());
        if let Some(parent) = tif_root.parent() {
            command.env(DATA_ROOT_ENV, parent);
        }
    }
    if !options.true_marble_path.trim().is_empty() && !options.true_marble_path.contains('=') {
        command.env(SURFACE_RASTER_ENV, options.true_marble_path.trim());
    }
    if !options.cache_rows.trim().is_empty() {
        command.env("EARTHMAP_HEIGHTMAP_CACHE_ROWS", options.cache_rows.trim());
    }
    if !options.surface_tile_cache_entries.trim().is_empty() {
        command.env(
            "EARTHMAP_SURFACE_TILE_CACHE_ENTRIES",
            options.surface_tile_cache_entries.trim(),
        );
    }
    if !options.rayon_threads.trim().is_empty() {
        command.env("RAYON_NUM_THREADS", options.rayon_threads.trim());
    }
}

fn generation_env_preview(options: &GenerationOptions) -> Vec<(String, String)> {
    let mut values = Vec::new();
    if !options.cache_rows.trim().is_empty() {
        values.push((
            "EARTHMAP_HEIGHTMAP_CACHE_ROWS".to_string(),
            options.cache_rows.trim().to_string(),
        ));
    }
    if !options.surface_tile_cache_entries.trim().is_empty() {
        values.push((
            "EARTHMAP_SURFACE_TILE_CACHE_ENTRIES".to_string(),
            options.surface_tile_cache_entries.trim().to_string(),
        ));
    }
    if !options.rayon_threads.trim().is_empty() {
        values.push((
            "RAYON_NUM_THREADS".to_string(),
            options.rayon_threads.trim().to_string(),
        ));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_generation_args_uses_parallel_surface_command() {
        let options = GenerationOptions {
            heightmap_path: "heightmap.tif".to_string(),
            tif_root: "TifFiles".to_string(),
            true_marble_path: true_marble_from_tif_root("TifFiles"),
            world_dir: "world".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[0], "generate-vanilla-delegated-regions-parallel");
        assert_eq!(args[3], "1000");
        assert_eq!(args[8], "linear");
        assert_eq!(args[10], "surface");
        assert_eq!(
            args[11],
            format!("surfaceRaster={}", true_marble_from_tif_root("TifFiles"))
        );
        assert_eq!(args[12], "verticalScale=auto");
        assert_eq!(args[13], "linearCompression=4");
    }

    #[test]
    fn surface_raster_plain_path_is_converted_to_cli_option() {
        let options = GenerationOptions {
            true_marble_path: "fixtures/TrueMarble.vrt".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], "surfaceRaster=fixtures/TrueMarble.vrt");
        assert_eq!(args[12], "verticalScale=auto");
    }

    #[test]
    fn generation_args_include_worker_tuning_toggle() {
        let options = GenerationOptions {
            worker_tuning_enabled: false,
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[14], "workerAutotune=false");
    }

    #[test]
    fn generation_args_include_prefetch_options_when_enabled() {
        let options = GenerationOptions {
            prefetch_enabled: true,
            prefetch_memory_gb: "25".to_string(),
            prefetch_queue_regions: "2".to_string(),
            prefetch_workers: "1".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[15], "prefetch=true");
        assert_eq!(args[16], "prefetchMemoryGB=25");
        assert_eq!(args[17], "prefetchRegions=2");
        assert_eq!(args[18], "prefetchWorkers=1");
    }

    #[test]
    fn generation_args_allow_auto_prefetch_without_memory_argument() {
        let options = GenerationOptions {
            prefetch_enabled: true,
            prefetch_memory_gb: "auto".to_string(),
            prefetch_queue_regions: "auto".to_string(),
            prefetch_workers: "auto".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[15], "prefetch=true");
        assert_eq!(args.len(), 16);
    }

    #[test]
    fn generation_commands_split_region_grid_into_column_shards() {
        let options = GenerationOptions {
            extent_mode: ExtentMode::RegionGrid,
            start_region_x: -10,
            start_region_z: 5,
            cols: 10,
            rows: 3,
            shard_processes: 4,
            ..GenerationOptions::default()
        };

        let commands = build_generation_commands(&options);

        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands
                .iter()
                .map(|command| command.grid)
                .collect::<Vec<_>>(),
            vec![
                RegionGrid {
                    start_region_x: -10,
                    start_region_z: 5,
                    cols: 3,
                    rows: 3
                },
                RegionGrid {
                    start_region_x: -7,
                    start_region_z: 5,
                    cols: 3,
                    rows: 3
                },
                RegionGrid {
                    start_region_x: -4,
                    start_region_z: 5,
                    cols: 2,
                    rows: 3
                },
                RegionGrid {
                    start_region_x: -2,
                    start_region_z: 5,
                    cols: 2,
                    rows: 3
                },
            ]
        );
        assert_eq!(commands[0].args[4], "-10");
        assert_eq!(commands[0].args[6], "3");
        assert_eq!(commands[3].args[4], "-2");
        assert_eq!(commands[3].args[6], "2");
        assert!(commands[0]
            .args
            .contains(&"projectGrid=-10,5,10,3".to_string()));
        assert!(commands[3]
            .args
            .contains(&"projectGrid=-10,5,10,3".to_string()));
    }

    #[test]
    fn generation_commands_cap_shards_to_region_columns() {
        let options = GenerationOptions {
            extent_mode: ExtentMode::RegionGrid,
            start_region_x: 3,
            start_region_z: 4,
            cols: 2,
            rows: 8,
            shard_processes: 8,
            ..GenerationOptions::default()
        };

        let commands = build_generation_commands(&options);

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].grid.cols, 1);
        assert_eq!(commands[1].grid.cols, 1);
    }

    #[test]
    fn generation_env_preview_includes_rayon_threads() {
        let options = GenerationOptions {
            rayon_threads: "13".to_string(),
            ..GenerationOptions::default()
        };
        let env = generation_env_preview(&options);
        assert!(env.contains(&("RAYON_NUM_THREADS".to_string(), "13".to_string())));
    }

    #[test]
    fn mca_generation_args_include_selected_compression_level() {
        let options = GenerationOptions {
            format: OutputFormatChoice::Mca,
            mca_compression_level: 9,
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[8], "mca");
        assert_eq!(args[13], "mcaCompression=9");
    }

    #[test]
    fn empty_true_marble_path_uses_auto_surface_raster() {
        let options = GenerationOptions {
            true_marble_path: String::new(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], "surfaceRaster=auto");
        assert_eq!(args[12], "verticalScale=auto");
    }

    #[test]
    fn generation_args_include_explicit_vertical_scale_when_selected() {
        let options = GenerationOptions {
            vertical_scale: "legacy".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[12], "verticalScale=legacy");
    }

    fn write_existing_parallel_project_fixture(root: &Path) -> (PathBuf, PathBuf) {
        let world = root.join("world");
        let heightmap = root.join("heightmap.tif");
        let true_marble = root.join("TifFiles").join("terrain").join("TrueMarble.vrt");
        std::fs::create_dir_all(true_marble.parent().unwrap()).unwrap();
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(&heightmap, b"heightmap").unwrap();
        std::fs::write(&true_marble, b"vrt").unwrap();
        std::fs::write(world.join("region").join("r.-157.-74.linear"), b"region").unwrap();
        std::fs::write(
            world.join(SURVIVAL_MANIFEST_FILE_NAME),
            format!(
                "# SR EarthMap survival manifest\n\
generator.name=vanilla-delegated-regions-parallel\n\
generation.format=LINEAR_V2\n\
generation.scaleDenominator=250\n\
generation.startRegionX=-157\n\
generation.startRegionZ=-74\n\
generation.regionCols=314\n\
generation.regionRows=148\n\
generation.chunkStatus=minecraft:surface\n\
generation.surfaceMaterialPath={}\n\
generation.verticalScale=4\n\
generation.verticalScaleMode=auto\n",
                true_marble.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let fingerprint = serde_json::json!({
            "schemaVersion": 2,
            "type": "fingerprint",
            "fingerprint": {
                "command": "generate-vanilla-delegated-regions-parallel",
                "format": "LINEAR_V2",
                "scale": 250,
                "startRegionX": -157,
                "startRegionZ": -74,
                "regionCols": 314,
                "regionRows": 148,
                "chunkStatus": "minecraft:surface",
                "verticalScale": 4.0,
                "verticalScaleMode": "auto",
                "heightmap": {
                    "path": heightmap.display().to_string()
                },
                "surfaceMaterial": {
                    "path": true_marble.display().to_string()
                },
                "compression": {
                    "linear": 6,
                    "mca": null
                }
            }
        });
        std::fs::write(
            world.join(VANILLA_DELEGATED_RESUME_JOURNAL_FILE_NAME),
            format!("{fingerprint}\n"),
        )
        .unwrap();
        (world, heightmap)
    }

    #[test]
    fn existing_project_settings_restore_resume_ready_command() {
        let temp = tempfile::tempdir().unwrap();
        let (world, heightmap) = write_existing_parallel_project_fixture(temp.path());
        let mut options = GenerationOptions {
            world_dir: world.display().to_string(),
            heightmap_path: "wrong-heightmap.tif".to_string(),
            scale: 1000,
            start_region_x: 0,
            start_region_z: 0,
            cols: 1,
            rows: 1,
            linear_compression_level: 4,
            ..GenerationOptions::default()
        };

        let report = apply_existing_project_settings(&mut options, &world)
            .unwrap()
            .unwrap();

        assert!(report.loaded_manifest);
        assert!(report.loaded_resume_fingerprint);
        assert_eq!(options.heightmap_path, heightmap.display().to_string());
        assert_eq!(
            options.tif_root,
            temp.path().join("TifFiles").display().to_string()
        );
        assert_eq!(options.scale, 250);
        assert_eq!(options.extent_mode, ExtentMode::RegionGrid);
        assert_eq!(options.start_region_x, -157);
        assert_eq!(options.start_region_z, -74);
        assert_eq!(options.cols, 314);
        assert_eq!(options.rows, 148);
        assert_eq!(options.format, OutputFormatChoice::Linear);
        assert_eq!(options.linear_compression_level, 6);
        assert_eq!(options.status, ChunkStatusChoice::Surface);
        assert_eq!(options.vertical_scale, "auto");

        let args = build_generation_args(&options);
        assert_eq!(args[1], heightmap.display().to_string());
        assert_eq!(args[2], world.display().to_string());
        assert_eq!(args[3], "250");
        assert_eq!(args[4], "-157");
        assert_eq!(args[5], "-74");
        assert_eq!(args[6], "314");
        assert_eq!(args[7], "148");
        assert_eq!(args[13], "linearCompression=6");
    }

    #[test]
    fn generation_options_persist_round_trip() {
        let options = GenerationOptions {
            heightmap_path: "C:\\data\\height.tif".to_string(),
            tif_root: "D:\\earthmap\\TifFiles".to_string(),
            true_marble_path: "D:\\earthmap\\TifFiles\\terrain\\TrueMarble.vrt".to_string(),
            world_dir: "D:\\earthmap\\1-250-earth-linear".to_string(),
            scale: 250,
            extent_mode: ExtentMode::WholeEarth,
            preset: AreaPreset::Korea,
            bounds: GeoBounds::new(124.0, 132.0, 43.5, 33.0),
            start_region_x: -157,
            start_region_z: -74,
            cols: 314,
            rows: 148,
            format: OutputFormatChoice::Linear,
            linear_compression_level: 6,
            mca_compression_level: 5,
            threads: 10,
            shard_processes: 4,
            status: ChunkStatusChoice::Surface,
            vertical_scale: "auto".to_string(),
            cache_rows: "auto".to_string(),
            surface_tile_cache_entries: "auto".to_string(),
            rayon_threads: "12".to_string(),
            worker_tuning_enabled: false,
            prefetch_enabled: true,
            prefetch_memory_gb: "25".to_string(),
            prefetch_queue_regions: "2".to_string(),
            prefetch_workers: "1".to_string(),
        };

        let encoded = serde_json::to_string(&options).unwrap();
        let decoded: GenerationOptions = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.world_dir, options.world_dir);
        assert_eq!(decoded.scale, 250);
        assert_eq!(decoded.extent_mode, ExtentMode::WholeEarth);
        assert_eq!(decoded.linear_compression_level, 6);
        assert_eq!(decoded.threads, 10);
        assert_eq!(decoded.shard_processes, 4);
        assert_eq!(decoded.rayon_threads, "12");
        assert!(!decoded.worker_tuning_enabled);
        assert!(decoded.prefetch_enabled);
        assert_eq!(decoded.prefetch_memory_gb, "25");
        assert_eq!(decoded.prefetch_queue_regions, "2");
        assert_eq!(decoded.prefetch_workers, "1");
    }

    #[test]
    fn generation_options_default_worker_tuning_when_loading_old_storage() {
        let json = r#"{
            "heightmap_path":"heightmap.tif",
            "tif_root":"TifFiles",
            "true_marble_path":"TifFiles/terrain/TrueMarble.vrt",
            "world_dir":"world",
            "scale":1000,
            "extent_mode":"Preset",
            "preset":"Australia",
            "bounds":{"west_longitude":112.0,"east_longitude":154.0,"north_latitude":-10.0,"south_latitude":-44.0},
            "start_region_x":26,
            "start_region_z":-10,
            "cols":3,
            "rows":3,
            "format":"Linear",
            "linear_compression_level":4,
            "mca_compression_level":6,
            "threads":8,
            "status":"Surface",
            "vertical_scale":"auto",
            "cache_rows":"auto",
            "surface_tile_cache_entries":"auto",
            "rayon_threads":""
        }"#;

        let decoded: GenerationOptions = serde_json::from_str(json).unwrap();

        assert!(decoded.worker_tuning_enabled);
        assert_eq!(decoded.shard_processes, 1);
        assert!(!decoded.prefetch_enabled);
        assert_eq!(decoded.prefetch_memory_gb, "auto");
        assert_eq!(decoded.prefetch_queue_regions, "auto");
        assert_eq!(decoded.prefetch_workers, "auto");
    }

    #[test]
    fn gui_world_dir_project_load_marks_existing_regions() {
        let temp = tempfile::tempdir().unwrap();
        let (world, _) = write_existing_parallel_project_fixture(temp.path());
        let mut app = EarthMapGuiApp {
            options: GenerationOptions {
                world_dir: world.display().to_string(),
                ..GenerationOptions::default()
            },
            ..EarthMapGuiApp::default()
        };

        app.apply_existing_project_settings_for_world_dir();

        assert_eq!(app.progress.total_regions, 314 * 148);
        assert_eq!(app.progress.completed_regions, 1);
        assert!(app
            .project_settings_status
            .as_deref()
            .is_some_and(|status| status.contains("1/46472 existing regions")));
    }

    #[test]
    fn startup_scan_filters_region_files_to_selected_grid() {
        let root = std::env::temp_dir().join(format!(
            "earthmap-gui-region-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let region = root.join("region");
        std::fs::create_dir_all(&region).unwrap();
        std::fs::write(region.join("r.10.20.linear"), b"linear").unwrap();
        std::fs::write(region.join("r.11.20.linear"), b"linear").unwrap();
        std::fs::write(region.join("r.12.20.linear"), b"outside").unwrap();
        std::fs::write(region.join("r.10.20.mca"), b"wrong format").unwrap();

        let mut scanned = scan_existing_region_files(
            &root,
            "linear",
            RegionGrid {
                start_region_x: 10,
                start_region_z: 20,
                cols: 2,
                rows: 1,
            },
        );
        scanned.sort();

        assert_eq!(scanned, vec![(10, 20), (11, 20)]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn region_progress_grid_does_not_double_count_completed_region() {
        let mut grid = RegionProgressGrid::new(RegionGrid {
            start_region_x: 5,
            start_region_z: -2,
            cols: 2,
            rows: 2,
        });

        assert!(grid.mark(5, -2, REGION_STATE_GENERATED));
        assert_eq!(grid.completed_regions, 1);
        assert!(!grid.mark(5, -2, REGION_STATE_GENERATED));
        assert_eq!(grid.completed_regions, 1);
        assert!(grid.mark(5, -2, REGION_STATE_RUNNING));
        assert_eq!(grid.completed_regions, 0);
        assert!(grid.mark(6, -1, REGION_STATE_FAILED));
        assert_eq!(grid.failed_regions, 1);
    }

    #[test]
    fn status_texture_layout_keeps_large_region_grids_capped() {
        assert_eq!(
            status_texture_layout(RegionGrid {
                start_region_x: -200,
                start_region_z: -100,
                cols: 400,
                rows: 200
            }),
            (400, 200, 1)
        );
        assert_eq!(
            status_texture_layout(RegionGrid {
                start_region_x: -400,
                start_region_z: -200,
                cols: 800,
                rows: 400
            }),
            (800, 400, 1)
        );
        let (width, height, scale) = status_texture_layout(RegionGrid {
            start_region_x: -1600,
            start_region_z: -800,
            cols: 3200,
            rows: 1600,
        });
        assert_eq!(scale, 2);
        assert_eq!((width, height), (1600, 800));
    }

    #[test]
    fn world_map_background_image_decodes_embedded_satellite_asset() {
        let image = world_map_background_image();

        assert_eq!(image.size, [2048, 1024]);
        assert_eq!(image.pixels.len(), 2048 * 1024);
    }

    #[test]
    fn status_overlay_pixels_aggregate_by_priority() {
        let mut grid = RegionProgressGrid::new(RegionGrid {
            start_region_x: 0,
            start_region_z: 0,
            cols: 4,
            rows: 4,
        });
        grid.mark(0, 0, REGION_STATE_GENERATED);
        grid.mark(1, 0, REGION_STATE_RUNNING);
        grid.mark(0, 1, REGION_STATE_FAILED);

        let pixels = status_overlay_pixels(&grid, 2, 2, 2);

        assert_eq!(pixels[0], region_state_color(REGION_STATE_FAILED));
    }

    #[test]
    fn status_overlay_pixels_handles_standard_large_grid_sizes() {
        for (cols, rows) in [(80, 40), (400, 200), (800, 400)] {
            let mut grid = RegionProgressGrid::new(RegionGrid {
                start_region_x: -(cols / 2),
                start_region_z: -(rows / 2),
                cols,
                rows,
            });
            for index in 0..grid.states.len() {
                grid.states[index] = match index % 17 {
                    0 => REGION_STATE_RUNNING,
                    1 => REGION_STATE_FAILED,
                    2..=8 => REGION_STATE_GENERATED,
                    _ => REGION_STATE_QUEUED,
                };
            }
            let (width, height, scale) = status_texture_layout(grid.grid);
            let started = Instant::now();
            let pixels = status_overlay_pixels(&grid, width, height, scale);
            let elapsed = started.elapsed();

            println!(
                "status overlay {}x{} regions -> {}x{} texture in {:.3}ms",
                cols,
                rows,
                width,
                height,
                elapsed.as_secs_f64() * 1000.0
            );
            assert_eq!(pixels.len(), width * height);
        }
    }

    #[test]
    fn whole_earth_scale_1000_resolves_centered_global_grid() {
        let grid = full_earth_region_grid(1000);
        assert_eq!(
            grid,
            RegionGrid {
                start_region_x: -40,
                start_region_z: -20,
                cols: 80,
                rows: 40
            }
        );
    }

    #[test]
    fn whole_earth_heightmap_extent_grid_matches_cli_describe_grid() {
        let mapping = EarthScaleMapping::for_denominator(250, -84.0, 84.0).unwrap();
        let grid = region_grid_for_mapping(&mapping);
        assert_eq!(
            grid,
            RegionGrid {
                start_region_x: -157,
                start_region_z: -74,
                cols: 314,
                rows: 148
            }
        );
    }

    #[test]
    fn preset_bounds_resolve_to_positive_region_count() {
        let grid = region_grid_for_bounds(1000, AreaPreset::Australia.bounds()).unwrap();
        assert!(grid.cols > 0);
        assert!(grid.rows > 0);
        assert!(grid.start_region_x > 0);
        assert!(grid.start_region_z >= 0);
    }

    #[test]
    fn parse_region_progress_line_extracts_region_and_elapsed() {
        let parsed = parse_region_line(
            "region,generated,27,-9,10977,1024,571110,fixtures/world/region/r.27.-9.linear,",
        );
        assert_eq!(
            parsed,
            Some((LegacyRegionStatus::Generated, 27, -9, Some(10977)))
        );
    }

    #[test]
    fn parse_json_progress_event_extracts_batch_and_region_updates() {
        let tuning_started = parse_progress_event_line(
            r#"event	{"schemaVersion":1,"type":"workerTuningStarted","enabled":true,"requestedThreads":8,"submittedRegions":100,"maxSamples":16}"#,
        )
        .unwrap();
        assert!(matches!(
            tuning_started,
            GeneratorProgressEvent::WorkerTuningStarted {
                enabled: true,
                requested_threads: Some(8),
                submitted_regions: Some(100),
                max_samples: Some(16)
            }
        ));

        let tuning_finished = parse_progress_event_line(
            r#"event	{"schemaVersion":1,"type":"workerTuningFinished","workerTuning":{"mode":"autotuned","selectedRegionWorkerThreads":4,"selectedRayonThreads":12,"parallelColumnSampling":true,"sampleCount":16,"landSamples":5,"mixedSamples":6,"oceanSamples":5,"message":null}}"#,
        )
        .unwrap();
        match tuning_finished {
            GeneratorProgressEvent::WorkerTuningFinished { result } => {
                assert_eq!(result.mode, "autotuned");
                assert_eq!(result.selected_worker_threads, Some(4));
                assert_eq!(result.selected_rayon_threads, Some(12));
                assert_eq!(result.sample_count, Some(16));
                assert_eq!(result.land_samples, Some(5));
                assert_eq!(result.mixed_samples, Some(6));
                assert_eq!(result.ocean_samples, Some(5));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let batch = parse_progress_event_line(
            r#"event	{"schemaVersion":1,"type":"batchStarted","regionStartX":-40,"regionStartZ":-20,"regionCols":80,"regionRows":40,"regionCount":3200,"resumeFingerprintMatched":true,"resumeJournalRegions":12}"#,
        )
        .unwrap();
        match batch {
            GeneratorProgressEvent::BatchStarted {
                grid,
                total_regions,
                resume_fingerprint_matched,
                resume_journal_regions,
            } => {
                assert_eq!(grid.start_region_x, -40);
                assert_eq!(grid.start_region_z, -20);
                assert_eq!(grid.cols, 80);
                assert_eq!(grid.rows, 40);
                assert_eq!(total_regions, 3200);
                assert!(resume_fingerprint_matched);
                assert_eq!(resume_journal_regions, 12);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let generated = parse_progress_event_line(
            r#"event	{"schemaVersion":1,"type":"regionGenerated","regionX":27,"regionZ":-9,"elapsedMillis":10977}"#,
        )
        .unwrap();
        assert!(matches!(
            generated,
            GeneratorProgressEvent::RegionGenerated {
                region_x: 27,
                region_z: -9,
                elapsed_millis: Some(10977)
            }
        ));
    }

    #[test]
    fn remaining_time_uses_live_regions_per_hour() {
        let mut app = EarthMapGuiApp::default();
        app.progress.total_regions = 100;
        app.progress.completed_regions = 25;
        app.progress.completed_events = 25;
        app.progress.regions_per_hour = Some(75.0);

        assert_eq!(app.remaining_regions(), 75);
        assert_eq!(
            app.remaining_for_display(75.0).map(format_duration_compact),
            Some("1h 0m".to_string())
        );
    }

    #[test]
    fn duration_format_is_compact() {
        assert_eq!(format_duration_compact(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration_compact(Duration::from_secs(125)), "2m 5s");
        assert_eq!(format_duration_compact(Duration::from_secs(3_900)), "1h 5m");
        assert_eq!(
            format_duration_compact(Duration::from_secs(90_000)),
            "1d 1h"
        );
    }
}
