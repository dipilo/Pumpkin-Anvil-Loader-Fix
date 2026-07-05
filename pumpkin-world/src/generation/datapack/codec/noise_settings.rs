//! Serde model of Minecraft's `worldgen/noise_settings` JSON (`ChunkGeneratorSettings`)
use serde::Deserialize;
use serde_json::Value;

use super::density_function::DensityFunction;

/// Top-level `worldgen/noise_settings/<id>.json`
#[derive(Debug, Clone, Deserialize)]
pub struct NoiseSettings {
    pub noise: GenerationShapeConfig,
    /// `{ "Name": "minecraft:stone", "Properties": {..} }` — decoded later
    pub default_block: Value,
    pub default_fluid: Value,
    pub sea_level: i32,
    #[serde(default)]
    pub legacy_random_source: bool,
    #[serde(default)]
    pub aquifers_enabled: bool,
    #[serde(default)]
    pub ore_veins_enabled: bool,
    #[serde(default)]
    pub disable_mob_generation: bool,
    pub noise_router: NoiseRouter,
    /// `MaterialRule` — decoded in the surface-rules phase
    pub surface_rule: Value,
    /// `List<NoiseHypercube>` — decoded when spawn positioning is wired up
    #[serde(default)]
    pub spawn_target: Value,
}

/// The vertical/horizontal cell shape of the noise grid (`GenerationShapeConfig`)
#[derive(Debug, Clone, Deserialize)]
pub struct GenerationShapeConfig {
    pub min_y: i32,
    pub height: i32,
    pub size_horizontal: i32,
    pub size_vertical: i32,
}

/// The 16 named entry points of the noise DAG (`NoiseRouter`)
#[derive(Debug, Clone, Deserialize)]
pub struct NoiseRouter {
    pub barrier: DensityFunction,
    pub fluid_level_floodedness: DensityFunction,
    pub fluid_level_spread: DensityFunction,
    pub lava: DensityFunction,
    pub temperature: DensityFunction,
    pub vegetation: DensityFunction,
    pub continents: DensityFunction,
    pub erosion: DensityFunction,
    pub depth: DensityFunction,
    pub ridges: DensityFunction,
    pub preliminary_surface_level: DensityFunction,
    pub final_density: DensityFunction,
    pub vein_toggle: DensityFunction,
    pub vein_ridged: DensityFunction,
    pub vein_gap: DensityFunction,
}

impl NoiseRouter {
    /// Every named router entry, for uniform traversal (coverage, flattening)
    #[must_use]
    pub const fn entries(&self) -> [(&'static str, &DensityFunction); 15] {
        [
            ("barrier", &self.barrier),
            ("fluid_level_floodedness", &self.fluid_level_floodedness),
            ("fluid_level_spread", &self.fluid_level_spread),
            ("lava", &self.lava),
            ("temperature", &self.temperature),
            ("vegetation", &self.vegetation),
            ("continents", &self.continents),
            ("erosion", &self.erosion),
            ("depth", &self.depth),
            ("ridges", &self.ridges),
            ("preliminary_surface_level", &self.preliminary_surface_level),
            ("final_density", &self.final_density),
            ("vein_toggle", &self.vein_toggle),
            ("vein_ridged", &self.vein_ridged),
            ("vein_gap", &self.vein_gap),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_noise_settings() {
        // A minimal but structurally faithful noise_settings document
        let json = r#"{
            "noise": { "min_y": -64, "height": 384, "size_horizontal": 1, "size_vertical": 2 },
            "default_block": { "Name": "minecraft:stone" },
            "default_fluid": { "Name": "minecraft:water", "Properties": { "level": "0" } },
            "sea_level": 63,
            "legacy_random_source": false,
            "aquifers_enabled": true,
            "ore_veins_enabled": true,
            "disable_mob_generation": false,
            "noise_router": {
                "barrier": 0.0,
                "fluid_level_floodedness": 0.0,
                "fluid_level_spread": 0.0,
                "lava": 0.0,
                "temperature": "minecraft:overworld/temperature",
                "vegetation": "minecraft:overworld/vegetation",
                "continents": "minecraft:overworld/continents",
                "erosion": "minecraft:overworld/erosion",
                "depth": 0.0,
                "ridges": "minecraft:overworld/ridges",
                "preliminary_surface_level": 0.0,
                "final_density": { "type": "minecraft:add", "argument1": 0.0, "argument2": 0.0 },
                "vein_toggle": 0.0,
                "vein_ridged": 0.0,
                "vein_gap": 0.0
            },
            "surface_rule": { "type": "minecraft:block", "result_state": { "Name": "minecraft:stone" } },
            "spawn_target": []
        }"#;

        let settings: NoiseSettings = serde_json::from_str(json).expect("should parse");
        assert_eq!(settings.noise.min_y, -64);
        assert_eq!(settings.sea_level, 63);
        assert!(settings.aquifers_enabled);
        assert_eq!(settings.noise_router.entries().len(), 15);
        assert!(matches!(
            settings.noise_router.continents,
            DensityFunction::Reference(_)
        ));
        assert!(matches!(
            settings.noise_router.final_density,
            DensityFunction::Inline(_)
        ));
    }
}
