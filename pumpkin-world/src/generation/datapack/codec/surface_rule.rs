//! Runtime codec for a datapack `worldgen/noise_settings` **`surface_rule`**
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use pumpkin_data::chunk::Biome;
use pumpkin_data::chunk_gen_settings::{
    AboveYMaterialCondition, BadLandsMaterialRule, BiomeMaterialCondition, BlockMaterialRule,
    ConditionMaterialRule, HoleMaterialCondition, MaterialCondition, MaterialRule,
    NoiseThresholdMaterialCondition, NotMaterialCondition, SequenceMaterialRule,
    StoneDepthMaterialCondition, SurfaceMaterialCondition, VerticalGradientMaterialCondition,
    WaterMaterialCondition,
};
use pumpkin_util::math::vertical_surface_type::VerticalSurfaceType;
use pumpkin_util::random::xoroshiro128::md5_to_lo_hi;
use pumpkin_util::y_offset::{AboveBottom, Absolute, BelowTop, YOffset};

use crate::block::BlockStateCodec;
use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;
use crate::generation::datapack::flatten::{
    SURFACE_NOISE_ID_BASE, WorldgenLookup, resolve_named_noise,
};

/// A parsed `surface_rule` node (`MaterialRules.MaterialRule`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum SurfaceRule {
    #[serde(rename = "minecraft:block")]
    Block { result_state: Value },
    #[serde(rename = "minecraft:sequence")]
    Sequence { sequence: Vec<SurfaceRule> },
    #[serde(rename = "minecraft:condition")]
    Condition {
        if_true: SurfaceCondition,
        then_run: Box<SurfaceRule>,
    },
    #[serde(rename = "minecraft:bandlands")]
    Badlands,
}

/// A parsed surface `MaterialCondition`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum SurfaceCondition {
    #[serde(rename = "minecraft:biome")]
    Biome {
        #[serde(deserialize_with = "string_or_vec")]
        biome_is: Vec<String>,
    },
    #[serde(rename = "minecraft:noise_threshold")]
    NoiseThreshold {
        noise: String,
        min_threshold: f64,
        max_threshold: f64,
    },
    #[serde(rename = "minecraft:vertical_gradient")]
    VerticalGradient {
        random_name: String,
        true_at_and_below: YOffsetJson,
        false_at_and_above: YOffsetJson,
    },
    #[serde(rename = "minecraft:y_above")]
    YAbove {
        anchor: YOffsetJson,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    #[serde(rename = "minecraft:water")]
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    #[serde(rename = "minecraft:temperature")]
    Temperature,
    #[serde(rename = "minecraft:steep")]
    Steep,
    #[serde(rename = "minecraft:not")]
    Not { invert: Box<SurfaceCondition> },
    #[serde(rename = "minecraft:hole")]
    Hole,
    #[serde(rename = "minecraft:above_preliminary_surface")]
    AbovePreliminarySurface,
    #[serde(rename = "minecraft:stone_depth")]
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_depth_range: i32,
        surface_type: String,
    },
}

/// A `YOffset` (`{"absolute":..}`, `{"above_bottom":..}`, or `{"below_top":..}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum YOffsetJson {
    Absolute { absolute: i16 },
    AboveBottom { above_bottom: i8 },
    BelowTop { below_top: i8 },
}

impl YOffsetJson {
    fn build(&self) -> YOffset {
        match *self {
            Self::Absolute { absolute } => YOffset::Absolute(Absolute { absolute }),
            Self::AboveBottom { above_bottom } => YOffset::AboveBottom(AboveBottom { above_bottom }),
            Self::BelowTop { below_top } => YOffset::BelowTop(BelowTop { below_top }),
        }
    }
}

