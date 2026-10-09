use crate::block_state_ids as ids;
use crate::chunk_generation_status::ChunkGenerationStatus;
use crate::chunk_model::{ChunkModel, SECTION_BIOME_CELL_COUNT, SECTION_BLOCK_COUNT};
use crate::dimension_profile::SECTION_HEIGHT;
use crate::heightmap::{Heightmap, COLUMN_COUNT};
use crate::nbt::{self, Compound, Tag};
use crate::packed_long_array::PackedLongArray;
use crate::section_palette::SectionPalette;
use crate::{MinecraftError, Result};

pub const DATA_VERSION: i32 = 4671;

pub fn encode_to_bytes(chunk: &ChunkModel, last_update: i64) -> Result<Vec<u8>> {
    encode_to_bytes_with_status(chunk, last_update, ChunkGenerationStatus::Full)
}

pub fn encode_to_bytes_with_status(
    chunk: &ChunkModel,
    last_update: i64,
    status: ChunkGenerationStatus,
) -> Result<Vec<u8>> {
    nbt::write_to_bytes("", &encode(chunk, last_update, status)?)
}

pub fn encode(
    chunk: &ChunkModel,
    last_update: i64,
    status: ChunkGenerationStatus,
) -> Result<Compound> {
    let [world_surface, ocean_floor, motion_blocking_no_leaves] = compute_heightmaps(chunk)?;

    let mut root = nbt::compound();
    root.put_int("DataVersion", DATA_VERSION)?
        .put_int("xPos", chunk.chunk_x())?
        .put_int("yPos", chunk.dimension().min_section_y())?
        .put_int("zPos", chunk.chunk_z())?
        .put_long("LastUpdate", last_update)?
        .put_long("InhabitedTime", 0)?
        .put_string("Status", status.id())?
        .put_byte("isLightOn", i32::from(status.light_on()))?;
    root.put(
        "sections",
        Tag::List(nbt::list(nbt::TAG_COMPOUND, section_tags(chunk)?)?),
    )?;
    root.put_compound(
        "Heightmaps",
        heightmaps(&world_surface, &ocean_floor, &motion_blocking_no_leaves)?,
    )?;
    root.put(
        "block_entities",
        Tag::List(nbt::list(nbt::TAG_COMPOUND, block_entity_tags(chunk))?),
    )?;
    root.put(
        "block_ticks",
        Tag::List(nbt::list(nbt::TAG_COMPOUND, Vec::new())?),
    )?;
    root.put(
        "fluid_ticks",
        Tag::List(nbt::list(nbt::TAG_COMPOUND, Vec::new())?),
    )?;
    root.put_compound("structures", empty_structures()?)?;
    Ok(root)
}

fn compute_heightmaps(chunk: &ChunkModel) -> Result<[Heightmap; 3]> {
    let dimension = chunk.dimension();
    let sections = (0..chunk.section_count() as i32)
        .map(|index| chunk.section_block_state_ids(index))
        .collect::<Result<Vec<_>>>()?;
    let mut values = [(); 3].map(|_| vec![dimension.min_y(); COLUMN_COUNT]);
    for column in 0..COLUMN_COUNT {
        let mut pending = [true; 3];
        'sections: for (section_index, section) in sections.iter().enumerate().rev() {
            // All three predicates exclude AIR; an unallocated section is AIR.
            let Some(section) = section else { continue };
            for local_y in (0..SECTION_HEIGHT as usize).rev() {
                let block = section[local_y * COLUMN_COUNT + column];
                let counts = [
                    counts_for_surface(block),
                    counts_for_ocean_floor(block),
                    counts_for_motion_blocking_no_leaves(block),
                ];
                for map in 0..3 {
                    if pending[map] && counts[map] {
                        values[map][column] = dimension.min_y()
                            + section_index as i32 * SECTION_HEIGHT
                            + local_y as i32
                            + 1;
                        pending[map] = false;
                    }
                }
                if pending == [false; 3] {
                    break 'sections;
                }
            }
        }
    }
    let [surface, floor, no_leaves] = values;
    Ok([
        Heightmap::new(dimension.clone(), surface)?,
        Heightmap::new(dimension.clone(), floor)?,
        Heightmap::new(dimension.clone(), no_leaves)?,
    ])
}

