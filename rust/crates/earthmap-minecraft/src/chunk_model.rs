use crate::block_state_ids;
use crate::dimension_profile::{DimensionProfile, OVERWORLD_1_21_11, SECTION_HEIGHT};
use crate::nbt;
use crate::{MinecraftError, Result};

pub const CHUNK_WIDTH: usize = 16;
pub const SECTION_BLOCK_COUNT: usize = CHUNK_WIDTH * CHUNK_WIDTH * SECTION_HEIGHT as usize;
pub const BIOME_CELL_WIDTH: usize = 4;
pub const SECTION_BIOME_CELL_COUNT: usize = BIOME_CELL_WIDTH * BIOME_CELL_WIDTH * BIOME_CELL_WIDTH;
const DEFAULT_BIOME_ID: &str = "minecraft:plains";

#[derive(Clone, Debug)]
pub(crate) enum BlockSection {
    Uniform(i32),
    Dense(Vec<i32>),
}

impl BlockSection {
    pub(crate) fn block_at(&self, index: usize) -> i32 {
        match self {
            Self::Uniform(block) => *block,
            Self::Dense(blocks) => blocks[index],
        }
    }

    fn dense_mut(&mut self) -> &mut Vec<i32> {
        if let Self::Uniform(block) = self {
            *self = Self::Dense(vec![*block; SECTION_BLOCK_COUNT]);
        }
        let Self::Dense(blocks) = self else {
            unreachable!("uniform section expanded above")
        };
        blocks
    }

    fn copy_blocks(&self) -> Vec<i32> {
        match self {
            Self::Uniform(block) => vec![*block; SECTION_BLOCK_COUNT],
            Self::Dense(blocks) => blocks.clone(),
        }
    }
}

