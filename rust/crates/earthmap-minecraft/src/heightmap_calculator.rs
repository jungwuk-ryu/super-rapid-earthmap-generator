use crate::chunk_model::{ChunkModel, CHUNK_WIDTH};
use crate::heightmap::{column_index, Heightmap, COLUMN_COUNT};
use crate::{MinecraftError, Result};

pub fn compute_top_y_exclusive(
    chunk: &ChunkModel,
    mut counts_for_heightmap: impl FnMut(i32) -> bool,
) -> Result<Heightmap> {
    let dimension = chunk.dimension().clone();
    let mut values = vec![dimension.min_y(); COLUMN_COUNT];
    for local_z in 0..CHUNK_WIDTH {
        for local_x in 0..CHUNK_WIDTH {
            values[column_index(local_x, local_z)?] = top_y_exclusive_for_column(
                chunk,
                local_x as i32,
                local_z as i32,
                &mut counts_for_heightmap,
            )?;
        }
    }
    Heightmap::new(dimension, values)
}

fn top_y_exclusive_for_column(
    chunk: &ChunkModel,
    local_x: i32,
    local_z: i32,
    counts_for_heightmap: &mut impl FnMut(i32) -> bool,
) -> Result<i32> {
    let dimension = chunk.dimension();
    for y in (dimension.min_y()..=dimension.max_y_inclusive()).rev() {
        if counts_for_heightmap(chunk.get_block_state_id(local_x, y, local_z)?) {
            return Ok(y + 1);
        }
    }
    Ok(dimension.min_y())
}

pub fn compute_top_y_exclusive_checked(
    chunk: Option<&ChunkModel>,
    counts_for_heightmap: Option<impl FnMut(i32) -> bool>,
) -> Result<Heightmap> {
    let chunk = chunk.ok_or_else(|| MinecraftError::invalid("chunk must not be null"))?;
    let counts_for_heightmap = counts_for_heightmap
        .ok_or_else(|| MinecraftError::invalid("countsForHeightmap must not be null"))?;
    compute_top_y_exclusive(chunk, counts_for_heightmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_state_ids::{AIR, STONE, WATER};

    #[test]
    fn empty_chunk_heightmap_matches_java() {
        let chunk = ChunkModel::overworld(0, 0);
        let heightmap = compute_top_y_exclusive(&chunk, |state| state != AIR).unwrap();

        assert_eq!(heightmap.top_y_exclusive_at(0, 0).unwrap(), -64);
        assert_eq!(heightmap.storage_value_at(0, 0).unwrap(), 0);
        assert_eq!(heightmap.bits_per_storage_value(), 9);
    }

    #[test]
    fn min_and_max_build_height_match_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_block_state_id(0, -64, 0, STONE).unwrap();
        chunk.set_block_state_id(15, 319, 15, STONE).unwrap();

        let heightmap = compute_top_y_exclusive(&chunk, |state| state != AIR).unwrap();

        assert_eq!(heightmap.top_y_exclusive_at(0, 0).unwrap(), -63);
        assert_eq!(heightmap.storage_value_at(0, 0).unwrap(), 1);
        assert_eq!(heightmap.top_y_exclusive_at(15, 15).unwrap(), 320);
        assert_eq!(heightmap.storage_value_at(15, 15).unwrap(), 384);
    }

    #[test]
    fn mixed_columns_and_predicate_match_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_block_state_id(2, 10, 3, STONE).unwrap();
        chunk.set_block_state_id(2, 11, 3, WATER).unwrap();

        let solid_only = compute_top_y_exclusive(&chunk, |state| state == STONE).unwrap();
        let any_non_air = compute_top_y_exclusive(&chunk, |state| state != AIR).unwrap();

        assert_eq!(solid_only.top_y_exclusive_at(2, 3).unwrap(), 11);
        assert_eq!(any_non_air.top_y_exclusive_at(2, 3).unwrap(), 12);
    }

    #[test]
    fn packed_storage_values_match_java_heightmap_test() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_block_state_id(0, -64, 0, STONE).unwrap();
        chunk.set_block_state_id(1, 0, 0, STONE).unwrap();
        chunk.set_block_state_id(15, 319, 15, STONE).unwrap();

        let heightmap = compute_top_y_exclusive(&chunk, |state| state != AIR).unwrap();
        let packed = heightmap.pack_storage_values().unwrap();

        assert_eq!(packed.value_count(), COLUMN_COUNT);
        assert_eq!(packed.bits_per_value(), 9);
        assert_eq!(packed.data_word_count(), 37);
        assert_eq!(packed.get(0).unwrap(), 1);
        assert_eq!(packed.get(1).unwrap(), 65);
        assert_eq!(packed.get(255).unwrap(), 384);
    }

    #[test]
    fn checked_api_matches_java_null_validation_cases() {
        assert!(compute_top_y_exclusive_checked(None, Some(|state| state != AIR)).is_err());
        let chunk = ChunkModel::overworld(0, 0);
        assert!(
            compute_top_y_exclusive_checked(Some(&chunk), Option::<fn(i32) -> bool>::None).is_err()
        );
    }
}