fn section_tags(chunk: &ChunkModel) -> Result<Vec<Tag>> {
    let mut sections = Vec::with_capacity(chunk.section_count());
    for section_index in 0..chunk.section_count() as i32 {
        if !chunk.is_section_allocated(section_index)?
            && !chunk.is_biome_section_allocated(section_index)?
        {
            continue;
        }
        let palette = SectionPalette::pack_block_states(
            chunk
                .section_block_state_ids(section_index)?
                .unwrap_or(&[ids::AIR; SECTION_BLOCK_COUNT]),
        )?;
        let mut section = nbt::compound();
        section
            .put_byte("Y", chunk.section_y_for_index(section_index)?)?
            .put_compound("block_states", block_states_tag(&palette)?)?
            .put_compound(
                "biomes",
                biome_tag(chunk.section_biome_ids(section_index)?)?,
            )?;
        sections.push(Tag::Compound(section));
    }
    Ok(sections)
}

fn block_states_tag(palette: &SectionPalette) -> Result<Compound> {
    let mut block_states = nbt::compound();
    block_states.put(
        "palette",
        Tag::List(nbt::list(
            nbt::TAG_COMPOUND,
            block_state_palette_tags(palette)?,
        )?),
    )?;
    if palette.bits_per_entry() > 0 {
        block_states.put_long_array("data", palette.copy_packed_data())?;
    }
    Ok(block_states)
}

fn block_state_palette_tags(palette: &SectionPalette) -> Result<Vec<Tag>> {
    let mut tags = Vec::with_capacity(palette.palette_size());
    for id in palette.copy_palette_block_state_ids() {
        tags.push(Tag::Compound(block_state_tag(id)?));
    }
    Ok(tags)
}

