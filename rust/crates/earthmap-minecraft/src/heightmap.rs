use crate::chunk_model::CHUNK_WIDTH;
use crate::dimension_profile::DimensionProfile;
use crate::packed_long_array::PackedLongArray;
use crate::{MinecraftError, Result};

pub const COLUMN_COUNT: usize = CHUNK_WIDTH * CHUNK_WIDTH;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heightmap {
    dimension: DimensionProfile,
    top_y_exclusive_by_column: Vec<i32>,
}

impl Heightmap {
    pub fn new(dimension: DimensionProfile, top_y_exclusive_by_column: Vec<i32>) -> Result<Self> {
        if top_y_exclusive_by_column.len() != COLUMN_COUNT {
            return Err(MinecraftError::invalid(format!(
                "heightmap must contain {COLUMN_COUNT} columns"
            )));
        }
        for value in &top_y_exclusive_by_column {
            require_top_y_exclusive(&dimension, *value)?;
        }
        Ok(Self {
            dimension,
            top_y_exclusive_by_column,
        })
    }

    pub fn dimension(&self) -> &DimensionProfile {
        &self.dimension
    }

    pub fn top_y_exclusive_at(&self, local_x: usize, local_z: usize) -> Result<i32> {
        Ok(self.top_y_exclusive_by_column[column_index(local_x, local_z)?])
    }

    pub fn storage_value_at(&self, local_x: usize, local_z: usize) -> Result<i32> {
        Ok(self.top_y_exclusive_at(local_x, local_z)? - self.dimension.min_y())
    }

    pub fn bits_per_storage_value(&self) -> u8 {
        bits_required_for_max_value(self.dimension.height()).expect("validated positive height")
    }

    pub fn pack_storage_values(&self) -> Result<PackedLongArray> {
        let bits_per_value = self.bits_per_storage_value();
        PackedLongArray::pack(COLUMN_COUNT, bits_per_value, |index| {
            self.top_y_exclusive_by_column[index] - self.dimension.min_y()
        })
    }

    pub fn copy_top_y_exclusive_values(&self) -> Vec<i32> {
        self.top_y_exclusive_by_column.clone()
    }
}

pub fn column_index(local_x: usize, local_z: usize) -> Result<usize> {
    require_local_xz(local_x, local_z)?;
    Ok((local_z * CHUNK_WIDTH) + local_x)
}

pub fn bits_required_for_max_value(max_value_inclusive: i32) -> Result<u8> {
    if max_value_inclusive < 0 {
        return Err(MinecraftError::invalid(
            "maxValueInclusive must be non-negative",
        ));
    }
    if max_value_inclusive == 0 {
        return Ok(0);
    }
    Ok((i32::BITS - max_value_inclusive.leading_zeros()) as u8)
}

fn require_top_y_exclusive(dimension: &DimensionProfile, top_y_exclusive: i32) -> Result<()> {
    if top_y_exclusive < dimension.min_y() || top_y_exclusive > dimension.max_y_exclusive() {
        return Err(MinecraftError::invalid(format!(
            "topYExclusive {top_y_exclusive} outside {}..{}",
            dimension.min_y(),
            dimension.max_y_exclusive()
        )));
    }
    Ok(())
}

fn require_local_xz(local_x: usize, local_z: usize) -> Result<()> {
    if local_x >= CHUNK_WIDTH {
        return Err(MinecraftError::invalid(format!(
            "localX outside 0..15: {local_x}"
        )));
    }
    if local_z >= CHUNK_WIDTH {
        return Err(MinecraftError::invalid(format!(
            "localZ outside 0..15: {local_z}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dimension_profile::OVERWORLD_1_21_11;

    #[test]
    fn empty_chunk_values_match_java_heightmap_shape() {
        let values = vec![OVERWORLD_1_21_11.min_y(); COLUMN_COUNT];
        let heightmap = Heightmap::new(OVERWORLD_1_21_11.clone(), values).unwrap();

        assert_eq!(heightmap.top_y_exclusive_at(0, 0).unwrap(), -64);
        assert_eq!(heightmap.storage_value_at(0, 0).unwrap(), 0);
        assert_eq!(heightmap.bits_per_storage_value(), 9);
    }

    #[test]
    fn min_and_max_storage_values_match_java() {
        let mut values = vec![OVERWORLD_1_21_11.min_y(); COLUMN_COUNT];
        values[column_index(0, 0).unwrap()] = -63;
        values[column_index(15, 15).unwrap()] = 320;
        let heightmap = Heightmap::new(OVERWORLD_1_21_11.clone(), values).unwrap();

        assert_eq!(heightmap.top_y_exclusive_at(0, 0).unwrap(), -63);
        assert_eq!(heightmap.storage_value_at(0, 0).unwrap(), 1);
        assert_eq!(heightmap.top_y_exclusive_at(15, 15).unwrap(), 320);
        assert_eq!(heightmap.storage_value_at(15, 15).unwrap(), 384);
    }

    #[test]
    fn packed_storage_values_match_java_test_cases() {
        let mut values = vec![OVERWORLD_1_21_11.min_y(); COLUMN_COUNT];
        values[0] = -63;
        values[1] = 1;
        values[255] = 320;
        let heightmap = Heightmap::new(OVERWORLD_1_21_11.clone(), values).unwrap();

        let packed = heightmap.pack_storage_values().unwrap();

        assert_eq!(packed.value_count(), COLUMN_COUNT);
        assert_eq!(packed.bits_per_value(), 9);
        assert_eq!(packed.data_word_count(), 37);
        assert_eq!(packed.get(0).unwrap(), 1);
        assert_eq!(packed.get(1).unwrap(), 65);
        assert_eq!(packed.get(255).unwrap(), 384);
    }

    #[test]
    fn validation_matches_java_heightmap_constructor() {
        assert_eq!(OVERWORLD_1_21_11.max_y_exclusive(), 320);
        assert!(Heightmap::new(OVERWORLD_1_21_11.clone(), vec![0; 1]).is_err());

        let mut too_low = vec![OVERWORLD_1_21_11.min_y(); COLUMN_COUNT];
        too_low[0] = -65;
        assert!(Heightmap::new(OVERWORLD_1_21_11.clone(), too_low).is_err());

        let mut too_high = vec![OVERWORLD_1_21_11.min_y(); COLUMN_COUNT];
        too_high[0] = 321;
        assert!(Heightmap::new(OVERWORLD_1_21_11.clone(), too_high).is_err());

        assert!(column_index(16, 0).is_err());
        assert!(column_index(0, 16).is_err());
        assert_eq!(bits_required_for_max_value(0).unwrap(), 0);
        assert_eq!(bits_required_for_max_value(384).unwrap(), 9);
        assert!(bits_required_for_max_value(-1).is_err());
    }
}
