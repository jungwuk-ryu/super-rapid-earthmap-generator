use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use earthmap_core::build_info;

use crate::nbt::{self, Compound, Tag};
use crate::{MinecraftError, Result};

pub const DATA_VERSION: i32 = 4671;
pub const LEVEL_FORMAT_VERSION: i32 = 19133;
pub const GAME_TYPE_SURVIVAL: i32 = 0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    level_name: String,
    seed: i64,
    spawn_x: i32,
    spawn_y: i32,
    spawn_z: i32,
}

impl Settings {
    pub fn new(
        level_name: impl Into<String>,
        seed: i64,
        spawn_x: i32,
        spawn_y: i32,
        spawn_z: i32,
    ) -> Result<Self> {
        let level_name = level_name.into();
        if level_name.trim().is_empty() {
            return Err(MinecraftError::invalid("levelName must not be blank"));
        }
        Ok(Self {
            level_name,
            seed,
            spawn_x,
            spawn_y,
            spawn_z,
        })
    }

    pub fn default_settings(level_name: impl Into<String>, seed: i64) -> Result<Self> {
        Self::new(level_name, seed, 0, 80, 0)
    }

    pub fn level_name(&self) -> &str {
        &self.level_name
    }

    pub fn seed(&self) -> i64 {
        self.seed
    }

    pub fn spawn_x(&self) -> i32 {
        self.spawn_x
    }

    pub fn spawn_y(&self) -> i32 {
        self.spawn_y
    }

    pub fn spawn_z(&self) -> i32 {
        self.spawn_z
    }
}

pub fn default_settings(level_name: impl Into<String>, seed: i64) -> Result<Settings> {
    Settings::default_settings(level_name, seed)
}

pub fn create_root(settings: &Settings) -> Result<Compound> {
    create_root_with_last_played(settings, current_time_millis())
}

pub fn create_root_with_last_played(settings: &Settings, last_played: i64) -> Result<Compound> {
    let mut data = nbt::compound();
    data.put_int("DataVersion", DATA_VERSION)?
        .put_int("version", LEVEL_FORMAT_VERSION)?
        .put_string("LevelName", settings.level_name())?
        .put_long("RandomSeed", settings.seed())?
        .put_int("GameType", GAME_TYPE_SURVIVAL)?
        .put_byte("Difficulty", 1)?
        .put_byte("DifficultyLocked", 0)?
        .put_byte("hardcore", 0)?
        .put_byte("allowCommands", 0)?
        .put_byte("initialized", 1)?
        .put_byte("WasModded", 0)?
        .put_int("SpawnX", settings.spawn_x())?
        .put_int("SpawnY", settings.spawn_y())?
        .put_int("SpawnZ", settings.spawn_z())?
        .put_compound("spawn", spawn_compound(settings)?)?
        .put_long("Time", 0)?
        .put_long("DayTime", 0)?
        .put_long("LastPlayed", last_played)?
        .put_int("clearWeatherTime", 0)?
        .put_int("rainTime", 0)?
        .put_byte("raining", 0)?
        .put_int("thunderTime", 0)?
        .put_byte("thundering", 0)?
        .put_int("WanderingTraderSpawnDelay", 24000)?
        .put_int("WanderingTraderSpawnChance", 25)?
        .put_compound("Version", version_compound()?)?
        .put_compound("game_rules", game_rules_compound()?)?
        .put_compound("DataPacks", data_packs_compound()?)?
        .put(
            "ServerBrands",
            Tag::List(nbt::list(
                nbt::TAG_STRING,
                vec![nbt::string_tag("vanilla")],
            )?),
        )?
        .put(
            "ScheduledEvents",
            Tag::List(nbt::list(nbt::TAG_COMPOUND, Vec::new())?),
        )?
        .put_compound("CustomBossEvents", nbt::compound())?
        .put_compound("DragonFight", nbt::compound())?
        .put_compound(
            "WorldGenSettings",
            world_gen_settings_compound(settings.seed())?,
        )?;

    let mut root = nbt::compound();
    root.put_compound("Data", data)?;
    Ok(root)
}

pub fn write(path: impl AsRef<Path>, settings: &Settings) -> Result<()> {
    nbt::write_gzip(path, "", &create_root(settings)?)
}

fn version_compound() -> Result<Compound> {
    let mut version = nbt::compound();
    version
        .put_int("Id", DATA_VERSION)?
        .put_string("Name", build_info::MINECRAFT_TARGET)?
        .put_string("Series", "main")?
        .put_byte("Snapshot", 0)?;
    Ok(version)
}