fn block_state_tag(block_state_id: i32) -> Result<Compound> {
    match block_state_id {
        ids::AIR => named_block("minecraft:air"),
        ids::STONE => named_block("minecraft:stone"),
        ids::WATER => block_with_properties("minecraft:water", &[("level", "0")]),
        ids::LAVA => block_with_properties("minecraft:lava", &[("level", "0")]),
        ids::DIRT => named_block("minecraft:dirt"),
        ids::GRASS_BLOCK => named_block("minecraft:grass_block"),
        ids::BEDROCK => named_block("minecraft:bedrock"),
        ids::SAND => named_block("minecraft:sand"),
        ids::SNOW_BLOCK => named_block("minecraft:snow_block"),
        ids::ICE => named_block("minecraft:ice"),
        ids::DEEPSLATE => named_block("minecraft:deepslate"),
        ids::COAL_ORE => named_block("minecraft:coal_ore"),
        ids::IRON_ORE => named_block("minecraft:iron_ore"),
        ids::COPPER_ORE => named_block("minecraft:copper_ore"),
        ids::GOLD_ORE => named_block("minecraft:gold_ore"),
        ids::REDSTONE_ORE => named_block("minecraft:redstone_ore"),
        ids::LAPIS_ORE => named_block("minecraft:lapis_ore"),
        ids::DIAMOND_ORE => named_block("minecraft:diamond_ore"),
        ids::EMERALD_ORE => named_block("minecraft:emerald_ore"),
        ids::DEEPSLATE_COAL_ORE => named_block("minecraft:deepslate_coal_ore"),
        ids::DEEPSLATE_IRON_ORE => named_block("minecraft:deepslate_iron_ore"),
        ids::DEEPSLATE_COPPER_ORE => named_block("minecraft:deepslate_copper_ore"),
        ids::DEEPSLATE_GOLD_ORE => named_block("minecraft:deepslate_gold_ore"),
        ids::DEEPSLATE_REDSTONE_ORE => named_block("minecraft:deepslate_redstone_ore"),
        ids::DEEPSLATE_LAPIS_ORE => named_block("minecraft:deepslate_lapis_ore"),
        ids::DEEPSLATE_DIAMOND_ORE => named_block("minecraft:deepslate_diamond_ore"),
        ids::DEEPSLATE_EMERALD_ORE => named_block("minecraft:deepslate_emerald_ore"),
        ids::CHEST => block_with_properties(
            "minecraft:chest",
            &[
                ("facing", "north"),
                ("type", "single"),
                ("waterlogged", "false"),
            ],
        ),
        ids::SPAWNER => named_block("minecraft:spawner"),
        ids::END_PORTAL_FRAME => block_with_properties(
            "minecraft:end_portal_frame",
            &[("eye", "false"), ("facing", "north")],
        ),
        ids::END_PORTAL_FRAME_FILLED => block_with_properties(
            "minecraft:end_portal_frame",
            &[("eye", "true"), ("facing", "north")],
        ),
        ids::END_PORTAL => named_block("minecraft:end_portal"),
        ids::STONE_BRICKS => named_block("minecraft:stone_bricks"),
        ids::OAK_LOG => block_with_properties("minecraft:oak_log", &[("axis", "y")]),
        ids::OAK_LEAVES => block_with_properties(
            "minecraft:oak_leaves",
            &[
                ("distance", "7"),
                ("persistent", "true"),
                ("waterlogged", "false"),
            ],
        ),
        ids::JUNGLE_LOG => block_with_properties("minecraft:jungle_log", &[("axis", "y")]),
        ids::JUNGLE_LEAVES => block_with_properties(
            "minecraft:jungle_leaves",
            &[
                ("distance", "7"),
                ("persistent", "true"),
                ("waterlogged", "false"),
            ],
        ),
        ids::DARK_OAK_LEAVES => block_with_properties(
            "minecraft:dark_oak_leaves",
            &[
                ("distance", "7"),
                ("persistent", "true"),
                ("waterlogged", "false"),
            ],
        ),
        ids::SPRUCE_LEAVES => block_with_properties(
            "minecraft:spruce_leaves",
            &[
                ("distance", "7"),
                ("persistent", "true"),
                ("waterlogged", "false"),
            ],
        ),
        ids::GRAVEL => named_block("minecraft:gravel"),
        ids::CLAY => named_block("minecraft:clay"),
        ids::RED_SAND => named_block("minecraft:red_sand"),
        ids::COARSE_DIRT => named_block("minecraft:coarse_dirt"),
        ids::TERRACOTTA => named_block("minecraft:terracotta"),
        ids::ORANGE_TERRACOTTA => named_block("minecraft:orange_terracotta"),
        ids::BROWN_TERRACOTTA => named_block("minecraft:brown_terracotta"),
        ids::MUD => named_block("minecraft:mud"),
        ids::MOSS_BLOCK => named_block("minecraft:moss_block"),
        ids::PODZOL => block_with_properties("minecraft:podzol", &[("snowy", "false")]),
        ids::WHITE_TERRACOTTA => named_block("minecraft:white_terracotta"),
        ids::LIGHT_GRAY_TERRACOTTA => named_block("minecraft:light_gray_terracotta"),
        ids::GRAY_TERRACOTTA => named_block("minecraft:gray_terracotta"),
        ids::BLACK_TERRACOTTA => named_block("minecraft:black_terracotta"),
        ids::YELLOW_TERRACOTTA => named_block("minecraft:yellow_terracotta"),
        ids::RED_TERRACOTTA => named_block("minecraft:red_terracotta"),
        ids::GREEN_TERRACOTTA => named_block("minecraft:green_terracotta"),
        ids::CYAN_TERRACOTTA => named_block("minecraft:cyan_terracotta"),
        ids::LIME_TERRACOTTA => named_block("minecraft:lime_terracotta"),
        ids::PACKED_MUD => named_block("minecraft:packed_mud"),
        ids::CALCITE => named_block("minecraft:calcite"),
        ids::TUFF => named_block("minecraft:tuff"),
        ids::SANDSTONE => named_block("minecraft:sandstone"),
        ids::ROOTED_DIRT => named_block("minecraft:rooted_dirt"),
        ids::MYCELIUM => block_with_properties("minecraft:mycelium", &[("snowy", "false")]),
        ids::ANDESITE => named_block("minecraft:andesite"),
        ids::GRANITE => named_block("minecraft:granite"),
        ids::DIORITE => named_block("minecraft:diorite"),
        ids::BLACK_CONCRETE => named_block("minecraft:black_concrete"),
        ids::QUARTZ_BLOCK => named_block("minecraft:quartz_block"),
        ids::BONE_BLOCK => block_with_properties("minecraft:bone_block", &[("axis", "y")]),
        ids::END_STONE => named_block("minecraft:end_stone"),
        ids::END_STONE_BRICKS => named_block("minecraft:end_stone_bricks"),
        ids::SMOOTH_SANDSTONE => named_block("minecraft:smooth_sandstone"),
        ids::CUT_SANDSTONE => named_block("minecraft:cut_sandstone"),
        ids::CHISELED_SANDSTONE => named_block("minecraft:chiseled_sandstone"),
        ids::SMOOTH_RED_SANDSTONE => named_block("minecraft:smooth_red_sandstone"),
        ids::CUT_RED_SANDSTONE => named_block("minecraft:cut_red_sandstone"),
        ids::CHISELED_RED_SANDSTONE => named_block("minecraft:chiseled_red_sandstone"),
        ids::MUD_BRICKS => named_block("minecraft:mud_bricks"),
        ids::DRIPSTONE_BLOCK => named_block("minecraft:dripstone_block"),
        _ => Err(MinecraftError::invalid(format!(
            "unmapped block state id: {block_state_id}"
        ))),
    }
}

