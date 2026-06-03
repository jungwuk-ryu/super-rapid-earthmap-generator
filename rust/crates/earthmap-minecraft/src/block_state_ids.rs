use crate::{MinecraftError, Result};

pub const AIR: i32 = 0;
pub const STONE: i32 = 1;
pub const WATER: i32 = 2;
pub const DIRT: i32 = 3;
pub const GRASS_BLOCK: i32 = 4;
pub const BEDROCK: i32 = 5;
pub const SAND: i32 = 6;
pub const SNOW_BLOCK: i32 = 7;
pub const ICE: i32 = 8;
pub const DEEPSLATE: i32 = 9;
pub const COAL_ORE: i32 = 10;
pub const IRON_ORE: i32 = 11;
pub const COPPER_ORE: i32 = 12;
pub const GOLD_ORE: i32 = 13;
pub const REDSTONE_ORE: i32 = 14;
pub const LAPIS_ORE: i32 = 15;
pub const DIAMOND_ORE: i32 = 16;
pub const EMERALD_ORE: i32 = 17;
pub const DEEPSLATE_COAL_ORE: i32 = 18;
pub const DEEPSLATE_IRON_ORE: i32 = 19;
pub const DEEPSLATE_COPPER_ORE: i32 = 20;
pub const DEEPSLATE_GOLD_ORE: i32 = 21;
pub const DEEPSLATE_REDSTONE_ORE: i32 = 22;
pub const DEEPSLATE_LAPIS_ORE: i32 = 23;
pub const DEEPSLATE_DIAMOND_ORE: i32 = 24;
pub const DEEPSLATE_EMERALD_ORE: i32 = 25;
pub const LAVA: i32 = 26;
pub const CHEST: i32 = 27;
pub const SPAWNER: i32 = 28;
pub const END_PORTAL_FRAME: i32 = 29;
pub const END_PORTAL: i32 = 30;
pub const STONE_BRICKS: i32 = 31;
pub const END_PORTAL_FRAME_FILLED: i32 = 32;
pub const OAK_LOG: i32 = 33;
pub const OAK_LEAVES: i32 = 34;
pub const JUNGLE_LOG: i32 = 35;
pub const JUNGLE_LEAVES: i32 = 36;
pub const GRAVEL: i32 = 37;
pub const CLAY: i32 = 38;
pub const RED_SAND: i32 = 39;
pub const COARSE_DIRT: i32 = 40;
pub const TERRACOTTA: i32 = 41;
pub const ORANGE_TERRACOTTA: i32 = 42;
pub const BROWN_TERRACOTTA: i32 = 43;
pub const MUD: i32 = 44;
pub const MOSS_BLOCK: i32 = 45;
pub const PODZOL: i32 = 46;
pub const WHITE_TERRACOTTA: i32 = 47;
pub const LIGHT_GRAY_TERRACOTTA: i32 = 48;
pub const GRAY_TERRACOTTA: i32 = 49;
pub const BLACK_TERRACOTTA: i32 = 50;
pub const YELLOW_TERRACOTTA: i32 = 51;
pub const RED_TERRACOTTA: i32 = 52;
pub const GREEN_TERRACOTTA: i32 = 53;
pub const CYAN_TERRACOTTA: i32 = 54;
pub const LIME_TERRACOTTA: i32 = 55;
pub const PACKED_MUD: i32 = 56;
pub const CALCITE: i32 = 57;
pub const TUFF: i32 = 58;
pub const SANDSTONE: i32 = 59;
pub const ROOTED_DIRT: i32 = 60;
pub const MYCELIUM: i32 = 61;
pub const ANDESITE: i32 = 62;
pub const GRANITE: i32 = 63;
pub const DIORITE: i32 = 64;
pub const DARK_OAK_LEAVES: i32 = 65;
pub const SPRUCE_LEAVES: i32 = 66;
pub const BLACK_CONCRETE: i32 = 67;
pub const QUARTZ_BLOCK: i32 = 68;
pub const BONE_BLOCK: i32 = 69;
pub const END_STONE: i32 = 70;
pub const END_STONE_BRICKS: i32 = 71;
pub const SMOOTH_SANDSTONE: i32 = 72;
pub const CUT_SANDSTONE: i32 = 73;
pub const CHISELED_SANDSTONE: i32 = 74;
pub const SMOOTH_RED_SANDSTONE: i32 = 75;
pub const CUT_RED_SANDSTONE: i32 = 76;
pub const CHISELED_RED_SANDSTONE: i32 = 77;
pub const MUD_BRICKS: i32 = 78;
pub const DRIPSTONE_BLOCK: i32 = 79;

