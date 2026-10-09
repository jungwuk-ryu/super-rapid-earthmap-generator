pub(super) const FOREST: u32 = 1 << 0;
pub(super) const TAIGA: u32 = 1 << 1;
pub(super) const JUNGLE: u32 = 1 << 2;
pub(super) const PLAINS: u32 = 1 << 3;
pub(super) const MEADOW: u32 = 1 << 4;
pub(super) const GROVE: u32 = 1 << 5;
pub(super) const WINDSWEPT: u32 = 1 << 6;
pub(super) const MOUNTAIN: u32 = 1 << 7;
pub(super) const HILL: u32 = 1 << 8;
pub(super) const STONY: u32 = 1 << 9;
pub(super) const PEAK: u32 = 1 << 10;
pub(super) const JAGGED: u32 = 1 << 11;
pub(super) const BEACH: u32 = 1 << 12;
pub(super) const OCEAN: u32 = 1 << 13;
pub(super) const RIVER: u32 = 1 << 14;
pub(super) const SHORE: u32 = 1 << 15;
pub(super) const DESERT: u32 = 1 << 16;
pub(super) const BADLANDS: u32 = 1 << 17;
pub(super) const SAVANNA: u32 = 1 << 18;
pub(super) const STEPPE: u32 = 1 << 19;
pub(super) const GRASSLAND: u32 = 1 << 20;
pub(super) const SWAMP: u32 = 1 << 21;
pub(super) const MANGROVE: u32 = 1 << 22;
pub(super) const WETLAND: u32 = 1 << 23;
pub(super) const SNOW: u32 = 1 << 24;
pub(super) const FROZEN: u32 = 1 << 25;
pub(super) const ICE: u32 = 1 << 26;

const KEYWORDS: &[&str] = &[
    "forest",
    "taiga",
    "jungle",
    "plains",
    "meadow",
    "grove",
    "windswept",
    "mountain",
    "hill",
    "stony",
    "peak",
    "jagged",
    "beach",
    "ocean",
    "river",
    "shore",
    "desert",
    "badlands",
    "savanna",
    "steppe",
    "grassland",
    "swamp",
    "mangrove",
    "wetland",
    "snow",
    "frozen",
    "ice",
];

const fn keyword_mask(name: &str) -> u32 {
    let name = name.as_bytes();
    let mut flags = 0;
    let mut term = 0;
    while term < KEYWORDS.len() {
        let needle = KEYWORDS[term].as_bytes();
        let mut start = 0;
        while start + needle.len() <= name.len() {
            let mut index = 0;
            while index < needle.len() && name[start + index] == needle[index] {
                index += 1;
            }
            if index == needle.len() {
                flags |= 1 << term;
                break;
            }
            start += 1;
        }
        term += 1;
    }
    flags
}

// Compile each vanilla identifier's keyword mask; fallback callers preserve
// arbitrary custom names and their original matching/case rules.
#[inline]
pub(super) fn known_biome_keywords(biome: &str) -> Option<u32> {
    macro_rules! known {
        ($($name:literal),* $(,)?) => { match biome {
            $($name => { const FLAGS: u32 = keyword_mask($name); Some(FLAGS) },)*
            _ => None,
        } };
    }
    known!(
        "minecraft:badlands",
        "minecraft:bamboo_jungle",
        "minecraft:beach",
        "minecraft:birch_forest",
        "minecraft:cold_ocean",
        "minecraft:dark_forest",
        "minecraft:deep_cold_ocean",
        "minecraft:deep_frozen_ocean",
        "minecraft:deep_lukewarm_ocean",
        "minecraft:deep_ocean",
        "minecraft:desert",
        "minecraft:eroded_badlands",
        "minecraft:flower_forest",
        "minecraft:forest",
        "minecraft:frozen_ocean",
        "minecraft:frozen_river",
        "minecraft:jagged_peaks",
        "minecraft:jungle",
        "minecraft:lukewarm_ocean",
        "minecraft:mangrove_swamp",
        "minecraft:meadow",
        "minecraft:mushroom_fields",
        "minecraft:ocean",
        "minecraft:old_growth_birch_forest",
        "minecraft:old_growth_pine_taiga",
        "minecraft:old_growth_spruce_taiga",
        "minecraft:plains",
        "minecraft:savanna",
        "minecraft:savanna_plateau",
        "minecraft:snowy_beach",
        "minecraft:snowy_plains",
        "minecraft:snowy_taiga",
        "minecraft:sparse_jungle",
        "minecraft:stony_peaks",
        "minecraft:sunflower_plains",
        "minecraft:swamp",
        "minecraft:taiga",
        "minecraft:warm_ocean",
        "minecraft:windswept_hills",
        "minecraft:windswept_savanna",
        "minecraft:wooded_badlands",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_masks_match_substrings_and_exclude_custom_names() {
        for name in [
            "minecraft:badlands",
            "minecraft:bamboo_jungle",
            "minecraft:beach",
            "minecraft:birch_forest",
            "minecraft:cold_ocean",
            "minecraft:dark_forest",
            "minecraft:deep_cold_ocean",
            "minecraft:deep_frozen_ocean",
            "minecraft:deep_lukewarm_ocean",
            "minecraft:deep_ocean",
            "minecraft:desert",
            "minecraft:eroded_badlands",
            "minecraft:flower_forest",
            "minecraft:forest",
            "minecraft:frozen_ocean",
            "minecraft:frozen_river",
            "minecraft:jagged_peaks",
            "minecraft:jungle",
            "minecraft:lukewarm_ocean",
            "minecraft:mangrove_swamp",
            "minecraft:meadow",
            "minecraft:mushroom_fields",
            "minecraft:ocean",
            "minecraft:old_growth_birch_forest",
            "minecraft:old_growth_pine_taiga",
            "minecraft:old_growth_spruce_taiga",
            "minecraft:plains",
            "minecraft:savanna",
            "minecraft:savanna_plateau",
            "minecraft:snowy_beach",
            "minecraft:snowy_plains",
            "minecraft:snowy_taiga",
            "minecraft:sparse_jungle",
            "minecraft:stony_peaks",
            "minecraft:sunflower_plains",
            "minecraft:swamp",
            "minecraft:taiga",
            "minecraft:warm_ocean",
            "minecraft:windswept_hills",
            "minecraft:windswept_savanna",
            "minecraft:wooded_badlands",
        ] {
            let expected = KEYWORDS
                .iter()
                .enumerate()
                .fold(0, |flags, (index, needle)| {
                    flags | if name.contains(needle) { 1 << index } else { 0 }
                });
            assert_eq!(known_biome_keywords(name), Some(expected), "{name}");
            for candidate in [
                name.to_string(),
                name.to_uppercase(),
                format!("custom:{name}"),
            ] {
                assert_eq!(
                    super::super::is_protected_intent_biome(&candidate),
                    candidate == "minecraft:beach"
                        || candidate.contains("snow")
                        || candidate.contains("swamp")
                        || candidate.contains("mangrove"),
                    "{candidate}",
                );
            }
            assert_eq!(known_biome_keywords(&name.to_uppercase()), None);
            assert_eq!(known_biome_keywords(&format!("custom:{name}")), None);
        }
    }
}