fn block_entity_tags(chunk: &ChunkModel) -> Vec<Tag> {
    chunk
        .block_entities()
        .iter()
        .cloned()
        .map(Tag::Compound)
        .collect()
}

fn named_block(name: &str) -> Result<Compound> {
    let mut block = nbt::compound();
    block.put_string("Name", name)?;
    Ok(block)
}

fn block_with_properties(name: &str, properties: &[(&str, &str)]) -> Result<Compound> {
    let mut block = named_block(name)?;
    let mut props = nbt::compound();
    for (key, value) in properties {
        props.put_string(*key, *value)?;
    }
    block.put_compound("Properties", props)?;
    Ok(block)
}

fn biome_tag<'a>(biome_ids: impl ExactSizeIterator<Item = &'a str>) -> Result<Compound> {
    let biome_count = biome_ids.len();
    if biome_count != SECTION_BIOME_CELL_COUNT {
        return Err(MinecraftError::invalid(format!(
            "section biomeIds length must be {SECTION_BIOME_CELL_COUNT}"
        )));
    }
    let mut palette = Vec::<&str>::new();
    let mut indices = vec![0i32; biome_count];
    for (index, biome_id) in biome_ids.enumerate() {
        if biome_id.trim().is_empty() {
            return Err(MinecraftError::invalid(format!(
                "biomeId must not be blank at index {index}"
            )));
        }
        let palette_index = match palette.iter().position(|candidate| *candidate == biome_id) {
            Some(existing) => existing,
            None => {
                palette.push(biome_id);
                palette.len() - 1
            }
        };
        indices[index] = palette_index as i32;
    }

    let mut tag = nbt::compound();
    tag.put(
        "palette",
        Tag::List(nbt::list(
            nbt::TAG_STRING,
            palette.iter().copied().map(nbt::string_tag).collect(),
        )?),
    )?;
    let bits_per_entry = biome_bits_per_entry(palette.len())?;
    if bits_per_entry > 0 {
        let packed = PackedLongArray::pack(biome_count, bits_per_entry, |index| indices[index])?;
        tag.put_long_array("data", packed.copy_data())?;
    }
    Ok(tag)
}

fn biome_bits_per_entry(palette_size: usize) -> Result<u8> {
    if palette_size == 0 {
        return Err(MinecraftError::invalid("paletteSize must be positive: 0"));
    }
    if palette_size == 1 {
        return Ok(0);
    }
    Ok((usize::BITS - (palette_size - 1).leading_zeros()) as u8)
}