/// `biome_is` is a string or a list of strings (vanilla `Codecs.listOrSingle`).
fn string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("string or list of strings")
        }
        fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
            Ok(vec![s.to_owned()])
        }
        fn visit_seq<S: serde::de::SeqAccess<'de>>(
            self,
            mut seq: S,
        ) -> Result<Self::Value, S::Error> {
            let mut v = Vec::new();
            while let Some(el) = seq.next_element()? {
                v.push(el);
            }
            Ok(v)
        }
    }
    deserializer.deserialize_any(V)
}

/// Resolve a biome resource id to its runtime id: vanilla via `Biome::from_name`,
fn resolve_biome_id(name: &str) -> Option<u8> {
    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(biome) = Biome::from_name(stripped) {
        return Some(biome.id);
    }
    let registry = DYNAMIC_BIOMES.read().unwrap();
    registry.lookup(name).or_else(|| registry.lookup(stripped))
}

/// Threads the noise `lookup` and a fresh-noise-id counter through the recursive build
struct Builder<'a> {
    lookup: &'a dyn WorldgenLookup,
    next_noise_id: usize,
}

impl Builder<'_> {
    fn rule(&mut self, rule: &SurfaceRule) -> Option<MaterialRule> {
        match rule {
            SurfaceRule::Block { result_state } => {
                let state = serde_json::from_value::<BlockStateCodec>(result_state.clone())
                    .ok()?
                    .get_state();
                Some(MaterialRule::Block(BlockMaterialRule {
                    result_state: state,
                }))
            }
            SurfaceRule::Sequence { sequence } => {
                let rules: Vec<MaterialRule> =
                    sequence.iter().map(|r| self.rule(r)).collect::<Option<_>>()?;
                let leaked: &'static [MaterialRule] = Box::leak(rules.into_boxed_slice());
                Some(MaterialRule::Sequence(SequenceMaterialRule { sequence: leaked }))
            }
            SurfaceRule::Condition { if_true, then_run } => {
                let if_true = self.condition(if_true)?;
                let then = self.rule(then_run)?;
                let then_run: &'static MaterialRule = Box::leak(Box::new(then));
                Some(MaterialRule::Condition(ConditionMaterialRule {
                    if_true,
                    then_run,
                }))
            }
            SurfaceRule::Badlands => Some(MaterialRule::Badlands(BadLandsMaterialRule)),
        }
    }

    fn condition(&mut self, cond: &SurfaceCondition) -> Option<MaterialCondition> {
        match cond {
            SurfaceCondition::Biome { biome_is } => {
                let ids: Vec<u8> = biome_is.iter().filter_map(|n| resolve_biome_id(n)).collect();
                let biome_is: &'static [u8] = Box::leak(ids.into_boxed_slice());
                Some(MaterialCondition::Biome(BiomeMaterialCondition { biome_is }))
            }
            SurfaceCondition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
            } => {
                let params = resolve_named_noise(self.lookup, noise, &mut self.next_noise_id)
                    .or_else(|| {
                        warn!("surface_rule noise_threshold references unresolved noise `{noise}`");
                        None
                    })?;
                Some(MaterialCondition::NoiseThreshold(
                    NoiseThresholdMaterialCondition {
                        noise: params,
                        min_threshold: *min_threshold,
                        max_threshold: *max_threshold,
                    },
                ))
            }
            SurfaceCondition::VerticalGradient {
                random_name,
                true_at_and_below,
                false_at_and_above,
            } => {
                let (random_lo, random_hi) = md5_to_lo_hi(random_name);
                Some(MaterialCondition::VerticalGradient(
                    VerticalGradientMaterialCondition {
                        random_lo,
                        random_hi,
                        true_at_and_below: true_at_and_below.build(),
                        false_at_and_above: false_at_and_above.build(),
                    },
                ))
            }
            SurfaceCondition::YAbove {
                anchor,
                surface_depth_multiplier,
                add_stone_depth,
            } => Some(MaterialCondition::YAbove(AboveYMaterialCondition {
                anchor: anchor.build(),
                surface_depth_multiplier: *surface_depth_multiplier,
                add_stone_depth: *add_stone_depth,
            })),
            SurfaceCondition::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            } => Some(MaterialCondition::Water(WaterMaterialCondition {
                offset: *offset,
                surface_depth_multiplier: *surface_depth_multiplier,
                add_stone_depth: *add_stone_depth,
            })),
            SurfaceCondition::Temperature => Some(MaterialCondition::Temperature),
            SurfaceCondition::Steep => Some(MaterialCondition::Steep),
            SurfaceCondition::Not { invert } => {
                let inner = self.condition(invert)?;
                let invert: &'static MaterialCondition = Box::leak(Box::new(inner));
                Some(MaterialCondition::Not(NotMaterialCondition { invert }))
            }
            SurfaceCondition::Hole => Some(MaterialCondition::Hole(HoleMaterialCondition)),
            SurfaceCondition::AbovePreliminarySurface => Some(
                MaterialCondition::AbovePreliminarySurface(SurfaceMaterialCondition),
            ),
            SurfaceCondition::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                surface_type,
            } => {
                let surface_type = match surface_type.as_str() {
                    "ceiling" => VerticalSurfaceType::Ceiling,
                    "floor" => VerticalSurfaceType::Floor,
                    other => {
                        warn!("surface_rule stone_depth has unknown surface_type `{other}`");
                        return None;
                    }
                };
                Some(MaterialCondition::StoneDepth(StoneDepthMaterialCondition {
                    offset: *offset,
                    add_surface_depth: *add_surface_depth,
                    secondary_depth_range: *secondary_depth_range,
                    surface_type,
                }))
            }
        }
    }
}

