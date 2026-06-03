use std::fs;
use std::path::Path;

use crate::block_state_ids as ids;
use crate::chunk_model::ChunkModel;
use crate::chunk_nbt_encoder;
use crate::level_dat_template;
use crate::nbt::{self, Tag};
use crate::Result;

pub const FIXTURE_NAMES: [&str; 7] = [
    "nbt-primitive-root.dat",
    "nbt-nested-empty-root.dat",
    "chunk-empty-full.nbt",
    "chunk-mixed-biome.nbt",
    "chunk-block-entities.nbt",
    "chunk-all-block-states.nbt",
    "leveldat-root-fixed.nbt",
];

pub const GZIP_FIXTURE_NAMES: [&str; 1] = ["leveldat-root-fixed.nbt.gz"];

pub fn write_nbt_parity_fixtures(output_dir: impl AsRef<Path>) -> Result<()> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;
    write_fixture(output_dir, FIXTURE_NAMES[0], nbt_primitive_fixture()?)?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[1],
        nbt_nested_empty_root_fixture()?,
    )?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[2],
        empty_full_chunk_payload_fixture()?,
    )?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[3],
        mixed_biome_chunk_payload_fixture()?,
    )?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[4],
        block_entity_chunk_payload_fixture()?,
    )?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[5],
        all_block_states_chunk_payload_fixture()?,
    )?;
    write_fixture(
        output_dir,
        FIXTURE_NAMES[6],
        level_dat_root_fixed_fixture()?,
    )?;
    Ok(())
}

pub fn chunk_payload_fixtures() -> Result<Vec<(&'static str, Vec<u8>)>> {
    Ok(vec![
        ("chunk-empty-full.nbt", empty_full_chunk_payload_fixture()?),
        (
            "chunk-mixed-biome.nbt",
            mixed_biome_chunk_payload_fixture()?,
        ),
        (
            "chunk-block-entities.nbt",
            block_entity_chunk_payload_fixture()?,
        ),
        (
            "chunk-all-block-states.nbt",
            all_block_states_chunk_payload_fixture()?,
        ),
    ])
}

pub fn write_nbt_gzip_parity_fixtures(output_dir: impl AsRef<Path>) -> Result<()> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;
    let settings = level_dat_template::default_settings("SR EarthMap Test", 123456789)?;
    let root = level_dat_template::create_root_with_last_played(&settings, 42)?;
    nbt::write_gzip(output_dir.join(GZIP_FIXTURE_NAMES[0]), "", &root)
}

fn write_fixture(output_dir: &Path, file_name: &str, bytes: Vec<u8>) -> Result<()> {
    fs::write(output_dir.join(file_name), bytes)?;
    Ok(())
}

fn nbt_primitive_fixture() -> Result<Vec<u8>> {
    let mut root = nbt::compound();
    root.put_byte("Byte", 1)?
        .put_short("Short", 2)?
        .put_int("Int", 123)?
        .put_long("Long", 456)?
        .put_float("Float", 1.25)?
        .put_double("Double", -2.5)?
        .put_string("String", "hello")?
        .put_string("UtfNull", "\0")?
        .put_string("UtfSupplementary", "\u{1d11e}")?
        .put_byte_array("ByteArray", vec![-1, 0, 1])?
        .put_int_array("IntArray", vec![1, -2, 3])?
        .put_long_array("LongArray", vec![7, -8])?;
    nbt::write_to_bytes("root", &root)
}

