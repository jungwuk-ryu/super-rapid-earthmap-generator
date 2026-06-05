#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use earthmap_geo::EarthScaleMapping;
use eframe::egui;
use serde_json::Value;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DEFAULT_HEIGHTMAP_PATH: &str = r"C:\earth_map_resources\HQheightmap.tif";
const DEFAULT_WORLD_DIR: &str = r"D:\earthmap\gui-world";
const DEFAULT_TIF_ROOT: &str = r"D:\earthmap\TifFiles";
const DEFAULT_CACHE_ROWS: &str = "auto";
const DEFAULT_SURFACE_TILE_CACHE_ENTRIES: &str = "auto";
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
        Box::new(|_| Ok(Box::new(EarthMapGuiApp::default()))),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RegionGrid {
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
}

#[derive(Clone, Debug)]
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
    status: ChunkStatusChoice,
    cache_rows: String,
    surface_tile_cache_entries: String,
    rayon_threads: String,
}

impl Default for GenerationOptions {
    fn default() -> Self {
        Self {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH.to_string(),
            tif_root: DEFAULT_TIF_ROOT.to_string(),
            true_marble_path: true_marble_from_tif_root(DEFAULT_TIF_ROOT),
            world_dir: DEFAULT_WORLD_DIR.to_string(),
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
            status: ChunkStatusChoice::Surface,
            cache_rows: DEFAULT_CACHE_ROWS.to_string(),
            surface_tile_cache_entries: DEFAULT_SURFACE_TILE_CACHE_ENTRIES.to_string(),
            rayon_threads: String::new(),
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

    fn resolved_region_grid(&self) -> RegionGrid {
        match self.extent_mode {
            ExtentMode::WholeEarth => full_earth_region_grid(self.scale),
            ExtentMode::Preset => region_grid_for_bounds(self.scale, self.preset.bounds())
                .unwrap_or_else(|| manual_region_grid(self)),
            ExtentMode::Bounds => region_grid_for_bounds(self.scale, self.bounds)
                .unwrap_or_else(|| manual_region_grid(self)),
            ExtentMode::RegionGrid => manual_region_grid(self),
        }
    }

    fn apply_local_defaults(&mut self) {
        self.heightmap_path = DEFAULT_HEIGHTMAP_PATH.to_string();
        self.tif_root = DEFAULT_TIF_ROOT.to_string();
        self.true_marble_path = true_marble_from_tif_root(DEFAULT_TIF_ROOT);
    }

    fn apply_tif_root(&mut self) {
        self.true_marble_path = true_marble_from_tif_root(&self.tif_root);
    }
}

fn build_generation_args(options: &GenerationOptions) -> Vec<String> {
    let grid = options.resolved_region_grid();
    vec![
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
        options.compression_option(),
    ]
}

fn manual_region_grid(options: &GenerationOptions) -> RegionGrid {
    RegionGrid {
        start_region_x: options.start_region_x,
        start_region_z: options.start_region_z,
        cols: options.cols.max(1),
        rows: options.rows.max(1),
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyRegionStatus {
    Generated,
    Skipped,
}

#[derive(Debug)]
enum WorkerEvent {
    Line(String),
    Progress(GeneratorProgressEvent),
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
    child: Arc<Mutex<Option<std::process::Child>>>,
    started_at: Instant,
}

impl GenerationRun {
    fn request_stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

#[derive(Default)]
struct ProgressState {
    completed_regions: usize,
    total_regions: usize,
    failed_regions: usize,
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
        self.recent_completions.push_back(now);
        while self
            .recent_completions
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > ROLLING_SPEED_WINDOW)
        {
            self.recent_completions.pop_front();
        }
        let window_secs = self
            .recent_completions
            .front()
            .map(|instant| now.duration_since(*instant).as_secs_f64().max(1.0))
            .unwrap_or(1.0);
        self.rolling_regions_per_hour =
            Some((self.recent_completions.len() as f64) * 3600.0 / window_secs);
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
}

impl Default for EarthMapGuiApp {
    fn default() -> Self {
        Self {
            options: GenerationOptions::default(),
            progress: ProgressState::default(),
            map_overlay: MapOverlayState::default(),
            run: None,
            log_lines: Vec::new(),
        }
    }
}

impl EarthMapGuiApp {
    fn start_generation(&mut self) {
        if self.run.is_some() {
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
        let child = Arc::new(Mutex::new(None));
        let args = build_generation_args(&self.options);
        let options = self.options.clone();
        let cancel_for_thread = Arc::clone(&cancel);
        let child_for_thread = Arc::clone(&child);
        thread::spawn(move || {
            run_generation_process(args, options, sender, cancel_for_thread, child_for_thread);
        });
        self.run = Some(GenerationRun {
            receiver,
            cancel,
            child,
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
                WorkerEvent::Progress(event) => self.apply_progress_event(event),
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

    fn apply_progress_event(&mut self, event: GeneratorProgressEvent) {
        match event {
            GeneratorProgressEvent::BatchStarted {
                grid,
                total_regions,
                resume_fingerprint_matched,
                resume_journal_regions,
            } => {
                self.progress.reset_from_batch(grid, total_regions);
                self.progress.resume_fingerprint_matched = Some(resume_fingerprint_matched);
                self.progress.resume_journal_regions = resume_journal_regions;
                self.map_overlay.status_dirty = true;
                self.push_log_line(format!(
                    "batch started: {} regions, resume match={}, journal regions={}",
                    total_regions, resume_fingerprint_matched, resume_journal_regions
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
                self.progress.elapsed_millis = elapsed_millis;
                self.progress.regions_per_hour = regions_per_hour;
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

    fn live_regions_per_hour(&self) -> f64 {
        if self.run.is_some() {
            if let Some(rolling_regions_per_hour) = self.progress.rolling_regions_per_hour {
                return rolling_regions_per_hour;
            }
        }
        if let Some(regions_per_hour) = self.progress.regions_per_hour {
            return regions_per_hour;
        }
        let elapsed_hours = self.elapsed_for_display().as_secs_f64() / 3600.0;
        if elapsed_hours <= 0.0 {
            0.0
        } else {
            self.progress.completed_regions as f64 / elapsed_hours
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
                if ui
                    .add_enabled(self.run.is_none(), egui::Button::new("Start"))
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
                ui.small("Expected: terrain\\TrueMarble.vrt, climate.tif, vegetation\\*.tif, bathymetry.tif, slope.tif.");
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
                    if ui.button("Local defaults").clicked() {
                        self.options.apply_local_defaults();
                    }
                    if ui.button("Use auto raster").clicked() {
                        self.options.true_marble_path.clear();
                    }
                });

                ui.separator();
                ui.heading("World");
                ui.label("World directory");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.options.world_dir);
                    if ui.button("Browse").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.options.world_dir = path.display().to_string();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Scale denominator");
                    ui.add(egui::DragValue::new(&mut self.options.scale).range(1..=100_000));
                });
                ui.small("1000 means about one Minecraft block per kilometer at the equator. Smaller numbers create larger worlds.");

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
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Progress");
            ui.add(
                egui::ProgressBar::new(self.progress.progress_fraction()).text(format!(
                    "{} / {} regions",
                    self.progress.completed_regions, self.progress.total_regions
                )),
            );
            ui.horizontal(|ui| {
                ui.label(format!(
                    "Elapsed: {:.1}s",
                    self.elapsed_for_display().as_secs_f64()
                ));
                ui.separator();
                ui.label(format!(
                    "Speed: {:.2} regions/hour",
                    self.live_regions_per_hour()
                ));
            });
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
            let args = build_generation_args(&self.options);
            ui.monospace(format!("earthmap-rs {}", args.join(" ")));
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

fn run_generation_process(
    args: Vec<String>,
    options: GenerationOptions,
    sender: Sender<WorkerEvent>,
    cancel: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<std::process::Child>>>,
) {
    let started_at = Instant::now();
    let executable = earthmap_cli_executable();
    let mut command = Command::new(&executable);
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_generation_env(&mut command, &options);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.send(WorkerEvent::Failed(format!(
                "failed to start {}: {error}",
                executable.display()
            )));
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Ok(mut guard) = child_slot.lock() {
        *guard = Some(child);
    }

    if let Some(stdout) = stdout {
        spawn_output_reader(stdout, sender.clone(), false);
    }
    if let Some(stderr) = stderr {
        spawn_output_reader(stderr, sender.clone(), true);
    }

    loop {
        if cancel.load(Ordering::SeqCst) {
            if let Ok(mut guard) = child_slot.lock() {
                if let Some(child) = guard.as_mut() {
                    let _ = child.kill();
                }
            }
        }
        let status = match child_slot.lock() {
            Ok(mut guard) => match guard.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(status) => status,
                    Err(error) => {
                        let _ = sender.send(WorkerEvent::Failed(format!(
                            "failed to wait for generator: {error}"
                        )));
                        return;
                    }
                },
                None => None,
            },
            Err(_) => {
                let _ = sender.send(WorkerEvent::Failed(
                    "generator process state was poisoned".to_string(),
                ));
                return;
            }
        };
        if let Some(status) = status {
            let _ = sender.send(WorkerEvent::Finished {
                code: status.code(),
                elapsed: started_at.elapsed(),
            });
            if let Ok(mut guard) = child_slot.lock() {
                *guard = None;
            }
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_output_reader<R>(reader: R, sender: Sender<WorkerEvent>, is_stderr: bool)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line_result in reader.lines() {
            let Ok(line) = line_result else {
                break;
            };
            let line = if is_stderr {
                format!("stderr: {line}")
            } else {
                line
            };
            send_progress_events(&sender, &line);
        }
    });
}

fn send_progress_events(sender: &Sender<WorkerEvent>, line: &str) {
    if let Some(event) = parse_progress_event_line(line) {
        let _ = sender.send(WorkerEvent::Progress(event));
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
    let _ = sender.send(WorkerEvent::Line(line.to_string()));
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

fn json_i32(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_generation_args_uses_parallel_surface_command() {
        let options = GenerationOptions::default();
        let args = build_generation_args(&options);
        assert_eq!(args[0], "generate-vanilla-delegated-regions-parallel");
        assert_eq!(args[3], "1000");
        assert_eq!(args[8], "linear");
        assert_eq!(args[10], "surface");
        assert_eq!(
            args[11],
            r"surfaceRaster=D:\earthmap\TifFiles\terrain\TrueMarble.vrt"
        );
        assert_eq!(args[12], "linearCompression=4");
    }

    #[test]
    fn surface_raster_plain_path_is_converted_to_cli_option() {
        let options = GenerationOptions {
            true_marble_path: r"D:\earthmap\TrueMarble.vrt".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], r"surfaceRaster=D:\earthmap\TrueMarble.vrt");
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
        assert_eq!(args[12], "mcaCompression=9");
    }

    #[test]
    fn empty_true_marble_path_uses_auto_surface_raster() {
        let options = GenerationOptions {
            true_marble_path: String::new(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], "surfaceRaster=auto");
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
            r"region,generated,27,-9,10977,1024,571110,D:\world\region\r.27.-9.linear,",
        );
        assert_eq!(
            parsed,
            Some((LegacyRegionStatus::Generated, 27, -9, Some(10977)))
        );
    }

    #[test]
    fn parse_json_progress_event_extracts_batch_and_region_updates() {
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
}
