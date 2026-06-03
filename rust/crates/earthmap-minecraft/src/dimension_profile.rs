use std::borrow::Cow;

use crate::{MinecraftError, Result};

pub const SECTION_HEIGHT: i32 = 16;
pub const OVERWORLD_1_21_11: DimensionProfile = DimensionProfile {
    id: Cow::Borrowed("minecraft:overworld"),
    min_y: -64,
    height: 384,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DimensionProfile {
    id: Cow<'static, str>,
    min_y: i32,
    height: i32,
}

impl DimensionProfile {
    pub fn new(id: impl Into<Cow<'static, str>>, min_y: i32, height: i32) -> Result<Self> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(MinecraftError::invalid("dimension id must not be blank"));
        }
        if height <= 0 {
            return Err(MinecraftError::invalid(format!(
                "height must be positive: {height}"
            )));
        }
        if height % SECTION_HEIGHT != 0 {
            return Err(MinecraftError::invalid(format!(
                "height must be section-aligned: {height}"
            )));
        }
        if min_y % SECTION_HEIGHT != 0 {
            return Err(MinecraftError::invalid(format!(
                "minY must be section-aligned: {min_y}"
            )));
        }
        if i64::from(min_y) + i64::from(height) > i64::from(i32::MAX) {
            return Err(MinecraftError::invalid(
                "dimension height overflows integer Y range",
            ));
        }
        Ok(Self { id, min_y, height })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn max_y_inclusive(&self) -> i32 {
        self.min_y + self.height - 1
    }

    pub fn max_y_exclusive(&self) -> i32 {
        self.min_y + self.height
    }

    pub fn section_count(&self) -> i32 {
        self.height / SECTION_HEIGHT
    }

    pub fn min_section_y(&self) -> i32 {
        self.min_y.div_euclid(SECTION_HEIGHT)
    }

    pub fn max_section_y_inclusive(&self) -> i32 {
        self.min_section_y() + self.section_count() - 1
    }

    pub fn contains_block_y(&self, y: i32) -> bool {
        y >= self.min_y && y <= self.max_y_inclusive()
    }

    pub fn section_index_for_block_y(&self, y: i32) -> Result<usize> {
        self.require_block_y(y)?;
        Ok(((y - self.min_y) / SECTION_HEIGHT) as usize)
    }

    pub fn section_y_for_index(&self, section_index: usize) -> Result<i32> {
        self.require_section_index(section_index)?;
        Ok(self.min_section_y() + section_index as i32)
    }

    pub fn require_block_y(&self, y: i32) -> Result<()> {
        if !self.contains_block_y(y) {
            return Err(MinecraftError::invalid(format!(
                "Y {y} outside {} build range {}..{}",
                self.id,
                self.min_y,
                self.max_y_inclusive()
            )));
        }
        Ok(())
    }

    pub fn require_section_index(&self, section_index: usize) -> Result<()> {
        let section_count = self.section_count() as usize;
        if section_index >= section_count {
            return Err(MinecraftError::invalid(format!(
                "sectionIndex {section_index} outside 0..{}",
                section_count - 1
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overworld_shape_matches_java_1_21_11() {
        let profile = &OVERWORLD_1_21_11;
        assert_eq!(profile.id(), "minecraft:overworld");
        assert_eq!(profile.min_y(), -64);
        assert_eq!(profile.height(), 384);
        assert_eq!(profile.max_y_inclusive(), 319);
        assert_eq!(profile.max_y_exclusive(), 320);
        assert_eq!(profile.section_count(), 24);
        assert_eq!(profile.min_section_y(), -4);
        assert_eq!(profile.max_section_y_inclusive(), 19);
    }

    #[test]
    fn block_and_section_indexing_match_java() {
        let profile = &OVERWORLD_1_21_11;
        assert!(profile.contains_block_y(-64));
        assert!(profile.contains_block_y(319));
        assert!(!profile.contains_block_y(320));
        assert_eq!(profile.section_index_for_block_y(-64).unwrap(), 0);
        assert_eq!(profile.section_index_for_block_y(319).unwrap(), 23);
        assert_eq!(profile.section_y_for_index(0).unwrap(), -4);
        assert_eq!(profile.section_y_for_index(23).unwrap(), 19);
        assert!(profile.section_index_for_block_y(-65).is_err());
        assert!(profile.section_y_for_index(24).is_err());
    }

    #[test]
    fn validation_matches_java_record_constructor() {
        assert!(DimensionProfile::new("minecraft:test", -64, 384).is_ok());
        assert_eq!(
            DimensionProfile::new(String::from("minecraft:runtime"), -64, 384)
                .unwrap()
                .id(),
            "minecraft:runtime"
        );
        assert!(DimensionProfile::new("", -64, 384).is_err());
        assert!(DimensionProfile::new("minecraft:test", -63, 384).is_err());
        assert!(DimensionProfile::new("minecraft:test", -64, 383).is_err());
        assert!(DimensionProfile::new("minecraft:test", -64, 0).is_err());
    }
}