fn heightmaps(
    world_surface: &Heightmap,
    ocean_floor: &Heightmap,
    motion_blocking_no_leaves: &Heightmap,
) -> Result<Compound> {
    let world_surface_packed = world_surface.pack_storage_values()?.copy_data();
    let ocean_floor_packed = ocean_floor.pack_storage_values()?.copy_data();
    let motion_blocking_no_leaves_packed =
        motion_blocking_no_leaves.pack_storage_values()?.copy_data();
    let mut tag = nbt::compound();
    tag.put_long_array("WORLD_SURFACE", world_surface_packed.clone())?
        .put_long_array("OCEAN_FLOOR", ocean_floor_packed)?
        .put_long_array("MOTION_BLOCKING", world_surface_packed)?
        .put_long_array(
            "MOTION_BLOCKING_NO_LEAVES",
            motion_blocking_no_leaves_packed,
        )?;
    Ok(tag)
}

fn empty_structures() -> Result<Compound> {
    let mut structures = nbt::compound();
    structures
        .put_compound("starts", nbt::compound())?
        .put_compound("References", nbt::compound())?;
    Ok(structures)
}

fn counts_for_surface(block_state_id: i32) -> bool {
    block_state_id != ids::AIR
}

fn counts_for_ocean_floor(block_state_id: i32) -> bool {
    block_state_id != ids::AIR && block_state_id != ids::WATER && block_state_id != ids::LAVA
}

