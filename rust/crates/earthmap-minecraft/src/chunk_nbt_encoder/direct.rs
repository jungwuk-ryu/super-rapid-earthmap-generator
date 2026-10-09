//! Stream the existing chunk schema, preserving insertion and palette order.
//! The tree-producing encoder remains the format oracle for regression tests.

use super::*;
use std::sync::OnceLock;

pub(super) fn encode(
    chunk: &ChunkModel,
    last_update: i64,
    status: ChunkGenerationStatus,
) -> Result<Vec<u8>> {
    let [surface, floor, no_leaves] = compute_heightmaps(chunk)?;
    let mut output = Vec::with_capacity(4096);
    header(&mut output, nbt::TAG_COMPOUND, "")?;
    named(&mut output, "DataVersion", &Tag::Int(DATA_VERSION))?;
    named(&mut output, "xPos", &Tag::Int(chunk.chunk_x()))?;
    named(
        &mut output,
        "yPos",
        &Tag::Int(chunk.dimension().min_section_y()),
    )?;
    named(&mut output, "zPos", &Tag::Int(chunk.chunk_z()))?;
    named(&mut output, "LastUpdate", &Tag::Long(last_update))?;
    named(&mut output, "InhabitedTime", &Tag::Long(0))?;
    header(&mut output, nbt::TAG_STRING, "Status")?;
    nbt::write_utf(&mut output, status.id())?;
    named(
        &mut output,
        "isLightOn",
        &Tag::Byte(i8::from(status.light_on())),
    )?;

    let sections = (0..chunk.section_count() as i32)
        .filter_map(|index| {
            match (
                chunk.is_section_allocated(index),
                chunk.is_biome_section_allocated(index),
            ) {
                (Ok(false), Ok(false)) => None,
                (Ok(_), Ok(_)) => Some(Ok(index)),
                (Err(error), _) | (_, Err(error)) => Some(Err(error)),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    list_header(&mut output, "sections", nbt::TAG_COMPOUND, sections.len())?;
    for index in sections {
        named(
            &mut output,
            "Y",
            &nbt::byte_tag(chunk.section_y_for_index(index)?),
        )?;
        write_blocks(&mut output, chunk.section_blocks(index)?)?;
        header(&mut output, nbt::TAG_COMPOUND, "biomes")?;
        if chunk.is_biome_section_allocated(index)? {
            biome_tag(chunk.section_biome_ids(index)?)?.write_payload(&mut output)?;
        } else {
            list_header(&mut output, "palette", nbt::TAG_STRING, 1)?;
            nbt::write_utf(&mut output, chunk.biome_id())?;
            output.push(nbt::TAG_END);
        }
        output.push(nbt::TAG_END);
    }
    header(&mut output, nbt::TAG_COMPOUND, "Heightmaps")?;
    heightmaps(&surface, &floor, &no_leaves)?.write_payload(&mut output)?;
    list_header(
        &mut output,
        "block_entities",
        nbt::TAG_COMPOUND,
        chunk.block_entities().len(),
    )?;
    for entity in chunk.block_entities() {
        entity.write_payload(&mut output)?;
    }
    list_header(&mut output, "block_ticks", nbt::TAG_COMPOUND, 0)?;
    list_header(&mut output, "fluid_ticks", nbt::TAG_COMPOUND, 0)?;
    header(&mut output, nbt::TAG_COMPOUND, "structures")?;
    header(&mut output, nbt::TAG_COMPOUND, "starts")?;
    output.push(nbt::TAG_END);
    header(&mut output, nbt::TAG_COMPOUND, "References")?;
    output.extend_from_slice(&[nbt::TAG_END, nbt::TAG_END, nbt::TAG_END]);
    Ok(output)
}

fn write_blocks(output: &mut Vec<u8>, section: Option<&BlockSection>) -> Result<()> {
    header(output, nbt::TAG_COMPOUND, "block_states")?;
    let palette = match section {
        Some(BlockSection::Dense(blocks)) => SectionPalette::pack_block_states(blocks)?,
        Some(BlockSection::Uniform(block)) => SectionPalette::uniform(*block)?,
        None => SectionPalette::uniform(ids::AIR)?,
    };
    list_header(output, "palette", nbt::TAG_COMPOUND, palette.palette_size())?;
    for &block in palette.palette_block_state_ids() {
        if let Some(bytes) = cached_block_payloads()
            .get(block as usize)
            .and_then(Option::as_ref)
        {
            output.extend_from_slice(bytes);
        } else {
            block_state_tag(block)?.write_payload(output)?;
        }
    }
    if palette.bits_per_entry() > 0 {
        header(output, nbt::TAG_LONG_ARRAY, "data")?;
        let words = palette.packed_data_words();
        output.extend_from_slice(&(words.len() as i32).to_be_bytes());
        for word in words {
            output.extend_from_slice(&word.to_be_bytes());
        }
    }
    output.push(nbt::TAG_END);
    Ok(())
}

fn cached_block_payloads() -> &'static [Option<Vec<u8>>] {
    static PAYLOADS: OnceLock<Vec<Option<Vec<u8>>>> = OnceLock::new();
    PAYLOADS.get_or_init(|| {
        // Keep this cache bounded. Other IDs still use the existing encoder,
        // including its error for unmapped block states.
        (0..=ids::DRIPSTONE_BLOCK)
            .map(|id| {
                let mut bytes = Vec::new();
                block_state_tag(id)
                    .and_then(|tag| tag.write_payload(&mut bytes))
                    .ok()?;
                Some(bytes)
            })
            .collect()
    })
}

fn header(output: &mut Vec<u8>, kind: u8, name: &str) -> Result<()> {
    output.push(kind);
    nbt::write_utf(output, name)
}

fn named(output: &mut Vec<u8>, name: &str, value: &Tag) -> Result<()> {
    header(output, value.type_id(), name)?;
    value.write_payload(output)
}

fn list_header(output: &mut Vec<u8>, name: &str, kind: u8, length: usize) -> Result<()> {
    let length =
        i32::try_from(length).map_err(|_| MinecraftError::invalid("NBT list too large"))?;
    header(output, nbt::TAG_LIST, name)?;
    output.push(kind);
    output.extend_from_slice(&length.to_be_bytes());
    Ok(())
}
