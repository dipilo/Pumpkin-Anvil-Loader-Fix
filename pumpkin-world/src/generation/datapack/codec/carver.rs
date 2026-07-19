//! Runtime codec for `worldgen/configured_carver/*.json`

use serde_json::Value;

use pumpkin_data::carver::{
    CanyonCarverConfig, CanyonShapeConfig, CarverAdditionalConfig, CarverConfig, CaveCarverConfig,
    HeightProvider, TrapezoidHeightProvider, UniformHeightProvider, VeryBiasedToBottomHeightProvider,
};
use pumpkin_util::math::float_provider::FloatProvider;

use super::feature::{f32_of, i32_of, norm_id, parse_y_offset, resolve_block_tag};

/// Parse a `FloatProvider`
fn parse_float_provider(v: &Value, default: f32) -> FloatProvider {
    if v.is_null() {
        return FloatProvider::Constant(default);
    }
    serde_json::from_value::<FloatProvider>(v.clone()).unwrap_or(FloatProvider::Constant(default))
}

/// Parse a carver `HeightProvider`
fn parse_height_provider(v: &Value) -> HeightProvider {
    let min = parse_y_offset(&v["min_inclusive"]);
    let max = parse_y_offset(&v["max_inclusive"]);
    match norm_id(v["type"].as_str().unwrap_or("uniform")).as_ref() {
        "minecraft:trapezoid" => HeightProvider::Trapezoid(TrapezoidHeightProvider {
            min_inclusive: min,
            max_inclusive: max,
            plateau: v["plateau"].as_i64().map(|x| x as i32),
        }),
        "minecraft:biased_to_bottom" | "minecraft:very_biased_to_bottom" => {
            HeightProvider::VeryBiasedToBottom(VeryBiasedToBottomHeightProvider {
                min_inclusive: min,
                max_inclusive: max,
                inner: v["inner"]
                    .as_u64()
                    .and_then(|x| u32::try_from(x).ok())
                    .and_then(std::num::NonZeroU32::new),
            })
        }
        // "minecraft:uniform" | unknown
        _ => HeightProvider::Uniform(UniformHeightProvider {
            min_inclusive: min,
            max_inclusive: max,
        }),
    }
}

fn parse_cave_config(config: &Value) -> CaveCarverConfig {
    CaveCarverConfig {
        horizontal_radius_multiplier: parse_float_provider(
            &config["horizontal_radius_multiplier"],
            1.0,
        ),
        vertical_radius_multiplier: parse_float_provider(
            &config["vertical_radius_multiplier"],
            1.0,
        ),
        floor_level: parse_float_provider(&config["floor_level"], -0.7),
    }
}

fn parse_canyon_config(config: &Value) -> CanyonCarverConfig {
    let shape = &config["shape"];
    CanyonCarverConfig {
        vertical_rotation: parse_float_provider(&config["vertical_rotation"], 0.0),
        shape: CanyonShapeConfig {
            distance_factor: parse_float_provider(&shape["distance_factor"], 1.0),
            thickness: parse_float_provider(&shape["thickness"], 0.0),
            width_smoothness: i32_of(shape, "width_smoothness", 0),
            horizontal_radius_factor: parse_float_provider(&shape["horizontal_radius_factor"], 1.0),
            vertical_radius_default_factor: f32_of(shape, "vertical_radius_default_factor", 0.0),
            vertical_radius_center_factor: f32_of(shape, "vertical_radius_center_factor", 0.0),
        },
    }
}

/// Parse configured carver JSON document into runtime [`CarverConfig`]
#[must_use]
pub fn parse_configured_carver(v: &Value) -> Option<CarverConfig> {
    let type_str = v["type"].as_str()?;
    let config = &v["config"];

    let additional = match norm_id(type_str).as_ref() {
        "minecraft:cave" => CarverAdditionalConfig::Cave(parse_cave_config(config)),
        "minecraft:nether_cave" => CarverAdditionalConfig::NetherCave(parse_cave_config(config)),
        "minecraft:canyon" => CarverAdditionalConfig::Canyon(parse_canyon_config(config)),
        _ => return None,
    };

    Some(CarverConfig {
        probability: f32_of(config, "probability", 0.0),
        y: parse_height_provider(&config["y"]),
        y_scale: parse_float_provider(&config["yScale"], 1.0),
        lava_level: parse_y_offset(&config["lava_level"]),
        replaceable: resolve_block_tag(config["replaceable"].as_str().unwrap_or("")),
        additional,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vanilla_cave_carver() {
        let v = serde_json::json!({
            "type": "minecraft:cave",
            "config": {
                "probability": 0.15,
                "y": {
                    "type": "minecraft:uniform",
                    "min_inclusive": { "above_bottom": 8 },
                    "max_inclusive": { "absolute": 180 }
                },
                "yScale": { "type": "minecraft:uniform", "min_inclusive": 0.1, "max_exclusive": 0.9 },
                "lava_level": { "above_bottom": 8 },
                "replaceable": "#minecraft:overworld_carver_replaceables",
                "horizontal_radius_multiplier": { "type": "minecraft:uniform", "min_inclusive": 0.7, "max_exclusive": 1.4 },
                "vertical_radius_multiplier": { "type": "minecraft:uniform", "min_inclusive": 0.8, "max_exclusive": 1.3 },
                "floor_level": { "type": "minecraft:uniform", "min_inclusive": -1.0, "max_exclusive": -0.4 }
            }
        });
        let carver = parse_configured_carver(&v).expect("cave carver parses");
        assert!((carver.probability - 0.15).abs() < 1e-6);
        assert!(matches!(carver.additional, CarverAdditionalConfig::Cave(_)));
        assert!(matches!(carver.y, HeightProvider::Uniform(_)));
    }

    #[test]
    fn parses_vanilla_canyon_carver() {
        let v = serde_json::json!({
            "type": "minecraft:canyon",
            "config": {
                "probability": 0.01,
                "y": { "type": "minecraft:uniform", "min_inclusive": { "absolute": 10 }, "max_inclusive": { "absolute": 67 } },
                "yScale": 3.0,
                "lava_level": { "above_bottom": 8 },
                "replaceable": "#minecraft:overworld_carver_replaceables",
                "vertical_rotation": { "type": "minecraft:uniform", "min_inclusive": -0.125, "max_exclusive": 0.125 },
                "shape": {
                    "distance_factor": { "type": "minecraft:uniform", "min_inclusive": 0.75, "max_exclusive": 1.0 },
                    "thickness": { "type": "minecraft:trapezoid", "min": 0.0, "max": 6.0, "plateau": 2.0 },
                    "width_smoothness": 3,
                    "horizontal_radius_factor": { "type": "minecraft:uniform", "min_inclusive": 0.75, "max_exclusive": 1.0 },
                    "vertical_radius_default_factor": 1.0,
                    "vertical_radius_center_factor": 0.0
                }
            }
        });
        let carver = parse_configured_carver(&v).expect("canyon carver parses");
        let CarverAdditionalConfig::Canyon(canyon) = carver.additional else {
            panic!("expected canyon config");
        };
        assert_eq!(canyon.shape.width_smoothness, 3);
        assert!((canyon.shape.vertical_radius_default_factor - 1.0).abs() < 1e-6);
        // `yScale` given as bare number becomes constant provider
        assert!(matches!(carver.y_scale, FloatProvider::Constant(_)));
    }

    #[test]
    fn unknown_carver_type_is_none() {
        let v = serde_json::json!({ "type": "somemod:spiral", "config": {} });
        assert!(parse_configured_carver(&v).is_none());
    }
}
