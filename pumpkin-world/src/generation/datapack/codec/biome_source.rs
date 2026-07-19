//! Serde model of a dimension's multi-noise `biome_source` (`dimension/<id>.json`)
use serde::{Deserialize, Deserializer};

/// Top-level `dimension/<id>.json` (only the parts we consume)
#[derive(Debug, Clone, Deserialize)]
pub struct DimensionFile {
    pub generator: DimensionGenerator,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DimensionGenerator {
    /// Absent for non-noise generators (debug/flat); then there's no biome list
    #[serde(default)]
    pub biome_source: Option<BiomeSource>,
}

/// A dimension's biome source
#[derive(Debug, Clone, Deserialize)]
pub struct BiomeSource {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub biomes: Vec<BiomeEntry>,
    #[serde(default)]
    pub preset: Option<String>,
}

/// One `(climate hypercube -> biome)` placement entry
#[derive(Debug, Clone, Deserialize)]
pub struct BiomeEntry {
    pub parameters: ClimateParameters,
    /// Resource location of the biome (`minecraft:plains`, `terralith:...`)
    pub biome: String,
}

/// The 7 climate axes of a placement entry (vanilla `Climate.ParameterPoint`)
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ClimateParameters {
    pub temperature: Param,
    pub humidity: Param,
    pub continentalness: Param,
    pub erosion: Param,
    pub depth: Param,
    pub weirdness: Param,
    pub offset: Param,
}

impl ClimateParameters {
    /// The 7 axes in the canonical order used by `NoiseValuePoint::convert_to_list`
    #[must_use]
    pub const fn axes(&self) -> [Param; 7] {
        [
            self.temperature,
            self.humidity,
            self.continentalness,
            self.erosion,
            self.depth,
            self.weirdness,
            self.offset,
        ]
    }
}

/// A climate parameter: a point value `x` or an inclusive range `[min, max]`
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub min: f64,
    pub max: f64,
}

impl Param {
    /// Quantize to the `i64` space the biome search uses (`value * 10000`)
    #[must_use]
    pub fn quantized(self) -> (i64, i64) {
        (
            (self.min * 10000.0) as i64,
            (self.max * 10000.0) as i64,
        )
    }
}

impl<'de> Deserialize<'de> for Param {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Point(f64),
            Range([f64; 2]),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Point(v) => Self { min: v, max: v },
            Raw::Range([min, max]) => Self { min, max },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terralith_shaped_biome_source() {
        // The shape of Terralith's dimension/overworld.json biome_source entries
        let json = r#"{
            "type": "minecraft:noise",
            "generator": {
                "type": "minecraft:noise",
                "settings": "minecraft:overworld",
                "biome_source": {
                    "type": "minecraft:multi_noise",
                    "biomes": [
                        {
                            "parameters": {
                                "temperature": [-0.45, -0.155],
                                "humidity": [-1, 1],
                                "continentalness": [-1.2, -0.455],
                                "erosion": [-1, -0.78],
                                "depth": 0,
                                "weirdness": [-1, 1],
                                "offset": 0
                            },
                            "biome": "terralith:mirage_isles"
                        }
                    ]
                }
            }
        }"#;
        let file: DimensionFile = serde_json::from_str(json).expect("parse dimension");
        let source = file.generator.biome_source.expect("has biome source");
        assert_eq!(source.kind, "minecraft:multi_noise");
        assert_eq!(source.biomes.len(), 1);
        let entry = &source.biomes[0];
        assert_eq!(entry.biome, "terralith:mirage_isles");
        // point value -> min==max
        assert_eq!(entry.parameters.depth.min, 0.0);
        assert_eq!(entry.parameters.depth.max, 0.0);
        // range -> [min,max]
        assert_eq!(entry.parameters.temperature.min, -0.45);
        assert_eq!(entry.parameters.temperature.max, -0.155);
        // quantization matches to_long (x * 10000)
        assert_eq!(entry.parameters.temperature.quantized(), (-4500, -1550));
        assert_eq!(entry.parameters.axes().len(), 7);
    }

    #[test]
    fn preset_only_source_has_no_biomes() {
        let json = r#"{
            "generator": {
                "biome_source": { "type": "minecraft:multi_noise", "preset": "minecraft:overworld" }
            }
        }"#;
        let file: DimensionFile = serde_json::from_str(json).expect("parse");
        let source = file.generator.biome_source.unwrap();
        assert!(source.biomes.is_empty());
        assert_eq!(source.preset.as_deref(), Some("minecraft:overworld"));
    }
}