impl PartialEq for BlockSection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Uniform(left), Self::Uniform(right)) => left == right,
            (Self::Dense(left), Self::Dense(right)) => left == right,
            (Self::Uniform(block), Self::Dense(blocks))
            | (Self::Dense(blocks), Self::Uniform(block)) => {
                blocks.iter().all(|value| value == block)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChunkModel {
    dimension: DimensionProfile,
    chunk_x: i32,
    chunk_z: i32,
    sections: Vec<Option<BlockSection>>,
    section_biomes: Vec<Option<Vec<Option<String>>>>,
    block_entities: Vec<nbt::Compound>,
    biome_id: String,
}

impl ChunkModel {
    pub fn new(dimension: DimensionProfile, chunk_x: i32, chunk_z: i32) -> Self {
        let section_count = dimension.section_count() as usize;
        Self {
            dimension,
            chunk_x,
            chunk_z,
            sections: vec![None; section_count],
            section_biomes: vec![None; section_count],
            block_entities: Vec::new(),
            biome_id: DEFAULT_BIOME_ID.to_owned(),
        }
    }

    pub fn overworld(chunk_x: i32, chunk_z: i32) -> Self {
        Self::new(OVERWORLD_1_21_11.clone(), chunk_x, chunk_z)
    }

    pub fn dimension(&self) -> &DimensionProfile {
        &self.dimension
    }

    pub fn chunk_x(&self) -> i32 {
        self.chunk_x
    }

    pub fn chunk_z(&self) -> i32 {
        self.chunk_z
    }

    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    pub fn biome_id(&self) -> &str {
        &self.biome_id
    }

    pub fn set_biome_id(&mut self, biome_id: impl Into<String>) -> Result<()> {
        let biome_id = biome_id.into();
        require_biome_id(&biome_id)?;
        self.biome_id = biome_id;
        Ok(())
    }

    pub fn get_biome_id_at(&self, local_x: i32, y: i32, local_z: i32) -> Result<&str> {
        require_local_xz(local_x, local_z)?;
        let section_index = self.dimension.section_index_for_block_y(y)?;
        let Some(biomes) = &self.section_biomes[section_index] else {
            return Ok(&self.biome_id);
        };
        let biome = biomes[section_biome_index(&self.dimension, local_x, y, local_z)?].as_deref();
        Ok(biome.unwrap_or(&self.biome_id))
    }

    pub fn set_biome_id_at(
        &mut self,
        local_x: i32,
        y: i32,
        local_z: i32,
        biome_id: impl Into<String>,
    ) -> Result<()> {
        require_local_xz(local_x, local_z)?;
        let biome_id = biome_id.into();
        require_biome_id(&biome_id)?;
        let section_index = self.dimension.section_index_for_block_y(y)?;
        let biome_index = section_biome_index(&self.dimension, local_x, y, local_z)?;
        if self.section_biomes[section_index].is_none() && self.biome_id == biome_id {
            return Ok(());
        }
        let biomes = self.section_biomes[section_index]
            .get_or_insert_with(|| vec![None; SECTION_BIOME_CELL_COUNT]);
        biomes[biome_index] = if self.biome_id == biome_id {
            None
        } else {
            Some(biome_id)
        };
        Ok(())
    }

    pub fn allocated_section_count(&self) -> usize {
        self.sections
            .iter()
            .filter(|section| section.is_some())
            .count()
    }

    pub fn is_section_allocated(&self, section_index: i32) -> Result<bool> {
        let section_index = self.section_index(section_index)?;
        Ok(self.sections[section_index].is_some())
    }

    pub fn is_biome_section_allocated(&self, section_index: i32) -> Result<bool> {
        let section_index = self.section_index(section_index)?;
        Ok(self.section_biomes[section_index].is_some())
    }

    pub fn get_block_state_id(&self, local_x: i32, y: i32, local_z: i32) -> Result<i32> {
        require_local_xz(local_x, local_z)?;
        let section_index = self.dimension.section_index_for_block_y(y)?;
        let Some(section) = &self.sections[section_index] else {
            return Ok(block_state_ids::AIR);
        };
        Ok(section.block_at(section_block_index(&self.dimension, local_x, y, local_z)?))
    }

    pub fn set_block_state_id(
        &mut self,
        local_x: i32,
        y: i32,
        local_z: i32,
        block_state_id: i32,
    ) -> Result<()> {
        require_local_xz(local_x, local_z)?;
        block_state_ids::require_valid(block_state_id)?;
        let section_index = self.dimension.section_index_for_block_y(y)?;
        if block_state_id == block_state_ids::AIR && self.sections[section_index].is_none() {
            return Ok(());
        }
        if matches!(self.sections[section_index], Some(BlockSection::Uniform(block)) if block == block_state_id)
        {
            return Ok(());
        }
        let block_index = section_block_index(&self.dimension, local_x, y, local_z)?;
        let section = self.section_or_allocate(section_index);
        section[block_index] = block_state_id;
        Ok(())
    }

    pub fn fill_column(
        &mut self,
        local_x: i32,
        local_z: i32,
        from_y_inclusive: i32,
        to_y_inclusive: i32,
        block_state_id: i32,
    ) -> Result<()> {
        require_local_xz(local_x, local_z)?;
        block_state_ids::require_valid(block_state_id)?;
        if from_y_inclusive > to_y_inclusive {
            return Ok(());
        }
        self.dimension.require_block_y(from_y_inclusive)?;
        self.dimension.require_block_y(to_y_inclusive)?;
        let start_section = self.dimension.section_index_for_block_y(from_y_inclusive)?;
        let end_section = self.dimension.section_index_for_block_y(to_y_inclusive)?;
        let column_base = ((local_z as usize) << 4) | local_x as usize;
        for section_index in start_section..=end_section {
            if matches!(self.sections[section_index], Some(BlockSection::Uniform(block)) if block == block_state_id)
            {
                continue;
            }
            let section_min_y = self.dimension.min_y() + (section_index as i32 * SECTION_HEIGHT);
            let start_y = from_y_inclusive.max(section_min_y);
            let end_y = to_y_inclusive.min(section_min_y + SECTION_HEIGHT - 1);
            let section = self.section_or_allocate(section_index);
            for y in start_y..=end_y {
                let local_y = (y - section_min_y) as usize;
                section[(local_y << 8) | column_base] = block_state_id;
            }
        }
        Ok(())
    }

    /// Fill every X/Z column between the inclusive Y bounds, preserving the
    /// section allocation behavior of calling `fill_column` for each column.
    pub fn fill_layers(
        &mut self,
        from_y_inclusive: i32,
        to_y_inclusive: i32,
        block_state_id: i32,
    ) -> Result<()> {
        block_state_ids::require_valid(block_state_id)?;
        if from_y_inclusive > to_y_inclusive {
            return Ok(());
        }
        let start_section = self.dimension.section_index_for_block_y(from_y_inclusive)?;
        let end_section = self.dimension.section_index_for_block_y(to_y_inclusive)?;
        for section_index in start_section..=end_section {
            let section_min_y = self.dimension.min_y() + section_index as i32 * SECTION_HEIGHT;
            let start_y = from_y_inclusive.max(section_min_y) - section_min_y;
            let end_y = to_y_inclusive.min(section_min_y + SECTION_HEIGHT - 1) - section_min_y;
            if start_y == 0 && end_y == SECTION_HEIGHT - 1 {
                self.sections[section_index] = Some(BlockSection::Uniform(block_state_id));
                continue;
            }
            if matches!(self.sections[section_index], Some(BlockSection::Uniform(block)) if block == block_state_id)
            {
                continue;
            }
            let section = self.section_or_allocate(section_index);
            section[start_y as usize * CHUNK_WIDTH * CHUNK_WIDTH
                ..(end_y as usize + 1) * CHUNK_WIDTH * CHUNK_WIDTH]
                .fill(block_state_id);
        }
        Ok(())
    }

    pub fn copy_section_block_state_ids(&self, section_index: i32) -> Result<Vec<i32>> {
        let section_index = self.section_index(section_index)?;
        Ok(self.sections[section_index]
            .as_ref()
            .map(BlockSection::copy_blocks)
            .unwrap_or_else(|| vec![block_state_ids::AIR; SECTION_BLOCK_COUNT]))
    }

    pub(crate) fn section_blocks(&self, section_index: i32) -> Result<Option<&BlockSection>> {
        let section_index = self.section_index(section_index)?;
        Ok(self.sections[section_index].as_ref())
    }

    pub(crate) fn section_biome_ids(
        &self,
        section_index: i32,
    ) -> Result<impl ExactSizeIterator<Item = &str>> {
        let section_index = self.section_index(section_index)?;
        let biomes = self.section_biomes[section_index].as_deref();
        Ok((0..SECTION_BIOME_CELL_COUNT).map(move |index| {
            biomes
                .and_then(|biomes| biomes[index].as_deref())
                .unwrap_or(&self.biome_id)
        }))
    }

    pub fn copy_section_biome_ids(&self, section_index: i32) -> Result<Vec<String>> {
        let section_index = self.section_index(section_index)?;
        let mut result = vec![self.biome_id.clone(); SECTION_BIOME_CELL_COUNT];
        if let Some(biomes) = &self.section_biomes[section_index] {
            for (index, biome) in biomes.iter().enumerate() {
                if let Some(biome) = biome {
                    result[index] = biome.clone();
                }
            }
        }
        Ok(result)
    }

    pub fn section_y_for_index(&self, section_index: i32) -> Result<i32> {
        let section_index = self.section_index(section_index)?;
        self.dimension.section_y_for_index(section_index)
    }

    pub fn add_block_entity(&mut self, block_entity: nbt::Compound) {
        self.block_entities.push(block_entity);
    }

    pub fn block_entities(&self) -> &[nbt::Compound] {
        &self.block_entities
    }

    fn section_or_allocate(&mut self, section_index: usize) -> &mut Vec<i32> {
        self.sections[section_index]
            .get_or_insert(BlockSection::Uniform(block_state_ids::AIR))
            .dense_mut()
    }

    fn section_index(&self, section_index: i32) -> Result<usize> {
        if section_index < 0 {
            return Err(MinecraftError::invalid(format!(
                "sectionIndex {section_index} outside 0..{}",
                self.section_count() - 1
            )));
        }
        let section_index = section_index as usize;
        self.dimension.require_section_index(section_index)?;
        Ok(section_index)
    }
}

fn section_block_index(
    dimension: &DimensionProfile,
    local_x: i32,
    y: i32,
    local_z: i32,
) -> Result<usize> {
    require_local_xz(local_x, local_z)?;
    let local_y = ((y - dimension.min_y()) & (SECTION_HEIGHT - 1)) as usize;
    Ok((local_y << 8) | ((local_z as usize) << 4) | local_x as usize)
}

fn section_biome_index(
    dimension: &DimensionProfile,
    local_x: i32,
    y: i32,
    local_z: i32,
) -> Result<usize> {
    require_local_xz(local_x, local_z)?;
    let local_cell_x = (local_x as usize) >> 2;
    let local_cell_y = (((y - dimension.min_y()) & (SECTION_HEIGHT - 1)) as usize) >> 2;
    let local_cell_z = (local_z as usize) >> 2;
    Ok((local_cell_y << 4) | (local_cell_z << 2) | local_cell_x)
}

fn require_local_xz(local_x: i32, local_z: i32) -> Result<()> {
    if local_x < 0 || local_x >= CHUNK_WIDTH as i32 {
        return Err(MinecraftError::invalid(format!(
            "localX outside 0..15: {local_x}"
        )));
    }
    if local_z < 0 || local_z >= CHUNK_WIDTH as i32 {
        return Err(MinecraftError::invalid(format!(
            "localZ outside 0..15: {local_z}"
        )));
    }
    Ok(())
}

fn require_biome_id(biome_id: &str) -> Result<()> {
    if biome_id.trim().is_empty() {
        return Err(MinecraftError::invalid("biomeId must not be blank"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_state_ids::{AIR, STONE, WATER};
    use crate::nbt::{compound, Tag};

    #[test]
    fn default_air_and_lazy_sections_match_java() {
        let mut chunk = ChunkModel::overworld(12, -7);

        assert_eq!(chunk.chunk_x(), 12);
        assert_eq!(chunk.chunk_z(), -7);
        assert_eq!(chunk.section_count(), 24);
        assert_eq!(chunk.allocated_section_count(), 0);
        assert_eq!(chunk.get_block_state_id(0, -64, 0).unwrap(), AIR);
        assert_eq!(chunk.get_block_state_id(15, 319, 15).unwrap(), AIR);

        chunk.set_block_state_id(1, 0, 1, AIR).unwrap();
        assert_eq!(chunk.allocated_section_count(), 0);
    }

    #[test]
    fn set_and_get_at_build_height_edges_match_java() {
        let mut chunk = ChunkModel::overworld(0, 0);

        chunk.set_block_state_id(0, -64, 0, STONE).unwrap();
        chunk.set_block_state_id(15, 319, 15, WATER).unwrap();

        assert_eq!(chunk.get_block_state_id(0, -64, 0).unwrap(), STONE);
        assert_eq!(chunk.get_block_state_id(15, 319, 15).unwrap(), WATER);
        assert_eq!(chunk.allocated_section_count(), 2);
        assert!(chunk.is_section_allocated(0).unwrap());
        assert!(chunk.is_section_allocated(23).unwrap());
        assert_eq!(chunk.section_y_for_index(0).unwrap(), -4);
        assert_eq!(chunk.section_y_for_index(23).unwrap(), 19);
    }

    #[test]
    fn bounds_checks_match_java_chunk_model_test() {
        let mut chunk = ChunkModel::overworld(0, 0);

        assert!(chunk.get_block_state_id(-1, 0, 0).is_err());
        assert!(chunk.get_block_state_id(16, 0, 0).is_err());
        assert!(chunk.get_block_state_id(0, 0, -1).is_err());
        assert!(chunk.get_block_state_id(0, 0, 16).is_err());
        assert!(chunk.get_block_state_id(0, -65, 0).is_err());
        assert!(chunk.get_block_state_id(0, 320, 0).is_err());
        assert!(chunk.set_block_state_id(0, 0, 0, -1).is_err());
        assert!(chunk.copy_section_block_state_ids(-1).is_err());
        assert!(chunk.copy_section_block_state_ids(24).is_err());
    }

    #[test]
    fn uniform_sections_expand_only_when_a_block_changes() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.fill_layers(-64, -49, STONE).unwrap();
        assert!(matches!(
            chunk.section_blocks(0).unwrap(),
            Some(BlockSection::Uniform(STONE))
        ));
        chunk.set_block_state_id(3, -60, 4, STONE).unwrap();
        chunk.fill_column(0, 0, -64, -49, STONE).unwrap();
        assert!(matches!(
            chunk.section_blocks(0).unwrap(),
            Some(BlockSection::Uniform(STONE))
        ));
        let mut copy = chunk.clone();
        copy.set_block_state_id(3, -60, 4, WATER).unwrap();
        assert!(matches!(
            copy.section_blocks(0).unwrap(),
            Some(BlockSection::Dense(_))
        ));
        assert_eq!(chunk.get_block_state_id(3, -60, 4).unwrap(), STONE);
        assert_eq!(copy.get_block_state_id(3, -60, 4).unwrap(), WATER);
        assert_eq!(copy.get_block_state_id(4, -60, 4).unwrap(), STONE);
        copy.fill_layers(-64, -49, AIR).unwrap();
        assert!(copy.is_section_allocated(0).unwrap());
        assert_eq!(
            copy.copy_section_block_state_ids(0).unwrap(),
            vec![AIR; SECTION_BLOCK_COUNT]
        );
    }

    #[test]
    fn fill_column_matches_java_inclusive_edges_and_noop() {
        let mut chunk = ChunkModel::overworld(0, 0);

        chunk.fill_column(3, 4, -64, -60, STONE).unwrap();
        for y in -64..=-60 {
            assert_eq!(chunk.get_block_state_id(3, y, 4).unwrap(), STONE);
        }
        assert_eq!(chunk.get_block_state_id(3, -59, 4).unwrap(), AIR);
        assert_eq!(chunk.allocated_section_count(), 1);

        chunk.fill_column(3, 4, 10, 9, WATER).unwrap();
        assert_eq!(chunk.allocated_section_count(), 1);
    }

    #[test]
    fn filling_layers_matches_individual_columns_with_partial_sections_and_air() {
        for dimension in [
            OVERWORLD_1_21_11.clone(),
            DimensionProfile::new("test:short", -32, 64).unwrap(),
        ] {
            let mut layers = ChunkModel::new(dimension.clone(), 1, -2);
            let mut columns = layers.clone();
            for (from, to, block) in [
                (dimension.min_y(), dimension.max_y_inclusive(), STONE),
                (dimension.min_y() + 3, dimension.min_y() + 19, WATER),
                (
                    dimension.max_y_inclusive() - 7,
                    dimension.max_y_inclusive(),
                    AIR,
                ),
                (10, 9, STONE),
            ] {
                layers.fill_layers(from, to, block).unwrap();
                for z in 0..CHUNK_WIDTH as i32 {
                    for x in 0..CHUNK_WIDTH as i32 {
                        columns.fill_column(x, z, from, to, block).unwrap();
                    }
                }
                assert_eq!(layers, columns);
            }
            assert!(layers.fill_layers(dimension.min_y() - 1, 0, STONE).is_err());
            assert!(layers
                .fill_layers(0, dimension.max_y_inclusive() + 1, STONE)
                .is_err());
            assert!(layers.fill_layers(10, 9, -1).is_err());
            let mut air = ChunkModel::new(dimension, 1, -2);
            air.fill_layers(0, 0, AIR).unwrap();
            assert_eq!(air.allocated_section_count(), 1);
        }
    }

    #[test]
    fn section_copy_is_defensive_like_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        let mut empty_copy = chunk.copy_section_block_state_ids(0).unwrap();

        assert_eq!(empty_copy.len(), SECTION_BLOCK_COUNT);
        empty_copy[0] = STONE;
        assert_eq!(chunk.get_block_state_id(0, -64, 0).unwrap(), AIR);

        chunk.set_block_state_id(0, -64, 0, WATER).unwrap();
        let mut copy = chunk.copy_section_block_state_ids(0).unwrap();
        assert_eq!(copy[0], WATER);
        copy[0] = STONE;
        assert_eq!(chunk.get_block_state_id(0, -64, 0).unwrap(), WATER);
    }

    #[test]
    fn biome_defaults_and_lazy_section_allocation_match_java() {
        let mut chunk = ChunkModel::overworld(0, 0);

        assert_eq!(chunk.biome_id(), DEFAULT_BIOME_ID);
        assert_eq!(chunk.get_biome_id_at(0, -64, 0).unwrap(), DEFAULT_BIOME_ID);
        chunk.set_biome_id_at(0, -64, 0, DEFAULT_BIOME_ID).unwrap();
        assert!(!chunk.is_biome_section_allocated(0).unwrap());

        chunk
            .set_biome_id_at(4, -60, 4, "minecraft:desert")
            .unwrap();
        assert!(chunk.is_biome_section_allocated(0).unwrap());
        assert_eq!(
            chunk.get_biome_id_at(4, -60, 4).unwrap(),
            "minecraft:desert"
        );
        assert_eq!(chunk.get_biome_id_at(0, -64, 0).unwrap(), DEFAULT_BIOME_ID);

        let copy = chunk.copy_section_biome_ids(0).unwrap();
        assert_eq!(copy.len(), SECTION_BIOME_CELL_COUNT);
        assert_eq!(copy[21], "minecraft:desert");
        assert!(chunk.set_biome_id("").is_err());
    }

    #[test]
    fn biome_copy_indices_match_java_nbt_palette_order_inputs() {
        let mut chunk = ChunkModel::overworld(1, 2);
        chunk.set_biome_id(DEFAULT_BIOME_ID).unwrap();
        chunk
            .set_biome_id_at(0, -64, 0, "minecraft:desert")
            .unwrap();
        chunk
            .set_biome_id_at(4, -64, 0, "minecraft:jungle")
            .unwrap();
        chunk.set_biome_id_at(8, -64, 0, DEFAULT_BIOME_ID).unwrap();
        chunk
            .set_biome_id_at(0, -60, 0, "minecraft:snowy_plains")
            .unwrap();

        assert_eq!(
            chunk.get_biome_id_at(0, -64, 0).unwrap(),
            "minecraft:desert"
        );
        assert_eq!(
            chunk.get_biome_id_at(4, -64, 0).unwrap(),
            "minecraft:jungle"
        );
        assert_eq!(chunk.get_biome_id_at(8, -64, 0).unwrap(), DEFAULT_BIOME_ID);
        assert_eq!(
            chunk.get_biome_id_at(0, -60, 0).unwrap(),
            "minecraft:snowy_plains"
        );

        let copy = chunk.copy_section_biome_ids(0).unwrap();
        assert_eq!(copy[0], "minecraft:desert");
        assert_eq!(copy[1], "minecraft:jungle");
        assert_eq!(copy[2], DEFAULT_BIOME_ID);
        assert_eq!(copy[16], "minecraft:snowy_plains");
        assert_eq!(copy[3], DEFAULT_BIOME_ID);
    }

    #[test]
    fn biome_only_section_state_matches_java_encoder_inputs() {
        let mut chunk = ChunkModel::overworld(3, 4);
        chunk.set_biome_id(DEFAULT_BIOME_ID).unwrap();
        chunk
            .set_biome_id_at(0, 80, 0, "minecraft:snowy_plains")
            .unwrap();

        assert!(chunk.is_biome_section_allocated(9).unwrap());
        assert!(!chunk.is_section_allocated(9).unwrap());
        assert_eq!(chunk.section_y_for_index(9).unwrap(), 5);
        assert!(chunk
            .copy_section_block_state_ids(9)
            .unwrap()
            .into_iter()
            .all(|state| state == AIR));
        let copy = chunk.copy_section_biome_ids(9).unwrap();
        assert_eq!(copy[0], "minecraft:snowy_plains");
        assert_eq!(copy[1], DEFAULT_BIOME_ID);
    }

    #[test]
    fn block_entities_preserve_java_chunk_model_order() {
        let mut chunk = ChunkModel::overworld(0, 0);
        let mut chest = compound();
        chest
            .put_string("id", "minecraft:chest")
            .unwrap()
            .put_int("x", 1)
            .unwrap();
        let mut spawner = compound();
        spawner
            .put_string("id", "minecraft:mob_spawner")
            .unwrap()
            .put_int("x", 2)
            .unwrap();

        chunk.add_block_entity(chest);
        chunk.add_block_entity(spawner);

        assert_eq!(chunk.block_entities().len(), 2);
        assert_eq!(
            chunk.block_entities()[0].get_string("id").unwrap(),
            "minecraft:chest"
        );
        assert_eq!(
            chunk.block_entities()[1].get_string("id").unwrap(),
            "minecraft:mob_spawner"
        );
        assert_eq!(chunk.block_entities()[1].get_int("x").unwrap(), 2);
        assert!(matches!(
            chunk.block_entities()[0].get("id").unwrap(),
            Tag::String(_)
        ));
    }
}