pub fn require_valid(block_state_id: i32) -> Result<()> {
    if block_state_id < 0 {
        return Err(MinecraftError::invalid(format!(
            "blockStateId must be non-negative: {block_state_id}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_java_ids() {
        let ids = [
            AIR,
            STONE,
            WATER,
            DIRT,
            GRASS_BLOCK,
            BEDROCK,
            SAND,
            SNOW_BLOCK,
            ICE,
            DEEPSLATE,
            COAL_ORE,
            IRON_ORE,
            COPPER_ORE,
            GOLD_ORE,
            REDSTONE_ORE,
            LAPIS_ORE,
            DIAMOND_ORE,
            EMERALD_ORE,
            DEEPSLATE_COAL_ORE,
            DEEPSLATE_IRON_ORE,
            DEEPSLATE_COPPER_ORE,
            DEEPSLATE_GOLD_ORE,
            DEEPSLATE_REDSTONE_ORE,
            DEEPSLATE_LAPIS_ORE,
            DEEPSLATE_DIAMOND_ORE,
            DEEPSLATE_EMERALD_ORE,
            LAVA,
            CHEST,
            SPAWNER,
            END_PORTAL_FRAME,
            END_PORTAL,
            STONE_BRICKS,
            END_PORTAL_FRAME_FILLED,
            OAK_LOG,
            OAK_LEAVES,
            JUNGLE_LOG,
            JUNGLE_LEAVES,
            GRAVEL,
            CLAY,
            RED_SAND,
            COARSE_DIRT,
            TERRACOTTA,
            ORANGE_TERRACOTTA,
            BROWN_TERRACOTTA,
            MUD,
            MOSS_BLOCK,
            PODZOL,
            WHITE_TERRACOTTA,
            LIGHT_GRAY_TERRACOTTA,
            GRAY_TERRACOTTA,
            BLACK_TERRACOTTA,
            YELLOW_TERRACOTTA,
            RED_TERRACOTTA,
            GREEN_TERRACOTTA,
            CYAN_TERRACOTTA,
            LIME_TERRACOTTA,
            PACKED_MUD,
            CALCITE,
            TUFF,
            SANDSTONE,
            ROOTED_DIRT,
            MYCELIUM,
            ANDESITE,
            GRANITE,
            DIORITE,
            DARK_OAK_LEAVES,
            SPRUCE_LEAVES,
            BLACK_CONCRETE,
            QUARTZ_BLOCK,
            BONE_BLOCK,
            END_STONE,
            END_STONE_BRICKS,
            SMOOTH_SANDSTONE,
            CUT_SANDSTONE,
            CHISELED_SANDSTONE,
            SMOOTH_RED_SANDSTONE,
            CUT_RED_SANDSTONE,
            CHISELED_RED_SANDSTONE,
            MUD_BRICKS,
            DRIPSTONE_BLOCK,
        ];
        assert_eq!(ids.len(), 80);
        for (expected, actual) in ids.into_iter().enumerate() {
            assert_eq!(actual, expected as i32);
        }
    }

    #[test]
    fn validation_matches_java_lower_bound() {
        assert!(require_valid(0).is_ok());
        assert!(require_valid(999).is_ok());
        assert!(require_valid(-1).is_err());
    }
}