fn nbt_nested_empty_root_fixture() -> Result<Vec<u8>> {
    let mut stone = nbt::compound();
    stone.put_string("Name", "minecraft:stone")?;
    let mut water_properties = nbt::compound();
    water_properties.put_string("level", "0")?;
    let mut water = nbt::compound();
    water
        .put_string("Name", "minecraft:water")?
        .put_compound("Properties", water_properties)?;
    let mut nested = nbt::compound();
    nested.put_int("A", 1)?.put_string("B", "bee")?;

    let mut root = nbt::compound();
    root.put_int("Replace", 1)?
        .put_long("Replace", 2)?
        .put(
            "Strings",
            Tag::List(nbt::list(
                nbt::TAG_STRING,
                vec![
                    nbt::string_tag("vanilla"),
                    nbt::string_tag("minecraft:plains"),
                ],
            )?),
        )?
        .put(
            "Compounds",
            Tag::List(nbt::list(
                nbt::TAG_COMPOUND,
                vec![Tag::Compound(stone), Tag::Compound(water)],
            )?),
        )?
        .put_compound("Nested", nested)?;
    nbt::write_to_bytes("", &root)
}

fn empty_full_chunk_payload_fixture() -> Result<Vec<u8>> {
    chunk_nbt_encoder::encode_to_bytes(&ChunkModel::overworld(0, 0), 0)
}

fn mixed_biome_chunk_payload_fixture() -> Result<Vec<u8>> {
    chunk_nbt_encoder::encode_to_bytes(&mixed_biome_fixture_chunk()?, 0)
}

fn block_entity_chunk_payload_fixture() -> Result<Vec<u8>> {
    chunk_nbt_encoder::encode_to_bytes(&block_entity_fixture_chunk()?, 0)
}

fn all_block_states_chunk_payload_fixture() -> Result<Vec<u8>> {
    chunk_nbt_encoder::encode_to_bytes(&all_block_states_fixture_chunk()?, 0)
}

fn mixed_biome_fixture_chunk() -> Result<ChunkModel> {
    let mut chunk = ChunkModel::overworld(1, 2);
    chunk.set_biome_id("minecraft:plains")?;
    chunk.set_block_state_id(0, -64, 0, ids::BEDROCK)?;
    chunk.set_biome_id_at(0, -64, 0, "minecraft:desert")?;
    chunk.set_biome_id_at(4, -64, 0, "minecraft:jungle")?;
    chunk.set_biome_id_at(8, -64, 0, "minecraft:plains")?;
    Ok(chunk)
}

fn block_entity_fixture_chunk() -> Result<ChunkModel> {
    let mut chunk = ChunkModel::overworld(0, 0);
    chunk.set_block_state_id(1, 64, 1, ids::CHEST)?;
    chunk.set_block_state_id(2, 64, 2, ids::SPAWNER)?;
    let mut chest = nbt::compound();
    chest
        .put_string("id", "minecraft:chest")?
        .put_int("x", 1)?
        .put_int("y", 64)?
        .put_int("z", 1)?
        .put_string("LootTable", "minecraft:chests/simple_dungeon")?;
    let mut entity = nbt::compound();
    entity.put_string("id", "minecraft:blaze")?;
    let mut spawn_data = nbt::compound();
    spawn_data.put_compound("entity", entity)?;
    let mut spawner = nbt::compound();
    spawner
        .put_string("id", "minecraft:mob_spawner")?
        .put_int("x", 2)?
        .put_int("y", 64)?
        .put_int("z", 2)?
        .put_compound("SpawnData", spawn_data)?;
    chunk.add_block_entity(chest);
    chunk.add_block_entity(spawner);
    Ok(chunk)
}

fn all_block_states_fixture_chunk() -> Result<ChunkModel> {
    let mut chunk = ChunkModel::overworld(0, 0);
    for block_state_id in 1..=ids::DRIPSTONE_BLOCK {
        let block_index = block_state_id - 1;
        let local_x = block_index & 15;
        let local_z = (block_index >> 4) & 15;
        let local_y = (block_index >> 8) & 15;
        chunk.set_block_state_id(local_x, -64 + local_y, local_z, block_state_id)?;
    }
    Ok(chunk)
}

fn level_dat_root_fixed_fixture() -> Result<Vec<u8>> {
    let settings = level_dat_template::default_settings("SR EarthMap Test", 123456789)?;
    let root = level_dat_template::create_root_with_last_played(&settings, 42)?;
    nbt::write_to_bytes("", &root)
}
