#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use earthmap_geo::EarthScaleMapping;
use eframe::egui;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DEFAULT_HEIGHTMAP_PATH: &str = r"C:\earth_map_resources\HQheightmap.tif";
const DEFAULT_WORLD_DIR: &str = r"D:\earthmap\gui-world";
const DEFAULT_TIF_ROOT: &str = r"D:\earthmap\TifFiles";
const DEFAULT_CACHE_ROWS: &str = "1024";
const DEFAULT_SURFACE_TILE_CACHE_ENTRIES: &str = "512";
const REGION_SIZE_BLOCKS: i32 = 512;
const FULL_EARTH_MIN_LATITUDE: f64 = -90.0;
const FULL_EARTH_MAX_LATITUDE: f64 = 90.0;

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

#[derive(Debug)]
enum WorkerEvent {
    Line(String),
    RegionFinished {
        region_x: String,
        region_z: String,
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
    last_region: String,
    elapsed_millis: Option<u64>,
    regions_per_hour: Option<f64>,
    exit_code: Option<i32>,
    running: bool,
    failed: Option<String>,
}

impl ProgressState {
    fn reset(&mut self, total_regions: usize) {
        *self = Self {
            total_regions,
            running: true,
            ..Default::default()
        };
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
    run: Option<GenerationRun>,
    log_lines: Vec<String>,
}

impl Default for EarthMapGuiApp {
    fn default() -> Self {
        Self {
            options: GenerationOptions::default(),
            progress: ProgressState::default(),
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
        self.progress.reset(self.options.region_count());
        self.log_lines.clear();
        let (sender, receiver) = unbounded();
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
                WorkerEvent::RegionFinished {
                    region_x,
                    region_z,
                    elapsed_millis,
                } => {
                    self.progress.completed_regions =
                        self.progress.completed_regions.saturating_add(1);
                    self.progress.last_region = format!("r.{region_x}.{region_z}");
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
                ui.horizontal(|ui| {
                    ui.label("Height cache rows");
                    ui.text_edit_singleline(&mut self.options.cache_rows);
                });
                ui.horizontal(|ui| {
                    ui.label("Surface tile cache");
                    ui.text_edit_singleline(&mut self.options.surface_tile_cache_entries);
                });
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
            if let Some(code) = self.progress.exit_code {
                ui.label(format!("Exit code: {code}"));
            }
            if let Some(error) = &self.progress.failed {
                ui.colored_label(egui::Color32::from_rgb(180, 30, 30), error);
            }
            ui.separator();
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
    let _ = sender.send(WorkerEvent::Line(line.to_string()));
    if let Some((region_x, region_z, elapsed_millis)) = parse_region_line(line) {
        let _ = sender.send(WorkerEvent::RegionFinished {
            region_x,
            region_z,
            elapsed_millis,
        });
    }
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

fn parse_region_line(line: &str) -> Option<(String, String, Option<u64>)> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() < 8 || parts[0] != "region" {
        return None;
    }
    let status = parts[1];
    if status != "generated" && status != "skipped" {
        return None;
    }
    let elapsed_millis = parts[4].parse::<u64>().ok();
    Some((parts[2].to_string(), parts[3].to_string(), elapsed_millis))
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
    fn empty_true_marble_path_uses_auto_surface_raster() {
        let options = GenerationOptions {
            true_marble_path: String::new(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], "surfaceRaster=auto");
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
            Some(("27".to_string(), "-9".to_string(), Some(10977)))
        );
    }
}