fn spawn_compound(settings: &Settings) -> Result<Compound> {
    let mut spawn = nbt::compound();
    spawn
        .put(
            "pos",
            nbt::int_array(vec![
                settings.spawn_x(),
                settings.spawn_y(),
                settings.spawn_z(),
            ]),
        )?
        .put_string("dimension", "minecraft:overworld")?
        .put_float("yaw", 0.0)?
        .put_float("pitch", 0.0)?;
    Ok(spawn)
}

fn game_rules_compound() -> Result<Compound> {
    let mut game_rules = nbt::compound();
    game_rules
        .put_byte("minecraft:advance_time", 1)?
        .put_byte("minecraft:advance_weather", 1)?
        .put_byte("minecraft:allow_entering_nether_using_portals", 1)?
        .put_byte("minecraft:block_drops", 1)?
        .put_byte("minecraft:block_explosion_drop_decay", 1)?
        .put_byte("minecraft:command_block_output", 1)?
        .put_byte("minecraft:command_blocks_work", 1)?
        .put_byte("minecraft:drowning_damage", 1)?
        .put_byte("minecraft:elytra_movement_check", 1)?
        .put_byte("minecraft:ender_pearls_vanish_on_death", 1)?
        .put_byte("minecraft:entity_drops", 1)?
        .put_byte("minecraft:fall_damage", 1)?
        .put_byte("minecraft:fire_damage", 1)?
        .put_int("minecraft:fire_spread_radius_around_player", 128)?
        .put_byte("minecraft:forgive_dead_players", 1)?
        .put_byte("minecraft:freeze_damage", 1)?
        .put_byte("minecraft:global_sound_events", 1)?
        .put_byte("minecraft:immediate_respawn", 0)?
        .put_byte("minecraft:keep_inventory", 0)?
        .put_byte("minecraft:lava_source_conversion", 0)?
        .put_byte("minecraft:limited_crafting", 0)?
        .put_byte("minecraft:locator_bar", 1)?
        .put_byte("minecraft:log_admin_commands", 1)?
        .put_int("minecraft:max_block_modifications", 32768)?
        .put_int("minecraft:max_command_forks", 65536)?
        .put_int("minecraft:max_command_sequence_length", 65536)?
        .put_int("minecraft:max_entity_cramming", 24)?
        .put_int("minecraft:max_snow_accumulation_height", 1)?
        .put_byte("minecraft:mob_drops", 1)?
        .put_byte("minecraft:mob_explosion_drop_decay", 1)?
        .put_byte("minecraft:mob_griefing", 1)?
        .put_byte("minecraft:natural_health_regeneration", 1)?
        .put_byte("minecraft:player_movement_check", 1)?
        .put_int("minecraft:players_nether_portal_creative_delay", 1)?
        .put_int("minecraft:players_nether_portal_default_delay", 80)?
        .put_int("minecraft:players_sleeping_percentage", 100)?
        .put_byte("minecraft:projectiles_can_break_blocks", 1)?
        .put_byte("minecraft:pvp", 1)?
        .put_byte("minecraft:raids", 1)?
        .put_int("minecraft:random_tick_speed", 3)?
        .put_byte("minecraft:reduced_debug_info", 0)?
        .put_int("minecraft:respawn_radius", 10)?
        .put_byte("minecraft:send_command_feedback", 1)?
        .put_byte("minecraft:show_advancement_messages", 1)?
        .put_byte("minecraft:show_death_messages", 1)?
        .put_byte("minecraft:spawn_mobs", 1)?
        .put_byte("minecraft:spawn_monsters", 1)?
        .put_byte("minecraft:spawn_patrols", 1)?
        .put_byte("minecraft:spawn_phantoms", 1)?
        .put_byte("minecraft:spawn_wandering_traders", 1)?
        .put_byte("minecraft:spawn_wardens", 1)?
        .put_byte("minecraft:spawner_blocks_work", 1)?
        .put_byte("minecraft:spectators_generate_chunks", 1)?
        .put_byte("minecraft:spread_vines", 1)?
        .put_byte("minecraft:tnt_explodes", 1)?
        .put_byte("minecraft:tnt_explosion_drop_decay", 0)?
        .put_byte("minecraft:universal_anger", 0)?
        .put_byte("minecraft:water_source_conversion", 1)?;
    Ok(game_rules)
}

fn data_packs_compound() -> Result<Compound> {
    let mut data_packs = nbt::compound();
    data_packs
        .put(
            "Enabled",
            Tag::List(nbt::list(
                nbt::TAG_STRING,
                vec![nbt::string_tag("vanilla")],
            )?),
        )?
        .put(
            "Disabled",
            Tag::List(nbt::list(
                nbt::TAG_STRING,
                vec![
                    nbt::string_tag("minecart_improvements"),
                    nbt::string_tag("redstone_experiments"),
                    nbt::string_tag("trade_rebalance"),
                ],
            )?),
        )?;
    Ok(data_packs)
}

