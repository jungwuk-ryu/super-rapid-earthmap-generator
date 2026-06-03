use crate::{MinecraftError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkGenerationStatus {
    Full,
    Surface,
    Carvers,
}

impl ChunkGenerationStatus {
    pub fn id(self) -> &'static str {
        match self {
            Self::Full => "minecraft:full",
            Self::Surface => "minecraft:surface",
            Self::Carvers => "minecraft:carvers",
        }
    }

    pub fn light_on(self) -> bool {
        self == Self::Full
    }

    pub fn delegates_to_server(self) -> bool {
        self != Self::Full
    }

    pub fn parse(text: &str) -> Result<Self> {
        if text.trim().is_empty() {
            return Err(MinecraftError::invalid("chunk status must not be blank"));
        }
        let mut normalized = text.to_lowercase().replace('-', "_");
        if let Some(stripped) = normalized.strip_prefix("minecraft:") {
            normalized = stripped.to_string();
        }
        match normalized.as_str() {
            "full" => Ok(Self::Full),
            "surface" => Ok(Self::Surface),
            "carvers" | "liquid_carvers" => Ok(Self::Carvers),
            _ => Err(MinecraftError::invalid(
                "chunk status must be full, surface, or carvers",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_delegation_match_java() {
        assert_eq!(ChunkGenerationStatus::Full.id(), "minecraft:full");
        assert_eq!(ChunkGenerationStatus::Surface.id(), "minecraft:surface");
        assert_eq!(ChunkGenerationStatus::Carvers.id(), "minecraft:carvers");
        assert!(ChunkGenerationStatus::Full.light_on());
        assert!(!ChunkGenerationStatus::Surface.light_on());
        assert!(!ChunkGenerationStatus::Full.delegates_to_server());
        assert!(ChunkGenerationStatus::Surface.delegates_to_server());
        assert!(ChunkGenerationStatus::Carvers.delegates_to_server());
    }

    #[test]
    fn parse_matches_java_normalization() {
        assert_eq!(
            ChunkGenerationStatus::parse("full").unwrap(),
            ChunkGenerationStatus::Full
        );
        assert_eq!(
            ChunkGenerationStatus::parse("minecraft:surface").unwrap(),
            ChunkGenerationStatus::Surface
        );
        assert_eq!(
            ChunkGenerationStatus::parse("liquid-carvers").unwrap(),
            ChunkGenerationStatus::Carvers
        );
        assert_eq!(
            ChunkGenerationStatus::parse("minecraft:liquid-carvers").unwrap(),
            ChunkGenerationStatus::Carvers
        );
        assert!(ChunkGenerationStatus::parse("").is_err());
        assert!(ChunkGenerationStatus::parse("noise").is_err());
    }
}
