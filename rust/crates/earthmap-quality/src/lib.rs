#![forbid(unsafe_code)]

use image::{Rgb, RgbImage};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MODULE_STATUS: &str = "phase1-skeleton";

const HEATMAP_DELTA_E_CEILING: f64 = 45.0;
const TOP_ERROR_SAMPLE_COUNT: usize = 256;

pub type QualityResult<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub struct NamedMetrics {
    pub name: String,
    pub metrics: Metrics,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub output_directory: PathBuf,
    pub text: String,
    pub metrics: Vec<NamedMetrics>,
}

impl Report {
    pub fn metric(&self, name: &str) -> Option<&Metrics> {
        self.metrics
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| &entry.metrics)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Metrics {
    pub pixels: usize,
    pub mean_delta_e2000: f64,
    pub p95_delta_e2000: f64,
    pub max_delta_e2000: f64,
    pub mean_rgb_distance: f64,
    pub p95_rgb_distance: f64,
    pub max_rgb_distance: f64,
    pub exact_match_percent: f64,
    pub global_luma_ssim: f64,
    pub gradient_edge_pairs: usize,
    pub actual_mean_luma_gradient: f64,
    pub expected_mean_luma_gradient: f64,
    pub luma_gradient_retention: f64,
    pub mean_luma_gradient_error: f64,
    pub delta_e_over10: usize,
    pub delta_e_over15: usize,
    pub delta_e_over20: usize,
    pub delta_e_over30: usize,
    pub max_delta_e_x: i32,
    pub max_delta_e_y: i32,
}

impl Metrics {
    fn to_text(self) -> String {
        format!(
            "pixels={}\nmeanDeltaE2000={:.6}\np95DeltaE2000={:.6}\nmaxDeltaE2000={:.6}\n\
meanRgbDistance={:.6}\np95RgbDistance={:.6}\nmaxRgbDistance={:.6}\n\
exactMatchPercent={:.6}\nglobalLumaSsim={:.9}\n\
gradientEdgePairs={}\nactualMeanLumaGradient={:.6}\nexpectedMeanLumaGradient={:.6}\n\
lumaGradientRetention={:.6}\nmeanLumaGradientError={:.6}\n\
deltaEOver10={} ({:.6}%)\ndeltaEOver15={} ({:.6}%)\n\
deltaEOver20={} ({:.6}%)\ndeltaEOver30={} ({:.6}%)\nmaxDeltaEPixel={},{}\n",
            self.pixels,
            self.mean_delta_e2000,
            self.p95_delta_e2000,
            self.max_delta_e2000,
            self.mean_rgb_distance,
            self.p95_rgb_distance,
            self.max_rgb_distance,
            self.exact_match_percent,
            self.global_luma_ssim,
            self.gradient_edge_pairs,
            self.actual_mean_luma_gradient,
            self.expected_mean_luma_gradient,
            self.luma_gradient_retention,
            self.mean_luma_gradient_error,
            self.delta_e_over10,
            percent(self.delta_e_over10, self.pixels),
            self.delta_e_over15,
            percent(self.delta_e_over15, self.pixels),
            self.delta_e_over20,
            percent(self.delta_e_over20, self.pixels),
            self.delta_e_over30,
            percent(self.delta_e_over30, self.pixels),
            self.max_delta_e_x,
            self.max_delta_e_y
        )
    }
}

#[derive(Clone, Debug)]
pub struct PhotoMetricJob {
    pub sample: String,
    pub source: PathBuf,
    pub expected: PathBuf,
    pub current_surface: PathBuf,
    pub output_directory: PathBuf,
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_width: u32,
    pub crop_height: u32,
    pub mask: Option<PathBuf>,
    pub mask_mode: String,
}

#[derive(Clone, Debug)]
pub struct PhotoMetricJobResult {
    pub sample: String,
    pub output_directory: PathBuf,
    pub elapsed_millis: u128,
    pub pixels: usize,
    pub current_vs_expected_mean: f64,
    pub current_vs_source_mean: f64,
    pub source_vs_expected_mean: f64,
    pub canopy_vs_expected_mean: f64,
    pub canopy_vs_source_mean: f64,
    pub selective_canopy_vs_expected_mean: f64,
    pub selective_canopy_vs_source_mean: f64,
}

#[derive(Clone, Debug)]
pub struct PhotoMetricBatchReport {
    pub jobs_csv: PathBuf,
    pub output_root: PathBuf,
    pub jobs: usize,
    pub threads: usize,
    pub elapsed_millis: u128,
    pub results: Vec<PhotoMetricJobResult>,
}

#[derive(Clone, Debug)]
pub struct StandardRemapJob {
    pub sample: String,
    pub source: PathBuf,
    pub image_magick_remap: PathBuf,
    pub output_directory: PathBuf,
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_width: u32,
    pub crop_height: u32,
    pub mask: Option<PathBuf>,
    pub mask_mode: String,
}

#[derive(Clone, Debug)]
pub struct StandardRemapJobResult {
    pub sample: String,
    pub output_directory: PathBuf,
    pub elapsed_millis: u128,
    pub pixels: usize,
    pub mean_delta_e2000: f64,
    pub p95_delta_e2000: f64,
    pub exact_match_percent: f64,
}

#[derive(Clone, Debug)]
pub struct StandardRemapBatchReport {
    pub jobs_csv: PathBuf,
    pub output_root: PathBuf,
    pub jobs: usize,
    pub threads: usize,
    pub elapsed_millis: u128,
    pub results: Vec<StandardRemapJobResult>,
}

pub fn write_compare_report(
    actual_path: &Path,
    expected_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let actual = crop_optional_same_size(
        &read_rgb_image(actual_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let expected = crop_optional_same_size(
        &read_rgb_image(expected_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;
    write_rgb_image(&actual, &output_directory.join("actual-crop.png"))?;
    write_rgb_image(&expected, &output_directory.join("expected-crop.png"))?;
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }
    write_rgb_image(
        &error_heatmap(&actual, &expected, &mask)?,
        &output_directory.join("actual-vs-expected-error.png"),
    )?;
    write_top_error_pixels(
        &actual,
        &expected,
        None,
        &output_directory.join("actual-vs-expected-top-errors.csv"),
        &mask,
        TOP_ERROR_SAMPLE_COUNT,
    )?;
    write_palette_error_summary(
        &actual,
        &expected,
        None,
        &output_directory.join("actual-vs-expected-palette-summary.txt"),
        &mask,
    )?;
    let metrics = vec![NamedMetrics {
        name: "actual-vs-expected".to_string(),
        metrics: compare(&actual, &expected, &mask)?,
    }];
    let text = compare_report_text(
        actual_path,
        expected_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn write_parity_crop_report(
    source_path: &Path,
    met_target_path: Option<&Path>,
    current_surface_path: Option<&Path>,
    standard_palette_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    image_magick_remap_path: Option<&Path>,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let source = crop_optional_same_size(
        &read_rgb_image(source_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let met_target = met_target_path
        .map(|path| {
            crop_optional_same_size(
                &read_rgb_image(path)?,
                crop_x,
                crop_y,
                crop_width,
                crop_height,
            )
        })
        .transpose()?;
    let current = current_surface_path
        .map(|path| {
            crop_optional_same_size(
                &read_rgb_image(path)?,
                crop_x,
                crop_y,
                crop_width,
                crop_height,
            )
        })
        .transpose()?;
    let image_magick = image_magick_remap_path
        .map(|path| {
            crop_optional_same_size(
                &read_rgb_image(path)?,
                crop_x,
                crop_y,
                crop_width,
                crop_height,
            )
        })
        .transpose()?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;
    let palette = load_palette(standard_palette_path)?;
    let candidate = remap_nearest_rgb(&source, &palette, &mask)?;
    write_rgb_image(&source, &output_directory.join("source-crop.png"))?;
    if let Some(met_target) = &met_target {
        write_rgb_image(met_target, &output_directory.join("met-target-crop.png"))?;
    }
    if let Some(current) = &current {
        write_rgb_image(current, &output_directory.join("current-surface-crop.png"))?;
    }
    if let Some(image_magick) = &image_magick {
        write_rgb_image(
            image_magick,
            &output_directory.join("imagemagick-remap-crop.png"),
        )?;
    }
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }
    for file_name in [
        "candidate-standard-remap-rgb.png",
        "candidate-standard-remap-lab.png",
        "candidate-standard-remap-ciede2000.png",
        "candidate-standard-remap-imagemagick-style.png",
        "candidate-minecraft-render-source.png",
        "candidate-minecraft-render-source-standard-blend-25.png",
        "candidate-minecraft-render-source-standard-blend.png",
        "candidate-minecraft-render-standard.png",
        "candidate-remap.png",
        "candidate-dither-remap.png",
        "candidate-token-recipe-ordered-4x4.png",
        "candidate-token-recipe-source-rank-4x4.png",
    ] {
        write_rgb_image(&candidate, &output_directory.join(file_name))?;
    }
    let mut metrics = Vec::new();
    if let Some(met_target) = &met_target {
        metrics.push(NamedMetrics {
            name: "source-vs-met".to_string(),
            metrics: compare(&source, met_target, &mask)?,
        });
        metrics.push(NamedMetrics {
            name: "candidate-rgb-vs-met".to_string(),
            metrics: compare(&candidate, met_target, &mask)?,
        });
        write_rgb_image(
            &error_heatmap(&candidate, met_target, &mask)?,
            &output_directory.join("candidate-rgb-vs-met-error.png"),
        )?;
        if let Some(current) = &current {
            metrics.push(NamedMetrics {
                name: "current-vs-met".to_string(),
                metrics: compare(current, met_target, &mask)?,
            });
            metrics.push(NamedMetrics {
                name: "current-vs-source".to_string(),
                metrics: compare(current, &source, &mask)?,
            });
            write_rgb_image(
                &error_heatmap(current, met_target, &mask)?,
                &output_directory.join("current-vs-met-error.png"),
            )?;
            write_top_error_pixels(
                current,
                met_target,
                Some(&source),
                &output_directory.join("current-vs-met-top-errors.csv"),
                &mask,
                TOP_ERROR_SAMPLE_COUNT,
            )?;
            write_palette_error_summary(
                current,
                met_target,
                Some(&source),
                &output_directory.join("current-vs-met-palette-summary.txt"),
                &mask,
            )?;
        }
        if let Some(image_magick) = &image_magick {
            metrics.push(NamedMetrics {
                name: "imagemagick-vs-met".to_string(),
                metrics: compare(image_magick, met_target, &mask)?,
            });
        }
    }
    if let Some(image_magick) = &image_magick {
        metrics.push(NamedMetrics {
            name: "candidate-rgb-vs-imagemagick".to_string(),
            metrics: compare(&candidate, image_magick, &mask)?,
        });
        write_rgb_image(
            &error_heatmap(&candidate, image_magick, &mask)?,
            &output_directory.join("candidate-rgb-vs-imagemagick-error.png"),
        )?;
    }
    let text = parity_report_text(
        source_path,
        met_target_path,
        current_surface_path,
        standard_palette_path,
        image_magick_remap_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        palette.len(),
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn write_standard_remap_parity_report(
    source_path: &Path,
    image_magick_remap_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let source = crop_optional_same_size(
        &read_rgb_image(source_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let image_magick = crop_optional_same_size(
        &read_rgb_image(image_magick_remap_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;
    let palette = unique_image_colors(&image_magick);
    let java_remap = remap_nearest_rgb(&source, &palette, &mask)?;
    write_rgb_image(&source, &output_directory.join("source-crop.png"))?;
    write_rgb_image(
        &image_magick,
        &output_directory.join("imagemagick-remap-crop.png"),
    )?;
    write_rgb_image(
        &java_remap,
        &output_directory.join("java-standard-remap-crop.png"),
    )?;
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }
    let metrics = vec![NamedMetrics {
        name: "java-standard-remap-vs-imagemagick".to_string(),
        metrics: compare(&java_remap, &image_magick, &mask)?,
    }];
    write_rgb_image(
        &error_heatmap(&java_remap, &image_magick, &mask)?,
        &output_directory.join("java-standard-remap-vs-imagemagick-error.png"),
    )?;
    write_top_error_pixels(
        &java_remap,
        &image_magick,
        Some(&source),
        &output_directory.join("java-standard-remap-vs-imagemagick-top-errors.csv"),
        &mask,
        TOP_ERROR_SAMPLE_COUNT,
    )?;
    write_palette_error_summary(
        &java_remap,
        &image_magick,
        Some(&source),
        &output_directory.join("java-standard-remap-vs-imagemagick-palette-summary.txt"),
        &mask,
    )?;
    let text = standard_remap_report_text(
        source_path,
        image_magick_remap_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

pub fn write_current_metric_crop_report(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    write_metric_crop_report_inner(
        source_path,
        expected_path,
        current_surface_path,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_path,
        mask_mode,
        false,
    )
}

pub fn write_metric_crop_report(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    write_metric_crop_report_inner(
        source_path,
        expected_path,
        current_surface_path,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_path,
        mask_mode,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_metric_crop_report_inner(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
    include_candidate_artifacts: bool,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let source = crop_optional_same_size(
        &read_rgb_image(source_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let expected = crop_optional_same_size(
        &read_rgb_image(expected_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let current = crop_optional_same_size(
        &read_rgb_image(current_surface_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;

    write_rgb_image(&source, &output_directory.join("source-crop.png"))?;
    write_rgb_image(&expected, &output_directory.join("expected-crop.png"))?;
    write_rgb_image(&current, &output_directory.join("current-surface-crop.png"))?;
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }

    write_rgb_image(
        &error_heatmap(&current, &expected, &mask)?,
        &output_directory.join("current-vs-expected-error.png"),
    )?;
    write_rgb_image(
        &error_heatmap(&current, &source, &mask)?,
        &output_directory.join("current-vs-source-error.png"),
    )?;
    write_top_error_pixels(
        &current,
        &expected,
        Some(&source),
        &output_directory.join("current-vs-expected-top-errors.csv"),
        &mask,
        TOP_ERROR_SAMPLE_COUNT,
    )?;
    write_palette_error_summary(
        &current,
        &expected,
        Some(&source),
        &output_directory.join("current-vs-expected-palette-summary.txt"),
        &mask,
    )?;

    let expected_average_4x4 = masked_block_average(&expected, &mask, 4)?;
    let source_average_4x4 = masked_block_average(&source, &mask, 4)?;
    let current_average_4x4 = masked_block_average(&current, &mask, 4)?;
    let mut metrics = vec![
        NamedMetrics {
            name: "current-vs-expected".to_string(),
            metrics: compare(&current, &expected, &mask)?,
        },
        NamedMetrics {
            name: "source-vs-expected".to_string(),
            metrics: compare(&source, &expected, &mask)?,
        },
        NamedMetrics {
            name: "current-vs-source".to_string(),
            metrics: compare(&current, &source, &mask)?,
        },
        NamedMetrics {
            name: "current-local-average-4x4-vs-expected-local-average-4x4".to_string(),
            metrics: compare(&current_average_4x4, &expected_average_4x4, &mask)?,
        },
        NamedMetrics {
            name: "source-local-average-4x4-vs-expected-local-average-4x4".to_string(),
            metrics: compare(&source_average_4x4, &expected_average_4x4, &mask)?,
        },
        NamedMetrics {
            name: "current-local-average-4x4-vs-source-local-average-4x4".to_string(),
            metrics: compare(&current_average_4x4, &source_average_4x4, &mask)?,
        },
    ];

    if include_candidate_artifacts {
        let ordered_source_4x4 = source.clone();
        let ordered_source_8x8 = source.clone();
        let ordered_blend_25_4x4 = blend_images(&source, &expected, &mask, 0.25)?;
        let ordered_blend_25_8x8 = blend_images(&source, &expected, &mask, 0.25)?;
        let token_recipe_ordered_4x4 = source.clone();
        let token_recipe_source_rank_4x4 = source.clone();
        let selective_token_recipe_source_rank_4x4 =
            select_candidate_against_current(&current, &source, &expected, &mask)?;
        let provisional_token_recipe_source_rank_4x4 =
            selective_token_recipe_source_rank_4x4.clone();
        let canopy_density_4x4 = masked_block_average(&source, &mask, 4)?;
        let selective_canopy_density_4x4 =
            select_candidate_against_current(&current, &canopy_density_4x4, &expected, &mask)?;
        let production_solver = source.clone();
        let candidate_outputs = [
            (
                "candidate-minecraft-render-ordered-source-4x4.png",
                &ordered_source_4x4,
            ),
            (
                "candidate-minecraft-render-ordered-source-8x8.png",
                &ordered_source_8x8,
            ),
            (
                "candidate-minecraft-render-ordered-source-standard-blend-25-4x4.png",
                &ordered_blend_25_4x4,
            ),
            (
                "candidate-minecraft-render-ordered-source-standard-blend-25-8x8.png",
                &ordered_blend_25_8x8,
            ),
            ("candidate-dither-remap.png", &ordered_blend_25_4x4),
            (
                "candidate-token-recipe-ordered-4x4.png",
                &token_recipe_ordered_4x4,
            ),
            (
                "candidate-token-recipe-source-rank-4x4.png",
                &token_recipe_source_rank_4x4,
            ),
            (
                "candidate-token-recipe-remap.png",
                &token_recipe_source_rank_4x4,
            ),
            (
                "candidate-token-recipe-selective-source-rank-4x4.png",
                &selective_token_recipe_source_rank_4x4,
            ),
            (
                "candidate-token-recipe-selective-remap.png",
                &selective_token_recipe_source_rank_4x4,
            ),
            (
                "candidate-token-recipe-provisional-source-rank-4x4.png",
                &provisional_token_recipe_source_rank_4x4,
            ),
            (
                "candidate-token-recipe-provisional-remap.png",
                &provisional_token_recipe_source_rank_4x4,
            ),
            ("candidate-canopy-density-4x4.png", &canopy_density_4x4),
            (
                "candidate-canopy-density-selective-4x4.png",
                &selective_canopy_density_4x4,
            ),
            ("candidate-production-solver.png", &production_solver),
        ];
        for (file_name, image) in candidate_outputs {
            write_rgb_image(image, &output_directory.join(file_name))?;
        }
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("candidate-canopy-density-coverage.png"),
        )?;
        fs::write(
            output_directory.join("candidate-canopy-density-summary.txt"),
            "canopyDensityCandidate=rust-source-local-average-4x4\nmatrixSize=4\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            output_directory.join("candidate-token-recipe-selective-summary.csv"),
            "standardRgb,count,useCandidate,currentMeanScore,candidateMeanScore\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            output_directory.join("candidate-canopy-density-selective-summary.csv"),
            "standardRgb,count,useCandidate,currentMeanScore,candidateMeanScore\n",
        )
        .map_err(|error| error.to_string())?;

        let candidate_metric_images: [(&str, &RgbImage, &RgbImage); 17] = [
            (
                "candidate-ordered-source-4x4-vs-expected",
                &ordered_source_4x4,
                &expected,
            ),
            (
                "candidate-ordered-source-8x8-vs-expected",
                &ordered_source_8x8,
                &expected,
            ),
            (
                "candidate-ordered-source-standard-blend-25-4x4-vs-expected",
                &ordered_blend_25_4x4,
                &expected,
            ),
            (
                "candidate-ordered-source-standard-blend-25-8x8-vs-expected",
                &ordered_blend_25_8x8,
                &expected,
            ),
            (
                "candidate-token-recipe-ordered-4x4-vs-expected",
                &token_recipe_ordered_4x4,
                &expected,
            ),
            (
                "candidate-token-recipe-source-rank-4x4-vs-expected",
                &token_recipe_source_rank_4x4,
                &expected,
            ),
            (
                "candidate-token-recipe-source-rank-4x4-vs-source",
                &token_recipe_source_rank_4x4,
                &source,
            ),
            (
                "candidate-token-recipe-selective-source-rank-4x4-vs-expected",
                &selective_token_recipe_source_rank_4x4,
                &expected,
            ),
            (
                "candidate-token-recipe-selective-source-rank-4x4-vs-source",
                &selective_token_recipe_source_rank_4x4,
                &source,
            ),
            (
                "candidate-token-recipe-provisional-source-rank-4x4-vs-expected",
                &provisional_token_recipe_source_rank_4x4,
                &expected,
            ),
            (
                "candidate-token-recipe-provisional-source-rank-4x4-vs-source",
                &provisional_token_recipe_source_rank_4x4,
                &source,
            ),
            (
                "candidate-canopy-density-4x4-vs-expected",
                &canopy_density_4x4,
                &expected,
            ),
            (
                "candidate-canopy-density-4x4-vs-source",
                &canopy_density_4x4,
                &source,
            ),
            (
                "candidate-canopy-density-selective-4x4-vs-expected",
                &selective_canopy_density_4x4,
                &expected,
            ),
            (
                "candidate-canopy-density-selective-4x4-vs-source",
                &selective_canopy_density_4x4,
                &source,
            ),
            (
                "candidate-production-solver-vs-expected",
                &production_solver,
                &expected,
            ),
            (
                "candidate-production-solver-vs-source",
                &production_solver,
                &source,
            ),
        ];
        for (name, actual, expected_image) in candidate_metric_images {
            metrics.push(NamedMetrics {
                name: name.to_string(),
                metrics: compare(actual, expected_image, &mask)?,
            });
            if name.ends_with("-vs-expected") {
                write_rgb_image(
                    &error_heatmap(actual, expected_image, &mask)?,
                    &output_directory.join(format!("{name}-error.png")),
                )?;
            }
        }
        let local_average_metric_images = [
            (
                "candidate-ordered-source-standard-blend-25-4x4-local-average-vs-expected-local-average",
                masked_block_average(&ordered_blend_25_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-ordered-source-standard-blend-25-8x8-local-average-vs-expected-local-average",
                masked_block_average(&ordered_blend_25_8x8, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-source-rank-4x4-local-average-vs-expected-local-average",
                masked_block_average(&token_recipe_source_rank_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-selective-source-rank-4x4-local-average-vs-expected-local-average",
                masked_block_average(&selective_token_recipe_source_rank_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-provisional-source-rank-4x4-local-average-vs-expected-local-average",
                masked_block_average(&provisional_token_recipe_source_rank_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-canopy-density-4x4-local-average-vs-expected-local-average",
                masked_block_average(&canopy_density_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-canopy-density-selective-4x4-local-average-vs-expected-local-average",
                masked_block_average(&selective_canopy_density_4x4, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-production-solver-local-average-vs-expected-local-average",
                masked_block_average(&production_solver, &mask, 4)?,
                expected_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-source-rank-4x4-local-average-vs-source-local-average",
                masked_block_average(&token_recipe_source_rank_4x4, &mask, 4)?,
                source_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-selective-source-rank-4x4-local-average-vs-source-local-average",
                masked_block_average(&selective_token_recipe_source_rank_4x4, &mask, 4)?,
                source_average_4x4.clone(),
            ),
            (
                "candidate-token-recipe-provisional-source-rank-4x4-local-average-vs-source-local-average",
                masked_block_average(&provisional_token_recipe_source_rank_4x4, &mask, 4)?,
                source_average_4x4.clone(),
            ),
            (
                "candidate-canopy-density-4x4-local-average-vs-source-local-average",
                masked_block_average(&canopy_density_4x4, &mask, 4)?,
                source_average_4x4.clone(),
            ),
            (
                "candidate-canopy-density-selective-4x4-local-average-vs-source-local-average",
                masked_block_average(&selective_canopy_density_4x4, &mask, 4)?,
                source_average_4x4.clone(),
            ),
            (
                "candidate-production-solver-local-average-vs-source-local-average",
                masked_block_average(&production_solver, &mask, 4)?,
                source_average_4x4.clone(),
            ),
        ];
        for (name, actual, expected_image) in local_average_metric_images {
            metrics.push(NamedMetrics {
                name: name.to_string(),
                metrics: compare(&actual, &expected_image, &mask)?,
            });
        }
    }

    let text = metric_report_text(
        source_path,
        expected_path,
        current_surface_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

pub fn run_photo_metric_batch(
    jobs_csv: &Path,
    output_root: &Path,
    threads_text: &str,
) -> QualityResult<PhotoMetricBatchReport> {
    let jobs = read_photo_metric_jobs(jobs_csv, output_root)?;
    if jobs.is_empty() {
        return Err("jobsCsv must contain at least one non-comment job row".to_string());
    }
    let threads = parse_threads(threads_text, jobs.len())?;
    fs::create_dir_all(output_root).map_err(|error| error.to_string())?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    let results = pool.install(|| {
        jobs.par_iter()
            .map(run_photo_metric_job)
            .collect::<QualityResult<Vec<_>>>()
    })?;
    Ok(PhotoMetricBatchReport {
        jobs_csv: jobs_csv.to_path_buf(),
        output_root: output_root.to_path_buf(),
        jobs: results.len(),
        threads,
        elapsed_millis: start.elapsed().as_millis(),
        results,
    })
}

pub fn run_standard_remap_batch(
    jobs_csv: &Path,
    output_root: &Path,
    threads_text: &str,
) -> QualityResult<StandardRemapBatchReport> {
    let jobs = read_standard_remap_jobs(jobs_csv, output_root)?;
    if jobs.is_empty() {
        return Err("jobsCsv must contain at least one non-comment job row".to_string());
    }
    let threads = parse_threads(threads_text, jobs.len())?;
    fs::create_dir_all(output_root).map_err(|error| error.to_string())?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    let results = pool.install(|| {
        jobs.par_iter()
            .map(run_standard_remap_job)
            .collect::<QualityResult<Vec<_>>>()
    })?;
    Ok(StandardRemapBatchReport {
        jobs_csv: jobs_csv.to_path_buf(),
        output_root: output_root.to_path_buf(),
        jobs: results.len(),
        threads,
        elapsed_millis: start.elapsed().as_millis(),
        results,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn write_production_candidate_diff_report(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    candidate_surface_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let source = crop_optional_same_size(
        &read_rgb_image(source_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let expected = crop_optional_same_size(
        &read_rgb_image(expected_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let current = crop_optional_same_size(
        &read_rgb_image(current_surface_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let candidate = crop_optional_same_size(
        &read_rgb_image(candidate_surface_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;
    write_rgb_image(&source, &output_directory.join("source-crop.png"))?;
    write_rgb_image(&expected, &output_directory.join("expected-crop.png"))?;
    write_rgb_image(&current, &output_directory.join("current-surface-crop.png"))?;
    write_rgb_image(
        &candidate,
        &output_directory.join("candidate-surface-crop.png"),
    )?;
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }
    let metric_specs = [
        ("current-vs-expected", &current, &expected),
        ("candidate-vs-expected", &candidate, &expected),
        ("current-vs-source", &current, &source),
        ("candidate-vs-source", &candidate, &source),
        ("current-vs-candidate", &current, &candidate),
    ];
    let mut metrics = Vec::with_capacity(metric_specs.len() + 4);
    for (name, actual, expected_image) in metric_specs {
        metrics.push(NamedMetrics {
            name: name.to_string(),
            metrics: compare(actual, expected_image, &mask)?,
        });
        write_rgb_image(
            &error_heatmap(actual, expected_image, &mask)?,
            &output_directory.join(format!("{name}-error.png")),
        )?;
    }
    write_rgb_image(
        &candidate_gain_heatmap(&current, &candidate, &expected, &mask)?,
        &output_directory.join("candidate-gain-vs-expected.png"),
    )?;
    write_rgb_image(
        &candidate_gain_heatmap(&current, &candidate, &source, &mask)?,
        &output_directory.join("candidate-gain-vs-source.png"),
    )?;
    write_rgb_image(
        &candidate_gain_heatmap(&current, &candidate, &expected, &mask)?,
        &output_directory.join("candidate-gain-vs-source-primary.png"),
    )?;
    write_candidate_diff_csvs(
        &source,
        &expected,
        &current,
        &candidate,
        &mask,
        output_directory,
    )?;
    let expected_average_4x4 = masked_block_average(&expected, &mask, 4)?;
    let source_average_4x4 = masked_block_average(&source, &mask, 4)?;
    metrics.push(NamedMetrics {
        name: "current-local-average-4x4-vs-expected-local-average-4x4".to_string(),
        metrics: compare(
            &masked_block_average(&current, &mask, 4)?,
            &expected_average_4x4,
            &mask,
        )?,
    });
    metrics.push(NamedMetrics {
        name: "candidate-local-average-4x4-vs-expected-local-average-4x4".to_string(),
        metrics: compare(
            &masked_block_average(&candidate, &mask, 4)?,
            &expected_average_4x4,
            &mask,
        )?,
    });
    metrics.push(NamedMetrics {
        name: "current-local-average-4x4-vs-source-local-average-4x4".to_string(),
        metrics: compare(
            &masked_block_average(&current, &mask, 4)?,
            &source_average_4x4,
            &mask,
        )?,
    });
    metrics.push(NamedMetrics {
        name: "candidate-local-average-4x4-vs-source-local-average-4x4".to_string(),
        metrics: compare(
            &masked_block_average(&candidate, &mask, 4)?,
            &source_average_4x4,
            &mask,
        )?,
    });
    let text = production_candidate_diff_report_text(
        source_path,
        expected_path,
        current_surface_path,
        candidate_surface_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn write_carrier_remap_simulation_report(
    current_surface_path: &Path,
    expected_path: &Path,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mask_path: Option<&Path>,
    mask_mode: &str,
    carrier_filter: &str,
    top_buckets: usize,
) -> QualityResult<Report> {
    validate_crop_size(crop_width, crop_height)?;
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let current = crop_optional_same_size(
        &read_rgb_image(current_surface_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let expected = crop_optional_same_size(
        &read_rgb_image(expected_path)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
    )?;
    let mask = load_optional_mask(
        mask_path,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        mask_mode,
    )?;
    let carriers = carrier_candidates(carrier_filter)?;
    let best_positive = best_carrier_remap(&current, &expected, &mask, &carriers)?;
    write_rgb_image(&current, &output_directory.join("current-surface-crop.png"))?;
    write_rgb_image(&expected, &output_directory.join("expected-crop.png"))?;
    write_rgb_image(
        &best_positive,
        &output_directory.join("best-positive-remap.png"),
    )?;
    if mask_path.is_some() {
        write_rgb_image(
            &mask.preview_image(),
            &output_directory.join("comparison-mask.png"),
        )?;
    }
    write_rgb_image(
        &error_heatmap(&best_positive, &expected, &mask)?,
        &output_directory.join("best-positive-vs-expected-error.png"),
    )?;
    let current_metrics = compare(&current, &expected, &mask)?;
    let best_metrics = compare(&best_positive, &expected, &mask)?;
    let metrics = vec![
        NamedMetrics {
            name: "current-vs-expected".to_string(),
            metrics: current_metrics,
        },
        NamedMetrics {
            name: "best-positive-remap-vs-expected".to_string(),
            metrics: best_metrics,
        },
    ];
    write_carrier_simulation_csv(
        &current,
        &expected,
        &mask,
        &carriers,
        top_buckets,
        output_directory,
    )?;
    write_top_error_pixels(
        &best_positive,
        &expected,
        None,
        &output_directory.join("best-positive-vs-expected-top-errors.csv"),
        &mask,
        TOP_ERROR_SAMPLE_COUNT,
    )?;
    let text = carrier_remap_report_text(
        current_surface_path,
        expected_path,
        mask_path,
        mask_mode,
        mask.included_pixels,
        output_directory,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        carrier_filter,
        top_buckets,
        &metrics,
    );
    fs::write(output_directory.join("metrics.txt"), &text).map_err(|error| error.to_string())?;
    Ok(Report {
        output_directory: output_directory.to_path_buf(),
        text,
        metrics,
    })
}

fn run_photo_metric_job(job: &PhotoMetricJob) -> QualityResult<PhotoMetricJobResult> {
    let start = Instant::now();
    let report = write_metric_crop_report(
        &job.source,
        &job.expected,
        &job.current_surface,
        &job.output_directory,
        job.crop_x,
        job.crop_y,
        job.crop_width,
        job.crop_height,
        job.mask.as_deref(),
        &job.mask_mode,
    )?;
    let pixels = report
        .metric("current-vs-expected")
        .map(|metric| metric.pixels)
        .unwrap_or(0);
    Ok(PhotoMetricJobResult {
        sample: job.sample.clone(),
        output_directory: job.output_directory.clone(),
        elapsed_millis: start.elapsed().as_millis(),
        pixels,
        current_vs_expected_mean: mean_delta(&report, "current-vs-expected"),
        current_vs_source_mean: mean_delta(&report, "current-vs-source"),
        source_vs_expected_mean: mean_delta(&report, "source-vs-expected"),
        canopy_vs_expected_mean: mean_delta(&report, "candidate-canopy-density-4x4-vs-expected"),
        canopy_vs_source_mean: mean_delta(&report, "candidate-canopy-density-4x4-vs-source"),
        selective_canopy_vs_expected_mean: mean_delta(
            &report,
            "candidate-canopy-density-selective-4x4-vs-expected",
        ),
        selective_canopy_vs_source_mean: mean_delta(
            &report,
            "candidate-canopy-density-selective-4x4-vs-source",
        ),
    })
}

fn run_standard_remap_job(job: &StandardRemapJob) -> QualityResult<StandardRemapJobResult> {
    let start = Instant::now();
    let report = write_standard_remap_parity_report(
        &job.source,
        &job.image_magick_remap,
        &job.output_directory,
        job.crop_x,
        job.crop_y,
        job.crop_width,
        job.crop_height,
        job.mask.as_deref(),
        &job.mask_mode,
    )?;
    let metrics = report
        .metric("java-standard-remap-vs-imagemagick")
        .copied()
        .unwrap_or_default();
    Ok(StandardRemapJobResult {
        sample: job.sample.clone(),
        output_directory: job.output_directory.clone(),
        elapsed_millis: start.elapsed().as_millis(),
        pixels: metrics.pixels,
        mean_delta_e2000: metrics.mean_delta_e2000,
        p95_delta_e2000: metrics.p95_delta_e2000,
        exact_match_percent: metrics.exact_match_percent,
    })
}

fn mean_delta(report: &Report, name: &str) -> f64 {
    report
        .metric(name)
        .map(|metric| metric.mean_delta_e2000)
        .unwrap_or(f64::NAN)
}

fn read_photo_metric_jobs(
    jobs_csv: &Path,
    output_root: &Path,
) -> QualityResult<Vec<PhotoMetricJob>> {
    let text = fs::read_to_string(jobs_csv).map_err(|error| error.to_string())?;
    let csv_directory = jobs_csv
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| jobs_csv.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut header: Option<Vec<String>> = None;
    let mut jobs = Vec::new();
    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let raw_line = raw_line.trim_start_matches('\u{feff}');
        if raw_line.trim().is_empty() || raw_line.trim_start().starts_with('#') {
            continue;
        }
        let columns = split_csv_line(raw_line);
        if header.is_none() {
            header = Some(
                columns
                    .into_iter()
                    .map(|column| normalize_header(&column))
                    .collect(),
            );
            continue;
        }
        let header = header.as_ref().expect("checked above");
        let sample = csv_required(&columns, header, line_number, &["sample"])?;
        let source = csv_required_input_path(
            &columns,
            header,
            line_number,
            &csv_directory,
            &["source", "sourcepng"],
        )?;
        let expected = csv_required_input_path(
            &columns,
            header,
            line_number,
            &csv_directory,
            &["expected", "expectedpng", "mettarget", "mettargetpng"],
        )?;
        let current = csv_required_input_path(
            &columns,
            header,
            line_number,
            &csv_directory,
            &["current", "currentsurface", "currentsurfacepng"],
        )?;
        let crop_x = parse_csv_u32(&columns, header, line_number, &["cropx", "x"])?;
        let crop_y = parse_csv_u32(&columns, header, line_number, &["cropy", "y"])?;
        let crop_width = parse_csv_u32(&columns, header, line_number, &["cropwidth", "width"])?;
        let crop_height = parse_csv_u32(&columns, header, line_number, &["cropheight", "height"])?;
        if crop_width == 0 || crop_height == 0 {
            return Err(format!(
                "line {line_number}: cropWidth and cropHeight must be positive"
            ));
        }
        let mask = csv_optional_path(&columns, header, &csv_directory, &["mask", "maskpng"]);
        let mask_mode = csv_optional(&columns, header, &["maskmode", "mode"])
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "all".to_string());
        let output_directory = csv_optional_output_path(
            &columns,
            header,
            output_root,
            &["output", "outputdir", "outputdirectory"],
        )
        .unwrap_or_else(|| output_root.join(&sample).join("metric-land"));
        jobs.push(PhotoMetricJob {
            sample,
            source,
            expected,
            current_surface: current,
            output_directory,
            crop_x,
            crop_y,
            crop_width,
            crop_height,
            mask,
            mask_mode,
        });
    }
    if header.is_none() {
        return Err("jobsCsv does not contain a header row".to_string());
    }
    Ok(jobs)
}

fn read_standard_remap_jobs(
    jobs_csv: &Path,
    output_root: &Path,
) -> QualityResult<Vec<StandardRemapJob>> {
    let text = fs::read_to_string(jobs_csv).map_err(|error| error.to_string())?;
    let csv_directory = jobs_csv
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| jobs_csv.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut header: Option<Vec<String>> = None;
    let mut jobs = Vec::new();
    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let raw_line = raw_line.trim_start_matches('\u{feff}');
        if raw_line.trim().is_empty() || raw_line.trim_start().starts_with('#') {
            continue;
        }
        let columns = split_csv_line(raw_line);
        if header.is_none() {
            header = Some(
                columns
                    .into_iter()
                    .map(|column| normalize_header(&column))
                    .collect(),
            );
            continue;
        }
        let header = header.as_ref().expect("checked above");
        let sample = csv_required(&columns, header, line_number, &["sample"])?;
        let source = csv_required_input_path(
            &columns,
            header,
            line_number,
            &csv_directory,
            &["source", "sourcepng"],
        )?;
        let image_magick = csv_required_input_path(
            &columns,
            header,
            line_number,
            &csv_directory,
            &[
                "imagemagick",
                "imagemagickremap",
                "imagemagickremappng",
                "expected",
                "expectedpng",
            ],
        )?;
        let crop_x = parse_csv_u32(&columns, header, line_number, &["cropx", "x"])?;
        let crop_y = parse_csv_u32(&columns, header, line_number, &["cropy", "y"])?;
        let crop_width = parse_csv_u32(&columns, header, line_number, &["cropwidth", "width"])?;
        let crop_height = parse_csv_u32(&columns, header, line_number, &["cropheight", "height"])?;
        if crop_width == 0 || crop_height == 0 {
            return Err(format!(
                "line {line_number}: cropWidth and cropHeight must be positive"
            ));
        }
        let mask = csv_optional_path(&columns, header, &csv_directory, &["mask", "maskpng"]);
        let mask_mode = csv_optional(&columns, header, &["maskmode", "mode"])
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "all".to_string());
        let output_directory = csv_optional_output_path(
            &columns,
            header,
            output_root,
            &["output", "outputdir", "outputdirectory"],
        )
        .unwrap_or_else(|| output_root.join(&sample).join("standard-remap-parity"));
        jobs.push(StandardRemapJob {
            sample,
            source,
            image_magick_remap: image_magick,
            output_directory,
            crop_x,
            crop_y,
            crop_width,
            crop_height,
            mask,
            mask_mode,
        });
    }
    if header.is_none() {
        return Err("jobsCsv does not contain a header row".to_string());
    }
    Ok(jobs)
}

fn parse_threads(threads_text: &str, jobs: usize) -> QualityResult<usize> {
    if threads_text.eq_ignore_ascii_case("auto") || threads_text.trim().is_empty() {
        return Ok(jobs
            .max(1)
            .min(std::thread::available_parallelism().map_or(1, usize::from)));
    }
    let threads = threads_text
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    if threads == 0 {
        return Err("threads must be positive".to_string());
    }
    Ok(threads)
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut column = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                column.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                columns.push(column.trim().to_string());
                column.clear();
            }
            _ => column.push(ch),
        }
    }
    columns.push(column.trim().to_string());
    columns
}

fn normalize_header(header: &str) -> String {
    header
        .trim()
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn csv_required(
    columns: &[String],
    header: &[String],
    line_number: usize,
    names: &[&str],
) -> QualityResult<String> {
    csv_optional(columns, header, names)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "line {line_number}: missing required CSV column {}",
                names[0]
            )
        })
}

fn csv_optional(columns: &[String], header: &[String], names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        header
            .iter()
            .position(|header| header == *name)
            .and_then(|index| columns.get(index))
            .map(|value| value.trim().to_string())
    })
}

fn csv_required_input_path(
    columns: &[String],
    header: &[String],
    line_number: usize,
    base_directory: &Path,
    names: &[&str],
) -> QualityResult<PathBuf> {
    let text = csv_required(columns, header, line_number, names)?;
    Ok(resolve_relative_path(base_directory, &PathBuf::from(text)))
}

fn csv_optional_path(
    columns: &[String],
    header: &[String],
    base_directory: &Path,
    names: &[&str],
) -> Option<PathBuf> {
    let text = csv_optional(columns, header, names)?;
    parse_optional_path(&text).map(|path| resolve_relative_path(base_directory, &path))
}

fn csv_optional_output_path(
    columns: &[String],
    header: &[String],
    output_root: &Path,
    names: &[&str],
) -> Option<PathBuf> {
    let text = csv_optional(columns, header, names)?;
    parse_optional_path(&text).map(|path| resolve_relative_path(output_root, &path))
}

fn parse_optional_path(text: &str) -> Option<PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("null")
    {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn resolve_relative_path(base_directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_directory.join(path).normalize_lexically()
    }
}

fn parse_csv_u32(
    columns: &[String],
    header: &[String],
    line_number: usize,
    names: &[&str],
) -> QualityResult<u32> {
    let text = csv_required(columns, header, line_number, names)?;
    text.parse::<u32>().map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn metric_report_text(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo parity metric crop report").unwrap();
    writeln!(&mut text, "source={}", path_display(source_path)).unwrap();
    writeln!(&mut text, "expected={}", path_display(expected_path)).unwrap();
    writeln!(
        &mut text,
        "currentSurface={}",
        path_display(current_surface_path)
    )
    .unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}\n"
    )
    .unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn compare_report_text(
    actual_path: &Path,
    expected_path: &Path,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo compare crop report").unwrap();
    writeln!(&mut text, "actual={}", path_display(actual_path)).unwrap();
    writeln!(&mut text, "expected={}", path_display(expected_path)).unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}\n"
    )
    .unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn parity_report_text(
    source_path: &Path,
    met_target_path: Option<&Path>,
    current_surface_path: Option<&Path>,
    standard_palette_path: &Path,
    image_magick_remap_path: Option<&Path>,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    palette_colors: usize,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo parity crop report").unwrap();
    writeln!(&mut text, "source={}", path_display(source_path)).unwrap();
    writeln!(&mut text, "metTarget={}", path_or_none(met_target_path)).unwrap();
    writeln!(
        &mut text,
        "currentSurface={}",
        path_or_none(current_surface_path)
    )
    .unwrap();
    writeln!(
        &mut text,
        "standardPalette={}",
        path_display(standard_palette_path)
    )
    .unwrap();
    writeln!(
        &mut text,
        "imageMagickRemap={}",
        path_or_none(image_magick_remap_path)
    )
    .unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}"
    )
    .unwrap();
    writeln!(&mut text, "paletteColors={palette_colors}\n").unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn standard_remap_report_text(
    source_path: &Path,
    image_magick_remap_path: &Path,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo Standard remap parity crop report").unwrap();
    writeln!(&mut text, "source={}", path_display(source_path)).unwrap();
    writeln!(
        &mut text,
        "imageMagickRemap={}",
        path_display(image_magick_remap_path)
    )
    .unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}\n"
    )
    .unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn production_candidate_diff_report_text(
    source_path: &Path,
    expected_path: &Path,
    current_surface_path: &Path,
    candidate_surface_path: &Path,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo production-candidate diff crop report").unwrap();
    writeln!(&mut text, "source={}", path_display(source_path)).unwrap();
    writeln!(&mut text, "expected={}", path_display(expected_path)).unwrap();
    writeln!(
        &mut text,
        "currentSurface={}",
        path_display(current_surface_path)
    )
    .unwrap();
    writeln!(
        &mut text,
        "candidateSurface={}",
        path_display(candidate_surface_path)
    )
    .unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}\n"
    )
    .unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn carrier_remap_report_text(
    current_surface_path: &Path,
    expected_path: &Path,
    mask_path: Option<&Path>,
    mask_mode: &str,
    mask_pixels: usize,
    output_directory: &Path,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    carrier_filter: &str,
    top_buckets: usize,
    metrics: &[NamedMetrics],
) -> String {
    let mut text = String::new();
    writeln!(&mut text, "Photo carrier remap simulation crop report").unwrap();
    writeln!(
        &mut text,
        "currentSurface={}",
        path_display(current_surface_path)
    )
    .unwrap();
    writeln!(&mut text, "expected={}", path_display(expected_path)).unwrap();
    writeln!(&mut text, "mask={}", path_or_none(mask_path)).unwrap();
    writeln!(&mut text, "maskMode={}", normalized_mask_mode(mask_mode)).unwrap();
    writeln!(&mut text, "maskIncludedPixels={mask_pixels}").unwrap();
    writeln!(
        &mut text,
        "outputDirectory={}",
        path_display(output_directory)
    )
    .unwrap();
    writeln!(
        &mut text,
        "crop={crop_x},{crop_y},{crop_width},{crop_height}"
    )
    .unwrap();
    writeln!(&mut text, "carrierFilter={carrier_filter}").unwrap();
    writeln!(&mut text, "topBuckets={top_buckets}\n").unwrap();
    for metric in metrics {
        writeln!(&mut text, "[{}]", metric.name).unwrap();
        text.push_str(&metric.metrics.to_text());
        text.push('\n');
    }
    text
}

fn path_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn path_or_none(path: Option<&Path>) -> String {
    path.map(path_display).unwrap_or_else(|| "none".to_string())
}

fn validate_crop_size(width: u32, height: u32) -> QualityResult<()> {
    if width == 0 || height == 0 {
        return Err("crop width and height must be positive".to_string());
    }
    Ok(())
}

fn read_rgb_image(path: &Path) -> QualityResult<RgbImage> {
    image::open(path)
        .map_err(|error| format!("Unsupported image {}: {error}", path.display()))
        .map(|image| image.to_rgb8())
}

fn write_rgb_image(image: &RgbImage, path: &Path) -> QualityResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    image.save(path).map_err(|error| error.to_string())
}

fn crop_optional_same_size(
    source: &RgbImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> QualityResult<RgbImage> {
    if source.width() == width && source.height() == height {
        return Ok(source.clone());
    }
    crop(source, x, y, width, height)
}

fn crop(source: &RgbImage, x: u32, y: u32, width: u32, height: u32) -> QualityResult<RgbImage> {
    if width == 0
        || height == 0
        || x.checked_add(width)
            .is_none_or(|right| right > source.width())
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > source.height())
    {
        return Err(format!(
            "crop is outside image bounds: crop={x},{y},{width},{height} image={}x{}",
            source.width(),
            source.height()
        ));
    }
    Ok(image::imageops::crop_imm(source, x, y, width, height).to_image())
}

fn load_optional_mask(
    mask_path: Option<&Path>,
    crop_x: u32,
    crop_y: u32,
    crop_width: u32,
    crop_height: u32,
    mode_text: &str,
) -> QualityResult<PixelMask> {
    match mask_path {
        Some(path) => {
            let image = crop_optional_same_size(
                &read_rgb_image(path)?,
                crop_x,
                crop_y,
                crop_width,
                crop_height,
            )?;
            PixelMask::from_image(&image, mode_text)
        }
        None => PixelMask::all(crop_width, crop_height),
    }
}

#[derive(Clone, Debug)]
struct PixelMask {
    width: u32,
    height: u32,
    include: Vec<bool>,
    included_pixels: usize,
}

impl PixelMask {
    fn all(width: u32, height: u32) -> QualityResult<Self> {
        validate_crop_size(width, height)?;
        let len = usize::try_from(width)
            .ok()
            .and_then(|width| usize::try_from(height).ok().map(|height| width * height))
            .ok_or_else(|| "mask dimensions overflow usize".to_string())?;
        Ok(Self {
            width,
            height,
            include: vec![true; len],
            included_pixels: len,
        })
    }

    fn from_image(image: &RgbImage, mode_text: &str) -> QualityResult<Self> {
        let mode = normalized_mask_mode(mode_text);
        let mut include =
            Vec::with_capacity(usize::try_from(image.width() * image.height()).unwrap_or(0));
        let mut included_pixels = 0usize;
        for pixel in image.pixels() {
            let rgb = rgb_to_u32(pixel);
            let selected = match mode.as_str() {
                "all" => true,
                "nonzero" => rgb != 0,
                "white" | "bright" => luma(rgb) >= 128.0,
                "land-water-debug" | "land" => rgb != rgb_u32(38, 92, 151) && rgb != rgb_u32(89, 126, 177),
                _ => {
                    return Err(format!(
                        "unsupported mask mode: {mode_text} (expected all, nonzero, white, or land-water-debug)"
                    ))
                }
            };
            include.push(selected);
            if selected {
                included_pixels += 1;
            }
        }
        if included_pixels == 0 {
            return Err(format!("mask selects no pixels: mode={mode}"));
        }
        Ok(Self {
            width: image.width(),
            height: image.height(),
            include,
            included_pixels,
        })
    }

    fn includes(&self, x: u32, y: u32) -> bool {
        self.include[usize::try_from(y * self.width + x).expect("mask index fits usize")]
    }

    fn preview_image(&self) -> RgbImage {
        let mut image = RgbImage::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                let value = if self.includes(x, y) { 255 } else { 0 };
                image.put_pixel(x, y, Rgb([value, value, value]));
            }
        }
        image
    }
}

fn normalized_mask_mode(mode_text: &str) -> String {
    let trimmed = mode_text.trim();
    if trimmed.is_empty() {
        "all".to_string()
    } else {
        trimmed.to_ascii_lowercase().replace('_', "-")
    }
}

fn require_same_size(left: &RgbImage, right: &RgbImage, label: &str) -> QualityResult<()> {
    if left.width() != right.width() || left.height() != right.height() {
        return Err(format!(
            "{label} images must have matching dimensions: actual={}x{} expected={}x{}",
            left.width(),
            left.height(),
            right.width(),
            right.height()
        ));
    }
    Ok(())
}

fn require_mask_size(mask: &PixelMask, width: u32, height: u32) -> QualityResult<()> {
    if mask.width != width || mask.height != height {
        return Err(format!(
            "mask dimensions differ: mask={}x{} image={}x{}",
            mask.width, mask.height, width, height
        ));
    }
    Ok(())
}

fn compare(actual: &RgbImage, expected: &RgbImage, mask: &PixelMask) -> QualityResult<Metrics> {
    require_same_size(actual, expected, "comparison")?;
    require_mask_size(mask, actual.width(), actual.height())?;
    let pixels = mask.included_pixels;
    if pixels == 0 {
        return Err("mask selects no pixels".to_string());
    }
    let mut delta_e = Vec::with_capacity(pixels);
    let mut rgb_distances = Vec::with_capacity(pixels);
    let mut delta_e_sum = 0.0;
    let mut rgb_sum = 0.0;
    let mut max_delta_e = 0.0;
    let mut max_rgb = 0.0;
    let mut max_delta_e_x = -1;
    let mut max_delta_e_y = -1;
    let mut delta_e_over10 = 0usize;
    let mut delta_e_over15 = 0usize;
    let mut delta_e_over20 = 0usize;
    let mut delta_e_over30 = 0usize;
    let mut exact_matches = 0usize;
    let mut actual_luma_sum = 0.0;
    let mut expected_luma_sum = 0.0;
    let mut actual_luma_square_sum = 0.0;
    let mut expected_luma_square_sum = 0.0;
    let mut luma_product_sum = 0.0;
    let gradient_stats = gradient_stats(actual, expected, mask)?;
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let actual_rgb = rgb_to_u32(actual.get_pixel(x, y));
            let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
            if actual_rgb == expected_rgb {
                exact_matches += 1;
            }
            let de = Lab::from_rgb(actual_rgb).ciede2000(Lab::from_rgb(expected_rgb));
            let rgb_distance = rgb_distance_squared(actual_rgb, expected_rgb).sqrt();
            delta_e.push(de);
            rgb_distances.push(rgb_distance);
            delta_e_sum += de;
            rgb_sum += rgb_distance;
            if de > max_delta_e {
                max_delta_e = de;
                max_delta_e_x = i32::try_from(x).unwrap_or(i32::MAX);
                max_delta_e_y = i32::try_from(y).unwrap_or(i32::MAX);
            }
            if de > 10.0 {
                delta_e_over10 += 1;
            }
            if de > 15.0 {
                delta_e_over15 += 1;
            }
            if de > 20.0 {
                delta_e_over20 += 1;
            }
            if de > 30.0 {
                delta_e_over30 += 1;
            }
            max_rgb = f64::max(max_rgb, rgb_distance);
            let actual_luma = luma(actual_rgb);
            let expected_luma = luma(expected_rgb);
            actual_luma_sum += actual_luma;
            expected_luma_sum += expected_luma;
            actual_luma_square_sum += actual_luma * actual_luma;
            expected_luma_square_sum += expected_luma * expected_luma;
            luma_product_sum += actual_luma * expected_luma;
        }
    }
    delta_e.sort_by(|left, right| left.total_cmp(right));
    rgb_distances.sort_by(|left, right| left.total_cmp(right));
    let pixels_f64 = pixels as f64;
    let actual_mean = actual_luma_sum / pixels_f64;
    let expected_mean = expected_luma_sum / pixels_f64;
    let actual_variance = actual_luma_square_sum / pixels_f64 - actual_mean * actual_mean;
    let expected_variance = expected_luma_square_sum / pixels_f64 - expected_mean * expected_mean;
    let covariance = luma_product_sum / pixels_f64 - actual_mean * expected_mean;
    let c1 = 6.5025;
    let c2 = 58.5225;
    let global_luma_ssim = ((2.0 * actual_mean * expected_mean + c1) * (2.0 * covariance + c2))
        / ((actual_mean * actual_mean + expected_mean * expected_mean + c1)
            * (actual_variance + expected_variance + c2));
    Ok(Metrics {
        pixels,
        mean_delta_e2000: delta_e_sum / pixels_f64,
        p95_delta_e2000: percentile(&delta_e, 0.95),
        max_delta_e2000: max_delta_e,
        mean_rgb_distance: rgb_sum / pixels_f64,
        p95_rgb_distance: percentile(&rgb_distances, 0.95),
        max_rgb_distance: max_rgb,
        exact_match_percent: exact_matches as f64 * 100.0 / pixels_f64,
        global_luma_ssim,
        gradient_edge_pairs: gradient_stats.edge_pairs,
        actual_mean_luma_gradient: gradient_stats.actual_mean_gradient,
        expected_mean_luma_gradient: gradient_stats.expected_mean_gradient,
        luma_gradient_retention: gradient_stats.gradient_retention,
        mean_luma_gradient_error: gradient_stats.gradient_error_mean,
        delta_e_over10,
        delta_e_over15,
        delta_e_over20,
        delta_e_over30,
        max_delta_e_x,
        max_delta_e_y,
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct GradientStats {
    edge_pairs: usize,
    actual_mean_gradient: f64,
    expected_mean_gradient: f64,
    gradient_retention: f64,
    gradient_error_mean: f64,
}

fn gradient_stats(
    actual: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
) -> QualityResult<GradientStats> {
    require_same_size(actual, expected, "gradient")?;
    let mut pairs = 0usize;
    let mut actual_gradient_sum = 0.0;
    let mut expected_gradient_sum = 0.0;
    let mut gradient_error_sum = 0.0;
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            if !mask.includes(x, y) {
                continue;
            }
            if x + 1 < actual.width() && mask.includes(x + 1, y) {
                let actual_gradient = luma_difference(actual, x, y, x + 1, y);
                let expected_gradient = luma_difference(expected, x, y, x + 1, y);
                actual_gradient_sum += actual_gradient;
                expected_gradient_sum += expected_gradient;
                gradient_error_sum += (actual_gradient - expected_gradient).abs();
                pairs += 1;
            }
            if y + 1 < actual.height() && mask.includes(x, y + 1) {
                let actual_gradient = luma_difference(actual, x, y, x, y + 1);
                let expected_gradient = luma_difference(expected, x, y, x, y + 1);
                actual_gradient_sum += actual_gradient;
                expected_gradient_sum += expected_gradient;
                gradient_error_sum += (actual_gradient - expected_gradient).abs();
                pairs += 1;
            }
        }
    }
    if pairs == 0 {
        return Ok(GradientStats::default());
    }
    let actual_mean = actual_gradient_sum / pairs as f64;
    let expected_mean = expected_gradient_sum / pairs as f64;
    let retention = if expected_mean <= 1.0e-9 {
        0.0
    } else {
        actual_mean / expected_mean
    };
    Ok(GradientStats {
        edge_pairs: pairs,
        actual_mean_gradient: actual_mean,
        expected_mean_gradient: expected_mean,
        gradient_retention: retention,
        gradient_error_mean: gradient_error_sum / pairs as f64,
    })
}

fn luma_difference(image: &RgbImage, left_x: u32, left_y: u32, right_x: u32, right_y: u32) -> f64 {
    (luma(rgb_to_u32(image.get_pixel(left_x, left_y)))
        - luma(rgb_to_u32(image.get_pixel(right_x, right_y))))
    .abs()
}

fn percentile(sorted_values: &[f64], percentile: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let index = (percentile * sorted_values.len() as f64).ceil() as usize;
    sorted_values[index.saturating_sub(1).min(sorted_values.len() - 1)]
}

fn error_heatmap(
    actual: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
) -> QualityResult<RgbImage> {
    require_same_size(actual, expected, "heatmap")?;
    require_mask_size(mask, actual.width(), actual.height())?;
    let mut heatmap = RgbImage::new(actual.width(), actual.height());
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            let color = if mask.includes(x, y) {
                let delta = Lab::from_rgb(rgb_to_u32(actual.get_pixel(x, y)))
                    .ciede2000(Lab::from_rgb(rgb_to_u32(expected.get_pixel(x, y))));
                heatmap_color(delta)
            } else {
                rgb_u32(18, 18, 18)
            };
            heatmap.put_pixel(x, y, u32_to_rgb(color));
        }
    }
    Ok(heatmap)
}

fn heatmap_color(delta_e: f64) -> u32 {
    let normalized = f64::clamp(delta_e / HEATMAP_DELTA_E_CEILING, 0.0, 1.0);
    let (red, green) = if normalized < 0.5 {
        ((normalized * 2.0 * 255.0).round() as u8, 0)
    } else {
        (255, ((normalized - 0.5) * 2.0 * 255.0).round() as u8)
    };
    rgb_u32(red, green, 0)
}

fn masked_block_average(
    image: &RgbImage,
    mask: &PixelMask,
    block_size: u32,
) -> QualityResult<RgbImage> {
    if block_size == 0 {
        return Err("blockSize must be positive".to_string());
    }
    require_mask_size(mask, image.width(), image.height())?;
    let mut output = image.clone();
    let mut block_y = 0;
    while block_y < image.height() {
        let max_y = (block_y + block_size).min(image.height());
        let mut block_x = 0;
        while block_x < image.width() {
            let max_x = (block_x + block_size).min(image.width());
            let mut red = 0u64;
            let mut green = 0u64;
            let mut blue = 0u64;
            let mut count = 0u64;
            for y in block_y..max_y {
                for x in block_x..max_x {
                    if !mask.includes(x, y) {
                        continue;
                    }
                    let rgb = image.get_pixel(x, y);
                    red += u64::from(rgb[0]);
                    green += u64::from(rgb[1]);
                    blue += u64::from(rgb[2]);
                    count += 1;
                }
            }
            if count > 0 {
                let average = Rgb([
                    (red as f64 / count as f64).round() as u8,
                    (green as f64 / count as f64).round() as u8,
                    (blue as f64 / count as f64).round() as u8,
                ]);
                for y in block_y..max_y {
                    for x in block_x..max_x {
                        if mask.includes(x, y) {
                            output.put_pixel(x, y, average);
                        }
                    }
                }
            }
            block_x += block_size;
        }
        block_y += block_size;
    }
    Ok(output)
}

fn blend_images(
    left: &RgbImage,
    right: &RgbImage,
    mask: &PixelMask,
    right_weight: f64,
) -> QualityResult<RgbImage> {
    require_same_size(left, right, "blend")?;
    let mut output = left.clone();
    let left_weight = 1.0 - right_weight;
    for y in 0..left.height() {
        for x in 0..left.width() {
            if !mask.includes(x, y) {
                output.put_pixel(x, y, *right.get_pixel(x, y));
                continue;
            }
            let l = left.get_pixel(x, y);
            let r = right.get_pixel(x, y);
            output.put_pixel(
                x,
                y,
                Rgb([
                    (f64::from(l[0]) * left_weight + f64::from(r[0]) * right_weight).round() as u8,
                    (f64::from(l[1]) * left_weight + f64::from(r[1]) * right_weight).round() as u8,
                    (f64::from(l[2]) * left_weight + f64::from(r[2]) * right_weight).round() as u8,
                ]),
            );
        }
    }
    Ok(output)
}

fn select_candidate_against_current(
    current: &RgbImage,
    candidate: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
) -> QualityResult<RgbImage> {
    require_same_size(current, candidate, "selective candidate")?;
    require_same_size(current, expected, "selective candidate")?;
    let mut output = current.clone();
    for y in 0..current.height() {
        for x in 0..current.width() {
            if !mask.includes(x, y) {
                output.put_pixel(x, y, *expected.get_pixel(x, y));
                continue;
            }
            let current_delta = Lab::from_rgb(rgb_to_u32(current.get_pixel(x, y)))
                .ciede2000(Lab::from_rgb(rgb_to_u32(expected.get_pixel(x, y))));
            let candidate_delta = Lab::from_rgb(rgb_to_u32(candidate.get_pixel(x, y)))
                .ciede2000(Lab::from_rgb(rgb_to_u32(expected.get_pixel(x, y))));
            if candidate_delta + 0.03 < current_delta {
                output.put_pixel(x, y, *candidate.get_pixel(x, y));
            }
        }
    }
    Ok(output)
}

fn load_palette(path: &Path) -> QualityResult<Vec<u32>> {
    let image = read_rgb_image(path)?;
    let colors = unique_image_colors(&image);
    if colors.is_empty() {
        return Err(format!("Palette has no colors: {}", path.display()));
    }
    Ok(colors)
}

fn unique_image_colors(image: &RgbImage) -> Vec<u32> {
    let mut colors = Vec::<u32>::new();
    for pixel in image.pixels() {
        let rgb = rgb_to_u32(pixel);
        if !colors.contains(&rgb) {
            colors.push(rgb);
        }
    }
    if colors.is_empty() {
        colors.push(0);
    }
    colors
}

fn remap_nearest_rgb(
    source: &RgbImage,
    palette: &[u32],
    mask: &PixelMask,
) -> QualityResult<RgbImage> {
    if palette.is_empty() {
        return Err("palette must contain at least one color".to_string());
    }
    require_mask_size(mask, source.width(), source.height())?;
    let mut output = source.clone();
    for y in 0..source.height() {
        for x in 0..source.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let source_rgb = rgb_to_u32(source.get_pixel(x, y));
            let best = nearest_rgb(source_rgb, palette);
            output.put_pixel(x, y, u32_to_rgb(best));
        }
    }
    Ok(output)
}

fn nearest_rgb(source_rgb: u32, palette: &[u32]) -> u32 {
    palette
        .iter()
        .copied()
        .min_by(|left, right| {
            rgb_distance_squared(source_rgb, *left)
                .total_cmp(&rgb_distance_squared(source_rgb, *right))
        })
        .unwrap_or(source_rgb)
}

fn candidate_gain_heatmap(
    current: &RgbImage,
    candidate: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
) -> QualityResult<RgbImage> {
    require_same_size(current, candidate, "candidate gain")?;
    require_same_size(current, expected, "candidate gain")?;
    let mut output = RgbImage::new(current.width(), current.height());
    for y in 0..current.height() {
        for x in 0..current.width() {
            if !mask.includes(x, y) {
                output.put_pixel(x, y, Rgb([18, 18, 18]));
                continue;
            }
            let expected_lab = Lab::from_rgb(rgb_to_u32(expected.get_pixel(x, y)));
            let current_delta =
                Lab::from_rgb(rgb_to_u32(current.get_pixel(x, y))).ciede2000(expected_lab);
            let candidate_delta =
                Lab::from_rgb(rgb_to_u32(candidate.get_pixel(x, y))).ciede2000(expected_lab);
            let gain = current_delta - candidate_delta;
            let normalized = f64::clamp(gain.abs() / HEATMAP_DELTA_E_CEILING, 0.0, 1.0);
            let intensity = (normalized * 255.0).round() as u8;
            let color = if gain >= 0.0 {
                Rgb([0, intensity, 0])
            } else {
                Rgb([intensity, 0, 0])
            };
            output.put_pixel(x, y, color);
        }
    }
    Ok(output)
}

fn write_candidate_diff_csvs(
    source: &RgbImage,
    expected: &RgbImage,
    current: &RgbImage,
    candidate: &RgbImage,
    mask: &PixelMask,
    output_directory: &Path,
) -> QualityResult<()> {
    let mut summary = BTreeMap::<u32, (usize, usize, usize)>::new();
    let mut gains = Vec::<(u32, u32, f64)>::new();
    let mut losses = Vec::<(u32, u32, f64)>::new();
    for y in 0..current.height() {
        for x in 0..current.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let source_rgb = rgb_to_u32(source.get_pixel(x, y));
            let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
            let current_rgb = rgb_to_u32(current.get_pixel(x, y));
            let candidate_rgb = rgb_to_u32(candidate.get_pixel(x, y));
            let expected_lab = Lab::from_rgb(expected_rgb);
            let current_delta = Lab::from_rgb(current_rgb).ciede2000(expected_lab);
            let candidate_delta = Lab::from_rgb(candidate_rgb).ciede2000(expected_lab);
            let gain = current_delta - candidate_delta;
            let entry = summary.entry(expected_rgb).or_default();
            entry.0 += 1;
            if gain > 0.03 {
                entry.1 += 1;
                gains.push((source_rgb, expected_rgb, gain));
            } else if gain < -0.03 {
                entry.2 += 1;
                losses.push((source_rgb, expected_rgb, gain.abs()));
            }
        }
    }
    let mut summary_text =
        String::from("standardRgb,count,winnerPrimary,candidateBetter,currentBetter\n");
    for (expected_rgb, (count, candidate_better, current_better)) in summary {
        let winner = if candidate_better >= current_better {
            "candidate"
        } else {
            "current"
        };
        writeln!(
            &mut summary_text,
            "{},{},{},{},{}",
            hex(expected_rgb),
            count,
            winner,
            candidate_better,
            current_better
        )
        .unwrap();
    }
    fs::write(
        output_directory.join("production-candidate-token-diff-summary.csv"),
        summary_text,
    )
    .map_err(|error| error.to_string())?;
    gains.sort_by(|left, right| right.2.total_cmp(&left.2));
    losses.sort_by(|left, right| right.2.total_cmp(&left.2));
    fs::write(
        output_directory.join("production-candidate-top-gains.csv"),
        top_gain_loss_csv("gainDeltaE", &gains),
    )
    .map_err(|error| error.to_string())?;
    fs::write(
        output_directory.join("production-candidate-top-losses.csv"),
        top_gain_loss_csv("lossDeltaE", &losses),
    )
    .map_err(|error| error.to_string())
}

fn top_gain_loss_csv(label: &str, rows: &[(u32, u32, f64)]) -> String {
    let mut text = format!("sourceRgb,expectedRgb,{label}\n");
    for (source_rgb, expected_rgb, value) in rows.iter().take(TOP_ERROR_SAMPLE_COUNT) {
        writeln!(
            &mut text,
            "{},{},{:.6}",
            hex(*source_rgb),
            hex(*expected_rgb),
            value
        )
        .unwrap();
    }
    text
}

#[derive(Clone, Debug)]
struct CarrierCandidate {
    id: String,
    rgb: u32,
}

fn carrier_candidates(filter: &str) -> QualityResult<Vec<CarrierCandidate>> {
    let path = Path::new(filter);
    if path.is_file() {
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let mut candidates = Vec::new();
        for (line_index, line) in text.lines().enumerate() {
            if line_index == 0 && line.to_ascii_lowercase().contains("rgb") {
                continue;
            }
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let columns = split_csv_line(line);
            if columns.len() < 2 {
                continue;
            }
            candidates.push(CarrierCandidate {
                id: columns[0].clone(),
                rgb: parse_hex_rgb(&columns[1])?,
            });
        }
        if !candidates.is_empty() {
            return Ok(candidates);
        }
    }
    let named = match filter.to_ascii_lowercase().as_str() {
        "vegetated" => vec![
            ("grass", rgb_u32(88, 126, 62)),
            ("oak_leaves", rgb_u32(48, 92, 38)),
            ("moss", rgb_u32(89, 109, 45)),
        ],
        "sand" => vec![
            ("sand", rgb_u32(218, 207, 163)),
            ("sandstone", rgb_u32(196, 181, 125)),
            ("terracotta_yellow", rgb_u32(184, 132, 40)),
        ],
        "red-sand" => vec![
            ("red_sand", rgb_u32(190, 95, 33)),
            ("orange_terracotta", rgb_u32(161, 83, 37)),
            ("granite", rgb_u32(149, 103, 85)),
        ],
        "rock" => vec![
            ("stone", rgb_u32(125, 125, 125)),
            ("tuff", rgb_u32(108, 109, 102)),
            ("deepslate", rgb_u32(80, 80, 85)),
        ],
        "snow" => vec![
            ("snow", rgb_u32(249, 254, 254)),
            ("calcite", rgb_u32(223, 224, 220)),
            ("quartz", rgb_u32(235, 229, 222)),
        ],
        "wet" => vec![
            ("mud", rgb_u32(60, 54, 45)),
            ("clay", rgb_u32(161, 167, 179)),
            ("moss", rgb_u32(89, 109, 45)),
        ],
        _ => vec![
            ("black", rgb_u32(0, 0, 0)),
            ("white", rgb_u32(255, 255, 255)),
            ("gray", rgb_u32(128, 128, 128)),
            ("grass", rgb_u32(88, 126, 62)),
            ("sand", rgb_u32(218, 207, 163)),
            ("stone", rgb_u32(125, 125, 125)),
        ],
    };
    Ok(named
        .into_iter()
        .map(|(id, rgb)| CarrierCandidate {
            id: id.to_string(),
            rgb,
        })
        .collect())
}

fn parse_hex_rgb(text: &str) -> QualityResult<u32> {
    let value = text.trim().trim_start_matches('#');
    u32::from_str_radix(value, 16)
        .map(|rgb| rgb & 0x00ff_ffff)
        .map_err(|error| error.to_string())
}

fn best_carrier_remap(
    current: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
    carriers: &[CarrierCandidate],
) -> QualityResult<RgbImage> {
    require_same_size(current, expected, "carrier remap")?;
    if carriers.is_empty() {
        return Err("carrier filter produced no candidates".to_string());
    }
    let mut output = current.clone();
    for y in 0..current.height() {
        for x in 0..current.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
            let current_rgb = rgb_to_u32(current.get_pixel(x, y));
            let current_delta = Lab::from_rgb(current_rgb).ciede2000(Lab::from_rgb(expected_rgb));
            let best = carriers
                .iter()
                .min_by(|left, right| {
                    Lab::from_rgb(left.rgb)
                        .ciede2000(Lab::from_rgb(expected_rgb))
                        .total_cmp(&Lab::from_rgb(right.rgb).ciede2000(Lab::from_rgb(expected_rgb)))
                })
                .expect("non-empty candidates");
            let best_delta = Lab::from_rgb(best.rgb).ciede2000(Lab::from_rgb(expected_rgb));
            if best_delta + 0.03 < current_delta {
                output.put_pixel(x, y, u32_to_rgb(best.rgb));
            }
        }
    }
    Ok(output)
}

fn write_carrier_simulation_csv(
    current: &RgbImage,
    expected: &RgbImage,
    mask: &PixelMask,
    carriers: &[CarrierCandidate],
    top_buckets: usize,
    output_directory: &Path,
) -> QualityResult<()> {
    let mut rows = Vec::<(String, u32, usize, f64, bool)>::new();
    for candidate in carriers {
        let mut improved = 0usize;
        let mut gain_sum = 0.0;
        for y in 0..current.height() {
            for x in 0..current.width() {
                if !mask.includes(x, y) {
                    continue;
                }
                let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
                let current_delta = Lab::from_rgb(rgb_to_u32(current.get_pixel(x, y)))
                    .ciede2000(Lab::from_rgb(expected_rgb));
                let candidate_delta =
                    Lab::from_rgb(candidate.rgb).ciede2000(Lab::from_rgb(expected_rgb));
                let gain = current_delta - candidate_delta;
                if gain > 0.03 {
                    improved += 1;
                    gain_sum += gain;
                }
            }
        }
        rows.push((
            candidate.id.clone(),
            candidate.rgb,
            improved,
            gain_sum,
            improved > 0,
        ));
    }
    rows.sort_by(|left, right| right.3.total_cmp(&left.3));
    let limit = top_buckets.max(1).min(rows.len());
    let mut text = String::from("id,rgb,improvedPixels,totalGainDeltaE,positive\n");
    for (id, rgb, improved, gain, positive) in rows.into_iter().take(limit) {
        writeln!(
            &mut text,
            "{},{},{},{:.6},{}",
            id,
            hex(rgb),
            improved,
            gain,
            positive
        )
        .unwrap();
    }
    fs::write(output_directory.join("carrier-remap-simulation.csv"), text)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug)]
struct ErrorPixel {
    x: u32,
    y: u32,
    delta_e: f64,
    rgb_distance: f64,
    actual_rgb: u32,
    expected_rgb: u32,
    source_rgb: Option<u32>,
}

impl PartialEq for ErrorPixel {
    fn eq(&self, other: &Self) -> bool {
        self.delta_e == other.delta_e
    }
}

impl Eq for ErrorPixel {}

impl PartialOrd for ErrorPixel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ErrorPixel {
    fn cmp(&self, other: &Self) -> Ordering {
        self.delta_e.total_cmp(&other.delta_e).reverse()
    }
}

fn write_top_error_pixels(
    actual: &RgbImage,
    expected: &RgbImage,
    source: Option<&RgbImage>,
    path: &Path,
    mask: &PixelMask,
    limit: usize,
) -> QualityResult<()> {
    require_same_size(actual, expected, "top-error")?;
    if let Some(source) = source {
        require_same_size(actual, source, "source top-error")?;
    }
    let mut top = BinaryHeap::<ErrorPixel>::new();
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let actual_rgb = rgb_to_u32(actual.get_pixel(x, y));
            let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
            let source_rgb = source.map(|image| rgb_to_u32(image.get_pixel(x, y)));
            let delta_e = Lab::from_rgb(actual_rgb).ciede2000(Lab::from_rgb(expected_rgb));
            let pixel = ErrorPixel {
                x,
                y,
                delta_e,
                rgb_distance: rgb_distance_squared(actual_rgb, expected_rgb).sqrt(),
                actual_rgb,
                expected_rgb,
                source_rgb,
            };
            if top.len() < limit {
                top.push(pixel);
            } else if top.peek().is_some_and(|peek| delta_e > peek.delta_e) {
                top.pop();
                top.push(pixel);
            }
        }
    }
    let mut ordered = top.into_vec();
    ordered.sort_by(|left, right| right.delta_e.total_cmp(&left.delta_e));
    let mut text = String::from(
        "x,y,deltaE2000,rgbDistance,actualRgb,expectedRgb,sourceRgb,actualLuma,expectedLuma,sourceLuma\n",
    );
    for pixel in ordered {
        writeln!(
            &mut text,
            "{},{},{:.6},{:.6},{},{},{},{:.6},{:.6},{}",
            pixel.x,
            pixel.y,
            pixel.delta_e,
            pixel.rgb_distance,
            hex(pixel.actual_rgb),
            hex(pixel.expected_rgb),
            pixel
                .source_rgb
                .map(hex)
                .unwrap_or_else(|| "none".to_string()),
            luma(pixel.actual_rgb),
            luma(pixel.expected_rgb),
            pixel
                .source_rgb
                .map(|rgb| format!("{:.6}", luma(rgb)))
                .unwrap_or_else(|| "none".to_string())
        )
        .unwrap();
    }
    fs::write(path, text).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Default)]
struct ErrorBucket {
    count: usize,
    sum_delta_e: f64,
    max_delta_e: f64,
    red_sum: u64,
    green_sum: u64,
    blue_sum: u64,
    source_red_sum: u64,
    source_green_sum: u64,
    source_blue_sum: u64,
    sum_source_expected_delta_e: f64,
    sum_actual_source_delta_e: f64,
}

impl ErrorBucket {
    fn add(&mut self, actual_rgb: u32, expected_rgb: u32, source_rgb: Option<u32>, delta_e: f64) {
        self.count += 1;
        self.sum_delta_e += delta_e;
        self.max_delta_e = f64::max(self.max_delta_e, delta_e);
        self.red_sum += u64::from(red(actual_rgb));
        self.green_sum += u64::from(green(actual_rgb));
        self.blue_sum += u64::from(blue(actual_rgb));
        if let Some(source_rgb) = source_rgb {
            self.source_red_sum += u64::from(red(source_rgb));
            self.source_green_sum += u64::from(green(source_rgb));
            self.source_blue_sum += u64::from(blue(source_rgb));
            let source_lab = Lab::from_rgb(source_rgb);
            self.sum_source_expected_delta_e += source_lab.ciede2000(Lab::from_rgb(expected_rgb));
            self.sum_actual_source_delta_e += Lab::from_rgb(actual_rgb).ciede2000(source_lab);
        }
    }

    fn mean_actual_rgb(&self) -> u32 {
        rgb_u32(
            (self.red_sum as f64 / self.count as f64).round() as u8,
            (self.green_sum as f64 / self.count as f64).round() as u8,
            (self.blue_sum as f64 / self.count as f64).round() as u8,
        )
    }

    fn mean_source_rgb(&self) -> u32 {
        rgb_u32(
            (self.source_red_sum as f64 / self.count as f64).round() as u8,
            (self.source_green_sum as f64 / self.count as f64).round() as u8,
            (self.source_blue_sum as f64 / self.count as f64).round() as u8,
        )
    }
}

fn write_palette_error_summary(
    actual: &RgbImage,
    expected: &RgbImage,
    source: Option<&RgbImage>,
    path: &Path,
    mask: &PixelMask,
) -> QualityResult<()> {
    require_same_size(actual, expected, "palette summary")?;
    if let Some(source) = source {
        require_same_size(actual, source, "source summary")?;
    }
    let mut buckets = BTreeMap::<u32, ErrorBucket>::new();
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            if !mask.includes(x, y) {
                continue;
            }
            let actual_rgb = rgb_to_u32(actual.get_pixel(x, y));
            let expected_rgb = rgb_to_u32(expected.get_pixel(x, y));
            let source_rgb = source.map(|image| rgb_to_u32(image.get_pixel(x, y)));
            let delta_e = Lab::from_rgb(actual_rgb).ciede2000(Lab::from_rgb(expected_rgb));
            buckets.entry(expected_rgb).or_default().add(
                actual_rgb,
                expected_rgb,
                source_rgb,
                delta_e,
            );
        }
    }
    let mut ordered = buckets.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|(_, left), (_, right)| right.sum_delta_e.total_cmp(&left.sum_delta_e));
    let mut text = String::new();
    if source.is_some() {
        text.push_str(
            "expectedRgb,count,meanDeltaE2000,maxDeltaE2000,contributionDeltaE,meanActualRgb,meanSourceRgb,meanSourceVsExpectedDeltaE,meanActualVsSourceDeltaE\n",
        );
    } else {
        text.push_str(
            "expectedRgb,count,meanDeltaE2000,maxDeltaE2000,contributionDeltaE,meanActualRgb\n",
        );
    }
    for (expected_rgb, bucket) in ordered {
        write!(
            &mut text,
            "{},{},{:.6},{:.6},{:.6},{}",
            hex(expected_rgb),
            bucket.count,
            bucket.sum_delta_e / bucket.count as f64,
            bucket.max_delta_e,
            bucket.sum_delta_e,
            hex(bucket.mean_actual_rgb())
        )
        .unwrap();
        if source.is_some() {
            write!(
                &mut text,
                ",{},{:.6},{:.6}",
                hex(bucket.mean_source_rgb()),
                bucket.sum_source_expected_delta_e / bucket.count as f64,
                bucket.sum_actual_source_delta_e / bucket.count as f64
            )
            .unwrap();
        }
        text.push('\n');
    }
    fs::write(path, text).map_err(|error| error.to_string())
}

fn percent(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 * 100.0 / total as f64
    }
}

fn rgb_to_u32(rgb: &Rgb<u8>) -> u32 {
    rgb_u32(rgb[0], rgb[1], rgb[2])
}

fn u32_to_rgb(rgb: u32) -> Rgb<u8> {
    Rgb([red(rgb), green(rgb), blue(rgb)])
}

fn rgb_u32(red: u8, green: u8, blue: u8) -> u32 {
    (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
}

fn red(rgb: u32) -> u8 {
    ((rgb >> 16) & 0xff) as u8
}

fn green(rgb: u32) -> u8 {
    ((rgb >> 8) & 0xff) as u8
}

fn blue(rgb: u32) -> u8 {
    (rgb & 0xff) as u8
}

fn hex(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0x00ff_ffff)
}

fn rgb_distance_squared(left: u32, right: u32) -> f64 {
    let red = i32::from(red(left)) - i32::from(red(right));
    let green = i32::from(green(left)) - i32::from(green(right));
    let blue = i32::from(blue(left)) - i32::from(blue(right));
    f64::from((red * red) + (green * green) + (blue * blue))
}

fn luma(rgb: u32) -> f64 {
    0.2126 * f64::from(red(rgb)) + 0.7152 * f64::from(green(rgb)) + 0.0722 * f64::from(blue(rgb))
}

#[derive(Clone, Copy, Debug)]
struct Lab {
    l: f64,
    a: f64,
    b: f64,
}

impl Lab {
    fn from_rgb(rgb: u32) -> Self {
        let red = linear_rgb(red(rgb));
        let green = linear_rgb(green(rgb));
        let blue = linear_rgb(blue(rgb));
        let x = red * 0.4124564 + green * 0.3575761 + blue * 0.1804375;
        let y = red * 0.2126729 + green * 0.7151522 + blue * 0.0721750;
        let z = red * 0.0193339 + green * 0.1191920 + blue * 0.9503041;
        let fx = lab_pivot(x / 0.95047);
        let fy = lab_pivot(y);
        let fz = lab_pivot(z / 1.08883);
        Self {
            l: 116.0 * fy - 16.0,
            a: 500.0 * (fx - fy),
            b: 200.0 * (fy - fz),
        }
    }

    fn ciede2000(self, other: Self) -> f64 {
        let c1 = self.a.hypot(self.b);
        let c2 = other.a.hypot(other.b);
        let average_c = (c1 + c2) * 0.5;
        let average_c7 = average_c.powf(7.0);
        let g = 0.5 * (1.0 - (average_c7 / (average_c7 + 25.0_f64.powf(7.0))).sqrt());
        let a1_prime = (1.0 + g) * self.a;
        let a2_prime = (1.0 + g) * other.a;
        let c1_prime = a1_prime.hypot(self.b);
        let c2_prime = a2_prime.hypot(other.b);
        let h1_prime = hue_degrees(a1_prime, self.b);
        let h2_prime = hue_degrees(a2_prime, other.b);
        let delta_l_prime = other.l - self.l;
        let delta_c_prime = c2_prime - c1_prime;
        let delta_h_prime = delta_hue_prime(c1_prime, c2_prime, h1_prime, h2_prime);
        let delta_h = 2.0 * (c1_prime * c2_prime).sqrt() * (delta_h_prime * 0.5).to_radians().sin();
        let average_l_prime = (self.l + other.l) * 0.5;
        let average_c_prime = (c1_prime + c2_prime) * 0.5;
        let average_h_prime = average_hue_prime(c1_prime, c2_prime, h1_prime, h2_prime);
        let t = 1.0 - 0.17 * (average_h_prime - 30.0).to_radians().cos()
            + 0.24 * (2.0 * average_h_prime).to_radians().cos()
            + 0.32 * (3.0 * average_h_prime + 6.0).to_radians().cos()
            - 0.20 * (4.0 * average_h_prime - 63.0).to_radians().cos();
        let delta_theta = 30.0 * (-((average_h_prime - 275.0) / 25.0).powf(2.0)).exp();
        let average_c_prime7 = average_c_prime.powf(7.0);
        let r_c = 2.0 * (average_c_prime7 / (average_c_prime7 + 25.0_f64.powf(7.0))).sqrt();
        let l_offset = average_l_prime - 50.0;
        let s_l = 1.0 + (0.015 * l_offset * l_offset) / (20.0 + l_offset * l_offset).sqrt();
        let s_c = 1.0 + 0.045 * average_c_prime;
        let s_h = 1.0 + 0.015 * average_c_prime * t;
        let r_t = -(2.0 * delta_theta).to_radians().sin() * r_c;
        let l_term = delta_l_prime / s_l;
        let c_term = delta_c_prime / s_c;
        let h_term = delta_h / s_h;
        (l_term * l_term + c_term * c_term + h_term * h_term + r_t * c_term * h_term).sqrt()
    }
}

fn linear_rgb(value: u8) -> f64 {
    let normalized = f64::from(value) / 255.0;
    if normalized <= 0.04045 {
        normalized / 12.92
    } else {
        ((normalized + 0.055) / 1.055).powf(2.4)
    }
}

fn lab_pivot(value: f64) -> f64 {
    let epsilon = 216.0 / 24389.0;
    let kappa = 24389.0 / 27.0;
    if value > epsilon {
        value.cbrt()
    } else {
        (kappa * value + 16.0) / 116.0
    }
}

fn hue_degrees(a: f64, b: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let hue = b.atan2(a).to_degrees();
    if hue >= 0.0 {
        hue
    } else {
        hue + 360.0
    }
}

fn delta_hue_prime(c1_prime: f64, c2_prime: f64, h1_prime: f64, h2_prime: f64) -> f64 {
    if c1_prime * c2_prime == 0.0 {
        return 0.0;
    }
    let delta = h2_prime - h1_prime;
    if delta.abs() <= 180.0 {
        delta
    } else if delta > 180.0 {
        delta - 360.0
    } else {
        delta + 360.0
    }
}

fn average_hue_prime(c1_prime: f64, c2_prime: f64, h1_prime: f64, h2_prime: f64) -> f64 {
    if c1_prime * c2_prime == 0.0 {
        return h1_prime + h2_prime;
    }
    let difference = (h1_prime - h2_prime).abs();
    if difference <= 180.0 {
        (h1_prime + h2_prime) * 0.5
    } else if h1_prime + h2_prime < 360.0 {
        (h1_prime + h2_prime + 360.0) * 0.5
    } else {
        (h1_prime + h2_prime - 360.0) * 0.5
    }
}

trait NormalizeLexically {
    fn normalize_lexically(self) -> Self;
}

impl NormalizeLexically for PathBuf {
    fn normalize_lexically(self) -> Self {
        let mut normalized = PathBuf::new();
        for component in self.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    }
}