fn counts_for_motion_blocking_no_leaves(block_state_id: i32) -> bool {
    counts_for_surface(block_state_id)
        && block_state_id != ids::OAK_LEAVES
        && block_state_id != ids::JUNGLE_LEAVES
        && block_state_id != ids::DARK_OAK_LEAVES
        && block_state_id != ids::SPRUCE_LEAVES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_heightmaps_match_independent_scans_in_sparse_and_mixed_chunks() {
        use crate::dimension_profile::{DimensionProfile, OVERWORLD_1_21_11};
        use crate::heightmap_calculator::compute_top_y_exclusive;

        for dimension in [
            OVERWORLD_1_21_11.clone(),
            DimensionProfile::new("test:short", -32, 64).unwrap(),
        ] {
            let mut chunk = ChunkModel::new(dimension.clone(), -7, 9);
            let compare = |chunk: &ChunkModel| {
                let actual = compute_heightmaps(chunk).unwrap();
                for (map, predicate) in actual.iter().zip([
                    counts_for_surface as fn(i32) -> bool,
                    counts_for_ocean_floor,
                    counts_for_motion_blocking_no_leaves,
                ]) {
                    assert_eq!(*map, compute_top_y_exclusive(chunk, predicate).unwrap());
                }
            };
            compare(&chunk);
            let blocks = [
                ids::AIR,
                ids::STONE,
                ids::WATER,
                ids::LAVA,
                ids::OAK_LEAVES,
                ids::JUNGLE_LEAVES,
                ids::DARK_OAK_LEAVES,
                ids::SPRUCE_LEAVES,
            ];
            for column in 0..COLUMN_COUNT {
                let x = (column % 16) as i32;
                let z = (column / 16) as i32;
                // Leave holes, put fluids and leaves above solids, and cover the
                // build limits as well as both sides of section boundaries.
                for offset in [0, 15, 16, 31, dimension.height() - 1] {
                    chunk
                        .set_block_state_id(
                            x,
                            dimension.min_y() + offset,
                            z,
                            blocks[(column + offset as usize) % blocks.len()],
                        )
                        .unwrap();
                }
            }
            compare(&chunk);
            chunk
                .set_biome_id_at(4, dimension.max_y_inclusive(), 8, "minecraft:forest")
                .unwrap();
            compare(&chunk);
        }
    }

    #[test]
    fn borrowed_biome_palette_matches_copy_with_default_cells_and_biome_only_sections() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:desert").unwrap();
        chunk
            .set_biome_id_at(0, 319, 0, "minecraft:forest")
            .unwrap();
        chunk.set_biome_id_at(4, 319, 0, "minecraft:swamp").unwrap();
        for section in 0..chunk.section_count() as i32 {
            let copied = chunk.copy_section_biome_ids(section).unwrap();
            let reference = biome_tag(copied.iter().map(String::as_str)).unwrap();
            let borrowed = biome_tag(chunk.section_biome_ids(section).unwrap()).unwrap();
            assert_eq!(reference, borrowed);
        }
        let encoded = encode(&chunk, 0, ChunkGenerationStatus::Surface).unwrap();
        let sections = encoded.get_list("sections").unwrap();
        assert_eq!(sections.values().len(), 1);
        let Tag::Compound(section) = &sections.values()[0] else {
            panic!("section compound")
        };
        assert_eq!(
            section
                .get_compound("block_states")
                .unwrap()
                .get_list("palette")
                .unwrap()
                .values()
                .len(),
            1
        );
    }

    #[test]
    fn default_and_delegated_status_match_java() {
        let chunk = ChunkModel::overworld(0, 0);
        let full = encode(&chunk, 0, ChunkGenerationStatus::Full).unwrap();
        assert_eq!(full.get_string("Status").unwrap(), "minecraft:full");
        assert_eq!(full.get_byte("isLightOn").unwrap(), 1);

        let delegated = encode(&chunk, 0, ChunkGenerationStatus::Surface).unwrap();
        assert_eq!(delegated.get_string("Status").unwrap(), "minecraft:surface");
        assert_eq!(delegated.get_byte("isLightOn").unwrap(), 0);
    }

    #[test]
    fn ocean_floor_heightmap_excludes_water_like_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.fill_column(0, 0, -64, 60, ids::STONE).unwrap();
        chunk.fill_column(0, 0, 61, 63, ids::WATER).unwrap();

        let encoded = encode(&chunk, 0, ChunkGenerationStatus::Surface).unwrap();
        let heightmaps = encoded.get_compound("Heightmaps").unwrap();

        assert_eq!(
            first_heightmap_storage_value(&heightmaps.get_long_array("WORLD_SURFACE").unwrap()),
            128
        );
        assert_eq!(
            first_heightmap_storage_value(&heightmaps.get_long_array("OCEAN_FLOOR").unwrap()),
            125
        );
    }

    #[test]
    fn single_biome_section_omits_data_like_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_biome_id("minecraft:desert").unwrap();
        chunk.set_block_state_id(0, -64, 0, ids::BEDROCK).unwrap();

        let root = encode_decode(&chunk);
        let section = first_section(&root);
        let biomes = section.get_compound("biomes").unwrap();

        assert_eq!(biomes.get_list("palette").unwrap().values().len(), 1);
        assert!(!biomes.contains("data"));
    }

    #[test]
    fn mixed_biome_section_uses_first_seen_palette_order_like_java() {
        let mut chunk = ChunkModel::overworld(1, 2);
        chunk.set_biome_id("minecraft:plains").unwrap();
        chunk.set_block_state_id(0, -64, 0, ids::BEDROCK).unwrap();
        chunk
            .set_biome_id_at(0, -64, 0, "minecraft:desert")
            .unwrap();
        chunk
            .set_biome_id_at(4, -64, 0, "minecraft:jungle")
            .unwrap();
        chunk
            .set_biome_id_at(8, -64, 0, "minecraft:plains")
            .unwrap();

        let root = encode_decode(&chunk);
        let section = first_section(&root);
        let biome_tag = section.get_compound("biomes").unwrap();
        let palette = biome_tag.get_list("palette").unwrap();

        assert_eq!(palette.values().len(), 3);
        assert_eq!(string_list_value(palette, 0), "minecraft:desert");
        assert_eq!(string_list_value(palette, 1), "minecraft:jungle");
        assert_eq!(string_list_value(palette, 2), "minecraft:plains");
        assert_eq!(biome_tag.get_long_array("data").unwrap().len(), 2);
    }

    #[test]
    fn biome_only_section_is_encoded_like_java() {
        let mut chunk = ChunkModel::overworld(3, 4);
        chunk.set_biome_id("minecraft:plains").unwrap();
        chunk
            .set_biome_id_at(0, 80, 0, "minecraft:snowy_plains")
            .unwrap();

        let root = encode_decode(&chunk);
        let sections = root.get_list("sections").unwrap();
        assert_eq!(sections.values().len(), 1);
        let section = first_section(&root);
        assert_eq!(section.get_byte("Y").unwrap(), 5);
        assert_eq!(
            section
                .get_compound("block_states")
                .unwrap()
                .get_list("palette")
                .unwrap()
                .values()
                .len(),
            1
        );
        assert_eq!(
            section
                .get_compound("biomes")
                .unwrap()
                .get_list("palette")
                .unwrap()
                .values()
                .len(),
            2
        );
    }

    #[test]
    fn block_entities_preserve_order_like_java() {
        let mut chunk = ChunkModel::overworld(0, 0);
        chunk.set_block_state_id(1, 64, 1, ids::CHEST).unwrap();
        chunk.set_block_state_id(2, 64, 2, ids::SPAWNER).unwrap();
        let mut chest = nbt::compound();
        chest
            .put_string("id", "minecraft:chest")
            .unwrap()
            .put_int("x", 1)
            .unwrap()
            .put_int("y", 64)
            .unwrap()
            .put_int("z", 1)
            .unwrap();
        let mut spawner = nbt::compound();
        let mut entity = nbt::compound();
        entity.put_string("id", "minecraft:blaze").unwrap();
        let mut spawn_data = nbt::compound();
        spawn_data.put_compound("entity", entity).unwrap();
        spawner
            .put_string("id", "minecraft:mob_spawner")
            .unwrap()
            .put_int("x", 2)
            .unwrap()
            .put_int("y", 64)
            .unwrap()
            .put_int("z", 2)
            .unwrap()
            .put_compound("SpawnData", spawn_data)
            .unwrap();
        chunk.add_block_entity(chest);
        chunk.add_block_entity(spawner);

        let root = encode_decode(&chunk);
        let block_entities = root.get_list("block_entities").unwrap();

        assert_eq!(block_entities.values().len(), 2);
        let Tag::Compound(chest) = &block_entities.values()[0] else {
            panic!("first block entity must be compound");
        };
        let Tag::Compound(spawner) = &block_entities.values()[1] else {
            panic!("second block entity must be compound");
        };
        assert_eq!(chest.get_string("id").unwrap(), "minecraft:chest");
        assert_eq!(spawner.get_string("id").unwrap(), "minecraft:mob_spawner");
        assert_eq!(
            spawner
                .get_compound("SpawnData")
                .unwrap()
                .get_compound("entity")
                .unwrap()
                .get_string("id")
                .unwrap(),
            "minecraft:blaze"
        );
    }

    #[test]
    fn encoded_payload_has_empty_root_name_like_java() {
        let chunk = ChunkModel::overworld(0, 0);
        let payload = encode_to_bytes(&chunk, 0).unwrap();
        let named = nbt::read_from_bytes(&payload).unwrap();

        assert_eq!(named.name(), "");
        assert!(matches!(named.tag(), Tag::Compound(_)));
    }

    fn encode_decode(chunk: &ChunkModel) -> Compound {
        let payload = encode_to_bytes(chunk, 0).unwrap();
        let named = nbt::read_from_bytes(&payload).unwrap();
        let Tag::Compound(root) = named.into_tag() else {
            panic!("root must be compound");
        };
        root
    }

    fn first_section(root: &Compound) -> &Compound {
        let sections = root.get_list("sections").unwrap();
        let Tag::Compound(section) = &sections.values()[0] else {
            panic!("section must be compound");
        };
        section
    }

    fn string_list_value(list: &nbt::ListTag, index: usize) -> &str {
        let Tag::String(value) = &list.values()[index] else {
            panic!("list value must be string");
        };
        value
    }

    fn first_heightmap_storage_value(data: &[i64]) -> i32 {
        (data[0] & 0x1ff) as i32
    }
}
