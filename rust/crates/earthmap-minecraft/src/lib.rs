#![forbid(unsafe_code)]

pub mod block_state_ids;
pub mod chunk_generation_status;
pub mod chunk_model;
pub mod chunk_nbt_encoder;
pub mod dimension_profile;
pub mod heightmap;
pub mod heightmap_calculator;
pub mod level_dat_template;
pub mod nbt;
pub mod nbt_parity_fixtures;
pub mod packed_long_array;
pub mod section_palette;

use std::fmt;

pub const MODULE_STATUS: &str = "phase2-minecraft-core-chunk-nbt-bootstrap";

pub type Result<T> = std::result::Result<T, MinecraftError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinecraftError {
    message: String,
}

impl MinecraftError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MinecraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MinecraftError {}
