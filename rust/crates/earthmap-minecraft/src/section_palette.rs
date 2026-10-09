use crate::block_state_ids;
use crate::chunk_model::SECTION_BLOCK_COUNT;
use crate::packed_long_array::PackedLongArray;
use crate::{MinecraftError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionPalette {
    palette_block_state_ids: Vec<i32>,
    bits_per_entry: u8,
    packed_indices: PackedLongArray,
}

impl SectionPalette {
    pub(crate) fn uniform(block_state_id: i32) -> Result<Self> {
        block_state_ids::require_valid(block_state_id)?;
        Self::new(
            vec![block_state_id],
            0,
            PackedLongArray::from_words(SECTION_BLOCK_COUNT, 0, Vec::new())?,
        )
    }

    fn new(
        palette_block_state_ids: Vec<i32>,
        bits_per_entry: u8,
        packed_indices: PackedLongArray,
    ) -> Result<Self> {
        if palette_block_state_ids.is_empty() {
            return Err(MinecraftError::invalid(
                "section palette must contain at least one entry",
            ));
        }
        if packed_indices.value_count() != SECTION_BLOCK_COUNT {
            return Err(MinecraftError::invalid(
                "packed section must contain 4096 entries",
            ));
        }
        Ok(Self {
            palette_block_state_ids,
            bits_per_entry,
            packed_indices,
        })
    }

    pub fn pack_block_states(block_state_ids: &[i32]) -> Result<Self> {
        if block_state_ids.len() != SECTION_BLOCK_COUNT {
            return Err(MinecraftError::invalid(format!(
                "section blockStateIds length must be {SECTION_BLOCK_COUNT}"
            )));
        }

        let first = block_state_ids[0];
        block_state_ids::require_valid(first)?;
        let first_run_end = block_state_ids
            .iter()
            .position(|&block| block != first)
            .unwrap_or(SECTION_BLOCK_COUNT);
        if first_run_end == SECTION_BLOCK_COUNT {
            // Solid underground sections need neither an index buffer nor a
            // second 4096-value pass to validate the known zero indices.
            return Self::uniform(first);
        }

        let mut palette = Vec::<i32>::with_capacity(16);
        palette.push(first);
        // A section has at most 4096 distinct states. The bounded stack array
        // avoids an allocation, and long runs need one palette lookup each.
        let mut indices = [0u16; SECTION_BLOCK_COUNT];
        let mut common_indices = [usize::MAX; 128];
        if let Some(index) = common_indices.get_mut(first as usize) {
            *index = 0;
        }

        let mut run_start = first_run_end;
        while run_start < SECTION_BLOCK_COUNT {
            let block_state_id = block_state_ids[run_start];
            block_state_ids::require_valid(block_state_id)?;
            let cached = common_indices.get(block_state_id as usize).copied();
            let existing = match cached {
                Some(usize::MAX) => None,
                Some(index) => Some(index),
                None => palette
                    .iter()
                    .position(|candidate| *candidate == block_state_id),
            };
            let palette_index = match existing {
                Some(existing) => existing,
                None => {
                    palette.push(block_state_id);
                    if let Some(index) = common_indices.get_mut(block_state_id as usize) {
                        *index = palette.len() - 1;
                    }
                    palette.len() - 1
                }
            };
            let run_end = block_state_ids[run_start + 1..]
                .iter()
                .position(|&block| block != block_state_id)
                .map_or(SECTION_BLOCK_COUNT, |offset| run_start + 1 + offset);
            indices[run_start..run_end].fill(palette_index as u16);
            run_start = run_end;
        }

        let bits_per_entry = bits_per_entry_for_palette_size(palette.len())?;
        let packed_indices = if bits_per_entry == 4 {
            // Each index has already been bounded by a palette of 2..=16
            // entries. Pack sixteen nibbles per word, with no padding bits.
            let words = indices
                .as_chunks::<16>()
                .0
                .iter()
                .map(|values| {
                    let mut word = 0;
                    for (index, &value) in values.iter().enumerate() {
                        word |= u64::from(value) << (index * 4);
                    }
                    word
                })
                .collect();
            PackedLongArray::from_words(SECTION_BLOCK_COUNT, 4, words)?
        } else {
            PackedLongArray::pack(SECTION_BLOCK_COUNT, bits_per_entry, |index| {
                i32::from(indices[index])
            })?
        };
        Self::new(palette, bits_per_entry, packed_indices)
    }

    pub fn palette_size(&self) -> usize {
        self.palette_block_state_ids.len()
    }

    pub(crate) fn palette_block_state_ids(&self) -> &[i32] {
        &self.palette_block_state_ids
    }

    pub(crate) fn packed_data_words(&self) -> &[u64] {
        self.packed_indices.data_words()
    }

    pub fn bits_per_entry(&self) -> u8 {
        self.bits_per_entry
    }

    pub fn data_word_count(&self) -> usize {
        self.packed_indices.data_word_count()
    }

    pub fn copy_palette_block_state_ids(&self) -> Vec<i32> {
        self.palette_block_state_ids.clone()
    }

    pub fn copy_packed_data(&self) -> Vec<i64> {
        self.packed_indices.copy_data()
    }

    pub fn palette_index_at(&self, block_index: usize) -> Result<usize> {
        let palette_index = self.packed_indices.get(block_index)? as usize;
        if palette_index >= self.palette_block_state_ids.len() {
            return Err(MinecraftError::invalid(format!(
                "packed palette index outside palette: {palette_index}"
            )));
        }
        Ok(palette_index)
    }

    pub fn block_state_id_at(&self, block_index: usize) -> Result<i32> {
        Ok(self.palette_block_state_ids[self.palette_index_at(block_index)?])
    }
}

pub fn bits_per_entry_for_palette_size(palette_size: usize) -> Result<u8> {
    if palette_size == 0 {
        return Err(MinecraftError::invalid("paletteSize must be positive: 0"));
    }
    if palette_size == 1 {
        return Ok(0);
    }
    let required_bits = usize::BITS - (palette_size - 1).leading_zeros();
    Ok(4.max(required_bits as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_state_ids::{AIR, STONE, WATER};
    use crate::chunk_model::ChunkModel;

    const SAND_LIKE_TEST_ID: i32 = 3;

    #[test]
    fn bits_per_entry_matches_java_thresholds() {
        assert_eq!(bits_per_entry_for_palette_size(1).unwrap(), 0);
        assert_eq!(bits_per_entry_for_palette_size(2).unwrap(), 4);
        assert_eq!(bits_per_entry_for_palette_size(16).unwrap(), 4);
        assert_eq!(bits_per_entry_for_palette_size(17).unwrap(), 5);
        assert!(bits_per_entry_for_palette_size(0).is_err());
    }

    #[test]
    fn all_air_section_matches_java_palette_shape() {
        let states = vec![AIR; SECTION_BLOCK_COUNT];
        let section = SectionPalette::pack_block_states(&states).unwrap();

        assert_eq!(section.palette_size(), 1);
        assert_eq!(section.bits_per_entry(), 0);
        assert_eq!(section.data_word_count(), 0);
        assert_eq!(section.copy_palette_block_state_ids(), vec![AIR]);
        assert_eq!(section.block_state_id_at(0).unwrap(), AIR);
        assert_eq!(section.block_state_id_at(4095).unwrap(), AIR);
    }

    #[test]
    fn first_seen_palette_order_and_round_trip_match_java() {
        let mut states = vec![AIR; SECTION_BLOCK_COUNT];
        states[0] = STONE;
        states[1] = WATER;
        states[2] = STONE;
        states[3] = SAND_LIKE_TEST_ID;

        let section = SectionPalette::pack_block_states(&states).unwrap();

        assert_eq!(section.palette_size(), 4);
        assert_eq!(section.bits_per_entry(), 4);
        assert_eq!(
            section.copy_palette_block_state_ids(),
            vec![STONE, WATER, SAND_LIKE_TEST_ID, AIR]
        );
        assert_eq!(section.block_state_id_at(0).unwrap(), STONE);
        assert_eq!(section.block_state_id_at(1).unwrap(), WATER);
        assert_eq!(section.block_state_id_at(2).unwrap(), STONE);
        assert_eq!(section.block_state_id_at(3).unwrap(), SAND_LIKE_TEST_ID);
        assert_eq!(section.block_state_id_at(4).unwrap(), AIR);
    }

    #[test]
    fn packs_chunk_model_section_order_like_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_block_state_id(0, -64, 0, STONE).unwrap();
        chunk.set_block_state_id(1, -64, 0, WATER).unwrap();

        let section =
            SectionPalette::pack_block_states(&chunk.copy_section_block_state_ids(0).unwrap())
                .unwrap();

        assert_eq!(section.block_state_id_at(0).unwrap(), STONE);
        assert_eq!(section.block_state_id_at(1).unwrap(), WATER);
        assert_eq!(section.block_state_id_at(2).unwrap(), AIR);
    }

    #[test]
    fn optimized_palettes_preserve_large_ids_and_first_seen_packed_indices() {
        for palette_size in [1, 2, 16, 17, 80, 129] {
            let ids = (0..palette_size)
                .map(|index| {
                    if index % 3 == 0 {
                        i32::MAX - index
                    } else {
                        index
                    }
                })
                .collect::<Vec<_>>();
            let states = (0..SECTION_BLOCK_COUNT)
                .map(|index| ids[index % ids.len()])
                .collect::<Vec<_>>();
            let section = SectionPalette::pack_block_states(&states).unwrap();
            assert_eq!(section.copy_palette_block_state_ids(), ids);
            let bits = bits_per_entry_for_palette_size(ids.len()).unwrap();
            let reference = PackedLongArray::pack(SECTION_BLOCK_COUNT, bits, |index| {
                (index % ids.len()) as i32
            })
            .unwrap();
            assert_eq!(section.copy_packed_data(), reference.copy_data());
            for (index, state) in states.into_iter().enumerate() {
                assert_eq!(section.block_state_id_at(index).unwrap(), state);
            }
        }
        assert!(SectionPalette::pack_block_states(&[-1; SECTION_BLOCK_COUNT]).is_err());
        let mut invalid = [STONE; SECTION_BLOCK_COUNT];
        invalid[SECTION_BLOCK_COUNT - 1] = -1;
        assert!(SectionPalette::pack_block_states(&invalid).is_err());
    }

    #[test]
    fn runs_crossing_packed_words_preserve_palette_order_and_full_section_capacity() {
        for distinct in [2, 16, 17, 4096] {
            for run_length in [1, 15, 16, 17, 255] {
                let states = (0..SECTION_BLOCK_COUNT)
                    .map(|index| i32::MAX - ((index / run_length) % distinct) as i32)
                    .collect::<Vec<_>>();
                let count = distinct.min((SECTION_BLOCK_COUNT - 1) / run_length + 1);
                let section = SectionPalette::pack_block_states(&states).unwrap();
                assert_eq!(
                    section.copy_palette_block_state_ids(),
                    (0..count)
                        .map(|index| i32::MAX - index as i32)
                        .collect::<Vec<_>>()
                );
                let reference = PackedLongArray::pack(
                    SECTION_BLOCK_COUNT,
                    bits_per_entry_for_palette_size(count).unwrap(),
                    |index| ((index / run_length) % distinct) as i32,
                )
                .unwrap();
                assert_eq!(section.copy_packed_data(), reference.copy_data());
                for (index, state) in states.into_iter().enumerate() {
                    assert_eq!(section.block_state_id_at(index).unwrap(), state);
                }
            }
        }
    }

    #[test]
    fn validates_section_length_and_block_state_ids() {
        assert!(SectionPalette::pack_block_states(&[AIR]).is_err());

        let mut invalid_states = vec![AIR; SECTION_BLOCK_COUNT];
        invalid_states[0] = -1;
        assert!(SectionPalette::pack_block_states(&invalid_states).is_err());
    }
}