fn world_gen_settings_compound(seed: i64) -> Result<Compound> {
    let mut dimensions = nbt::compound();
    dimensions
        .put_compound(
            "minecraft:overworld",
            dimension(
                "minecraft:overworld",
                noise_generator(
                    "minecraft:overworld",
                    multi_noise_biome_source("minecraft:overworld")?,
                )?,
            )?,
        )?
        .put_compound(
            "minecraft:the_nether",
            dimension(
                "minecraft:the_nether",
                noise_generator(
                    "minecraft:nether",
                    multi_noise_biome_source("minecraft:nether")?,
                )?,
            )?,
        )?;
    let mut end_biome_source = nbt::compound();
    end_biome_source.put_string("type", "minecraft:the_end")?;
    dimensions.put_compound(
        "minecraft:the_end",
        dimension(
            "minecraft:the_end",
            noise_generator("minecraft:end", end_biome_source)?,
        )?,
    )?;

    let mut world_gen = nbt::compound();
    world_gen
        .put_long("seed", seed)?
        .put_byte("generate_features", 1)?
        .put_byte("bonus_chest", 0)?
        .put_compound("dimensions", dimensions)?;
    Ok(world_gen)
}

fn dimension(type_id: &str, generator: Compound) -> Result<Compound> {
    let mut dimension = nbt::compound();
    dimension
        .put_string("type", type_id)?
        .put_compound("generator", generator)?;
    Ok(dimension)
}

fn noise_generator(settings: &str, biome_source: Compound) -> Result<Compound> {
    let mut generator = nbt::compound();
    generator
        .put_string("type", "minecraft:noise")?
        .put_string("settings", settings)?
        .put_compound("biome_source", biome_source)?;
    Ok(generator)
}

fn multi_noise_biome_source(preset: &str) -> Result<Compound> {
    let mut biome_source = nbt::compound();
    biome_source
        .put_string("type", "minecraft:multi_noise")?
        .put_string("preset", preset)?;
    Ok(biome_source)
}

fn current_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_match_java() {
        let settings = default_settings("SR EarthMap Test", 123456789).unwrap();

        assert_eq!(settings.level_name(), "SR EarthMap Test");
        assert_eq!(settings.seed(), 123456789);
        assert_eq!(settings.spawn_x(), 0);
        assert_eq!(settings.spawn_y(), 80);
        assert_eq!(settings.spawn_z(), 0);
        assert!(default_settings("", 0).is_err());
    }

    #[test]
    fn root_contains_java_level_dat_contract_fields() {
        let settings = default_settings("SR EarthMap Test", 123456789).unwrap();
        let root = create_root_with_last_played(&settings, 42).unwrap();
        let data = root.get_compound("Data").unwrap();

        assert_eq!(data.get_int("DataVersion").unwrap(), DATA_VERSION);
        assert_eq!(data.get_int("version").unwrap(), LEVEL_FORMAT_VERSION);
        assert_eq!(data.get_string("LevelName").unwrap(), "SR EarthMap Test");
        assert_eq!(data.get_long("RandomSeed").unwrap(), 123456789);
        assert_eq!(data.get_int("GameType").unwrap(), GAME_TYPE_SURVIVAL);
        assert_eq!(data.get_byte("initialized").unwrap(), 1);
        assert_eq!(data.get_long("LastPlayed").unwrap(), 42);
        assert_eq!(
            data.get_compound("spawn")
                .unwrap()
                .get_string("dimension")
                .unwrap(),
            "minecraft:overworld"
        );
        assert_eq!(
            data.get_compound("Version").unwrap().get_int("Id").unwrap(),
            DATA_VERSION
        );
        let world_gen = data.get_compound("WorldGenSettings").unwrap();
        assert_eq!(world_gen.get_long("seed").unwrap(), 123456789);
        assert!(world_gen
            .get_compound("dimensions")
            .unwrap()
            .contains("minecraft:overworld"));
    }

    #[test]
    fn gzip_round_trip_matches_java_test_contract() {
        let settings = default_settings("SR EarthMap Test", 123456789).unwrap();
        let path = std::env::temp_dir().join(format!(
            "earthmap-rust-leveldat-{}-level.dat",
            std::process::id()
        ));

        write(&path, &settings).unwrap();
        let read = nbt::read_gzip(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(read.name(), "");
        let Tag::Compound(root) = read.tag() else {
            panic!("level.dat root must be compound");
        };
        assert_eq!(
            root.get_compound("Data")
                .unwrap()
                .get_string("LevelName")
                .unwrap(),
            "SR EarthMap Test"
        );
    }
}
