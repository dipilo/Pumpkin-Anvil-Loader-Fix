//! Serde model of Minecraft's `worldgen/density_function` JSON

use serde::Deserialize;
use serde_json::Value;

/// A noise-parameters reference: either a resource location (`"minecraft:..."`) or
/// an inline `{ "firstOctave": .., "amplitudes": [..] }` definition
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NoiseRef {
    Reference(String),
    Inline(NoiseParameters),
}

/// Inline `DoublePerlinNoiseSampler.NoiseParameters` (`worldgen/noise` entries)
#[derive(Debug, Clone, Deserialize)]
pub struct NoiseParameters {
    #[serde(rename = "firstOctave")]
    pub first_octave: i32,
    pub amplitudes: Vec<f64>,
}

/// A node of the density-function DAG
#[derive(Debug, Clone)]
pub enum DensityFunction {
    /// A bare numeric constant
    Constant(f64),
    /// A reference to another density function by resource location
    Reference(String),
    /// An inline, typed node
    Inline(Box<InlineFunction>),
    /// A recognized object whose `type` we do not model yet. Retained so parsing
    /// never fails; `kind` is the raw `type` string for coverage reporting
    Unknown { kind: String },
}

/// A typed density-function node (the object forms)
#[derive(Debug, Clone)]
pub enum InlineFunction {
    // Caching wrappers (transparent to the result, load-bearing for performance)
    Interpolated(DensityFunction),
    FlatCache(DensityFunction),
    Cache2d(DensityFunction),
    CacheOnce(DensityFunction),
    CacheAllInCell(DensityFunction),
    BlendDensity(DensityFunction),
    // Unary operators
    Abs(DensityFunction),
    Square(DensityFunction),
    Cube(DensityFunction),
    HalfNegative(DensityFunction),
    QuarterNegative(DensityFunction),
    Squeeze(DensityFunction),
    // Binary operators
    Add(DensityFunction, DensityFunction),
    Mul(DensityFunction, DensityFunction),
    Min(DensityFunction, DensityFunction),
    Max(DensityFunction, DensityFunction),
    // Noise samplers
    Noise {
        noise: NoiseRef,
        xz_scale: f64,
        y_scale: f64,
    },
    ShiftedNoise {
        noise: NoiseRef,
        xz_scale: f64,
        y_scale: f64,
        shift_x: DensityFunction,
        shift_y: DensityFunction,
        shift_z: DensityFunction,
    },
    ShiftA(NoiseRef),
    ShiftB(NoiseRef),
    Shift(NoiseRef),
    WeirdScaledSampler {
        rarity_value_mapper: String,
        noise: NoiseRef,
        input: DensityFunction,
    },
    // Misc
    Clamp {
        input: DensityFunction,
        min: f64,
        max: f64,
    },
    RangeChoice {
        input: DensityFunction,
        min_inclusive: f64,
        max_exclusive: f64,
        when_in_range: DensityFunction,
        when_out_of_range: DensityFunction,
    },
    YClampedGradient {
        from_y: f64,
        to_y: f64,
        from_value: f64,
        to_value: f64,
    },
    Constant(f64),
    Spline(SplineRepr),
    // Context/marker nodes with no arguments
    EndIslands,
    BlendAlpha,
    BlendOffset,
    Beardifier,
}

/// A cubic spline over one input density function
#[derive(Debug, Clone, Deserialize)]
pub struct SplineRepr {
    pub coordinate: DensityFunction,
    pub points: Vec<SplinePoint>,
}

/// One control point of a [`SplineRepr`]
#[derive(Debug, Clone, Deserialize)]
pub struct SplinePoint {
    pub location: f32,
    pub value: SplineValue,
    pub derivative: f32,
}

/// A spline point value: either a constant or a nested spline
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SplineValue {
    Constant(f32),
    Spline(Box<SplineRepr>),
}

impl<'de> Deserialize<'de> for DensityFunction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