/// Parse a datapack `surface_rule` JSON value into a runtime [`MaterialRule`]
#[must_use]
pub fn build_surface_rule(json: &Value, lookup: &dyn WorldgenLookup) -> Option<MaterialRule> {
    let parsed: SurfaceRule = match serde_json::from_value(json.clone()) {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!("datapack surface_rule failed to parse: {e}");
            return None;
        }
    };
    let mut builder = Builder {
        lookup,
        next_noise_id: SURFACE_NOISE_ID_BASE,
    };
    builder.rule(&parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_block_condition_rule() {
        // block, sequence, condition + vanilla biome / vertical_gradient / y_above
        let json = serde_json::json!({
            "type": "minecraft:sequence",
            "sequence": [
                {
                    "type": "minecraft:condition",
                    "if_true": { "type": "minecraft:biome", "biome_is": ["minecraft:plains", "minecraft:desert"] },
                    "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:grass_block" } }
                },
                {
                    "type": "minecraft:condition",
                    "if_true": {
                        "type": "minecraft:vertical_gradient",
                        "random_name": "minecraft:bedrock_floor",
                        "true_at_and_below": { "above_bottom": 0 },
                        "false_at_and_above": { "above_bottom": 5 }
                    },
                    "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:bedrock" } }
                }
            ]
        });

        struct NoLookup;
        impl WorldgenLookup for NoLookup {
            fn density_function(
                &self,
                _id: &str,
            ) -> Option<&crate::generation::datapack::codec::density_function::DensityFunction>
            {
                None
            }
            fn noise(
                &self,
                _id: &str,
            ) -> Option<&crate::generation::datapack::codec::density_function::NoiseParameters>
            {
                None
            }
        }

        let rule = build_surface_rule(&json, &NoLookup).expect("should build");
        let MaterialRule::Sequence(seq) = rule else {
            panic!("expected sequence")
        };
        assert_eq!(seq.sequence.len(), 2);
        let MaterialRule::Condition(cond) = &seq.sequence[0] else {
            panic!("expected condition")
        };
        let MaterialCondition::Biome(biome) = &cond.if_true else {
            panic!("expected biome condition")
        };
        assert_eq!(biome.biome_is.len(), 2);
        assert!(biome.biome_is.contains(&Biome::PLAINS.id));
        assert!(biome.biome_is.contains(&Biome::DESERT.id));
    }
}
