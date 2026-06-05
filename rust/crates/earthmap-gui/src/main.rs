#![forbid(unsafe_code)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DEFAULT_HEIGHTMAP_PATH: &str = r"C:\earth_map_resources\HQheightmap.tif";
const DEFAULT_WORLD_DIR: &str = r"D:\earthmap\gui-world";
const DEFAULT_SURFACE_RASTER: &str = "surfaceRaster=auto";
const DEFAULT_CACHE_ROWS: &str = "1024";
const DEFAULT_SURFACE_TILE_CACHE_ENTRIES: &str = "512";

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

#[derive(Clone, Debug)]
struct GenerationOptions {
    heightmap_path: String,
    world_dir: String,
    scale: i32,
    start_region_x: i32,
    start_region_z: i32,
    cols: i32,
    rows: i32,
    format: OutputFormatChoice,
    threads: usize,
    status: ChunkStatusChoice,
    surface_raster: String,
    cache_rows: String,
    surface_tile_cache_entries: String,
    rayon_threads: String,
}

impl Default for GenerationOptions {
    fn default() -> Self {
        Self {
            heightmap_path: DEFAULT_HEIGHTMAP_PATH.to_string(),
            world_dir: DEFAULT_WORLD_DIR.to_string(),
            scale: 1000,
            start_region_x: 26,
            start_region_z: -10,
            cols: 3,
            rows: 3,
            format: OutputFormatChoice::Linear,
            threads: 8,
            status: ChunkStatusChoice::Surface,
            surface_raster: DEFAULT_SURFACE_RASTER.to_string(),
            cache_rows: DEFAULT_CACHE_ROWS.to_string(),
            surface_tile_cache_entries: DEFAULT_SURFACE_TILE_CACHE_ENTRIES.to_string(),
            rayon_threads: String::new(),
        }
    }
}

impl GenerationOptions {
    fn region_count(&self) -> usize {
        let cols = self.cols.max(1) as usize;
        let rows = self.rows.max(1) as usize;
        cols.saturating_mul(rows)
    }

    fn normalized_surface_raster(&self) -> String {
        let trimmed = self.surface_raster.trim();
        if trimmed.is_empty() {
            DEFAULT_SURFACE_RASTER.to_string()
        } else if trimmed.contains('=') {
            trimmed.to_string()
        } else {
            format!("surfaceRaster={trimmed}")
        }
    }
}

fn build_generation_args(options: &GenerationOptions) -> Vec<String> {
    vec![
        "generate-vanilla-delegated-regions-parallel".to_string(),
        options.heightmap_path.clone(),
        options.world_dir.clone(),
        options.scale.max(1).to_string(),
        options.start_region_x.to_string(),
        options.start_region_z.to_string(),
        options.cols.max(1).to_string(),
        options.rows.max(1).to_string(),
        options.format.as_cli_arg().to_string(),
        options.threads.max(1).to_string(),
        options.status.as_cli_arg().to_string(),
        options.normalized_surface_raster(),
    ]
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
            .default_size(360.0)
            .show_inside(ui, |ui| {
                ui.heading("Options");
                ui.separator();
                ui.label("Heightmap");
                ui.text_edit_singleline(&mut self.options.heightmap_path);
                ui.label("World directory");
                ui.text_edit_singleline(&mut self.options.world_dir);
                ui.horizontal(|ui| {
                    ui.label("Scale");
                    ui.add(egui::DragValue::new(&mut self.options.scale).range(1..=100_000));
                });
                ui.horizontal(|ui| {
                    ui.label("Start X");
                    ui.add(egui::DragValue::new(&mut self.options.start_region_x));
                    ui.label("Start Z");
                    ui.add(egui::DragValue::new(&mut self.options.start_region_z));
                });
                ui.horizontal(|ui| {
                    ui.label("Cols");
                    ui.add(egui::DragValue::new(&mut self.options.cols).range(1..=256));
                    ui.label("Rows");
                    ui.add(egui::DragValue::new(&mut self.options.rows).range(1..=256));
                });
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
                ui.label("Surface raster");
                ui.text_edit_singleline(&mut self.options.surface_raster);
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
        assert_eq!(args[11], "surfaceRaster=auto");
    }

    #[test]
    fn surface_raster_plain_path_is_converted_to_cli_option() {
        let options = GenerationOptions {
            surface_raster: r"D:\earthmap\TrueMarble.vrt".to_string(),
            ..GenerationOptions::default()
        };
        let args = build_generation_args(&options);
        assert_eq!(args[11], r"surfaceRaster=D:\earthmap\TrueMarble.vrt");
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