impl DensityFunction {
    /// Parse from an already-decoded JSON value
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => n
                .as_f64()
                .map(DensityFunction::Constant)
                .ok_or_else(|| "non-finite density-function constant".to_string()),
            Value::String(s) => Ok(Self::Reference(s)),
            Value::Object(_) => Self::from_object(value),
            other => Err(format!("invalid density function JSON: {other}")),
        }
    }

    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    fn from_object(value: Value) -> Result<Self, String> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "density-function object missing `type`".to_string())?
            .to_string();

        // Helpers that pull typed argument shapes out of the object
        // `type` is an extra field the argument structs ignore
        let field = |name: &str| -> Result<Self, String> {
            value
                .get(name)
                .cloned()
                .ok_or_else(|| format!("`{kind}` missing `{name}`"))
                .and_then(Self::from_value)
        };
        let noise = |name: &str| -> Result<NoiseRef, String> {
            value
                .get(name)
                .cloned()
                .ok_or_else(|| format!("`{kind}` missing `{name}`"))
                .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        };
        let num = |name: &str| -> Result<f64, String> {
            value
                .get(name)
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("`{kind}` missing numeric `{name}`"))
        };
        let string = |name: &str| -> Result<String, String> {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("`{kind}` missing string `{name}`"))
        };

        let inline = match kind.as_str() {
            "minecraft:interpolated" => InlineFunction::Interpolated(field("argument")?),
            "minecraft:flat_cache" => InlineFunction::FlatCache(field("argument")?),
            "minecraft:cache_2d" => InlineFunction::Cache2d(field("argument")?),
            "minecraft:cache_once" => InlineFunction::CacheOnce(field("argument")?),
            "minecraft:cache_all_in_cell" => InlineFunction::CacheAllInCell(field("argument")?),
            "minecraft:blend_density" => InlineFunction::BlendDensity(field("argument")?),
            "minecraft:abs" => InlineFunction::Abs(field("argument")?),
            "minecraft:square" => InlineFunction::Square(field("argument")?),
            "minecraft:cube" => InlineFunction::Cube(field("argument")?),
            "minecraft:half_negative" => InlineFunction::HalfNegative(field("argument")?),
            "minecraft:quarter_negative" => InlineFunction::QuarterNegative(field("argument")?),
            "minecraft:squeeze" => InlineFunction::Squeeze(field("argument")?),
            "minecraft:add" => InlineFunction::Add(field("argument1")?, field("argument2")?),
            "minecraft:mul" => InlineFunction::Mul(field("argument1")?, field("argument2")?),
            "minecraft:min" => InlineFunction::Min(field("argument1")?, field("argument2")?),
            "minecraft:max" => InlineFunction::Max(field("argument1")?, field("argument2")?),
            "minecraft:noise" => InlineFunction::Noise {
                noise: noise("noise")?,
                xz_scale: num("xz_scale")?,
                y_scale: num("y_scale")?,
            },
            "minecraft:shifted_noise" => InlineFunction::ShiftedNoise {
                noise: noise("noise")?,
                xz_scale: num("xz_scale")?,
                y_scale: num("y_scale")?,
                shift_x: field("shift_x")?,
                shift_y: field("shift_y")?,
                shift_z: field("shift_z")?,
            },
            "minecraft:shift_a" => InlineFunction::ShiftA(noise("argument")?),
            "minecraft:shift_b" => InlineFunction::ShiftB(noise("argument")?),
            "minecraft:shift" => InlineFunction::Shift(noise("argument")?),
            "minecraft:weird_scaled_sampler" => InlineFunction::WeirdScaledSampler {
                rarity_value_mapper: string("rarity_value_mapper")?,
                noise: noise("noise")?,
                input: field("input")?,
            },
            "minecraft:clamp" => InlineFunction::Clamp {
                input: field("input")?,
                min: num("min")?,
                max: num("max")?,
            },
            "minecraft:range_choice" => InlineFunction::RangeChoice {
                input: field("input")?,
                min_inclusive: num("min_inclusive")?,
                max_exclusive: num("max_exclusive")?,
                when_in_range: field("when_in_range")?,
                when_out_of_range: field("when_out_of_range")?,
            },
            "minecraft:y_clamped_gradient" => InlineFunction::YClampedGradient {
                from_y: num("from_y")?,
                to_y: num("to_y")?,
                from_value: num("from_value")?,
                to_value: num("to_value")?,
            },
            "minecraft:constant" => InlineFunction::Constant(num("argument")?),
            "minecraft:spline" => {
                let spline = value
                    .get("spline")
                    .cloned()
                    .ok_or_else(|| "`minecraft:spline` missing `spline`".to_string())?;
                InlineFunction::Spline(
                    serde_json::from_value(spline).map_err(|e| e.to_string())?,
                )
            }
            "minecraft:end_islands" => InlineFunction::EndIslands,
            "minecraft:blend_alpha" => InlineFunction::BlendAlpha,
            "minecraft:blend_offset" => InlineFunction::BlendOffset,
            "minecraft:beardifier" => InlineFunction::Beardifier,
            // `old_blended_noise` and any future/unknown type: keep raw, don't fail
            _ => return Ok(Self::Unknown { kind }),
        };
        Ok(Self::Inline(Box::new(inline)))
    }

    /// The `type` string if this is an unmodeled node, for coverage reporting
    #[must_use]
    pub fn unknown_kind(&self) -> Option<&str> {
        match self {
            Self::Unknown { kind } => Some(kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> DensityFunction {
        serde_json::from_str(json).expect("should parse")
    }

    #[test]
    fn parses_constant_and_reference() {
        assert!(matches!(parse("0.5"), DensityFunction::Constant(v) if (v - 0.5).abs() < 1e-9));
        assert!(matches!(parse("\"terralith:overworld/arch/total\""), DensityFunction::Reference(s) if s == "terralith:overworld/arch/total"));
    }

    #[test]
    fn parses_nested_terralith_shaped_object() {
        // The exact shape of terralith overworld/extra_terrain_base.json
        let df = parse(
            r#"{
                "type": "minecraft:cache_once",
                "argument": {
                    "type": "minecraft:max",
                    "argument1": "terralith:overworld/arch/total",
                    "argument2": {
                        "type": "minecraft:max",
                        "argument1": "terralith:overworld/dune/total",
                        "argument2": "terralith:overworld/spikes/tendrils"
                    }
                }
            }"#,
        );
        let DensityFunction::Inline(inner) = df else {
            panic!("expected inline");
        };
        assert!(matches!(*inner, InlineFunction::CacheOnce(_)));
    }

    #[test]
    fn parses_noise_and_spline() {
        let df = parse(
            r#"{ "type": "minecraft:noise", "noise": "minecraft:cave_entrance", "xz_scale": 0.75, "y_scale": 0.5 }"#,
        );
        assert!(matches!(df, DensityFunction::Inline(b) if matches!(*b, InlineFunction::Noise { .. })));

        let spline = parse(
            r#"{
                "type": "minecraft:spline",
                "spline": {
                    "coordinate": "minecraft:overworld/continents",
                    "points": [
                        { "location": -1.0, "value": 0.0, "derivative": 0.0 },
                        { "location": 1.0, "value": { "coordinate": "minecraft:zero", "points": [] }, "derivative": 0.0 }
                    ]
                }
            }"#,
        );
        assert!(matches!(spline, DensityFunction::Inline(b) if matches!(*b, InlineFunction::Spline(_))));
    }

    #[test]
    fn unknown_type_is_retained_not_failed() {
        let df = parse(r#"{ "type": "minecraft:old_blended_noise", "xz_scale": 1.0 }"#);
        assert_eq!(df.unknown_kind(), Some("minecraft:old_blended_noise"));
    }
}
