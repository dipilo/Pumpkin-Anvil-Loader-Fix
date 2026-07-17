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
    Invert(DensityFunction),
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
    /// Selects one of `functions` by which interval of `thresholds` the `input` value falls into 
    IntervalSelect {
        input: DensityFunction,
        thresholds: Vec<f64>,
        functions: Vec<DensityFunction>,
    },
    YClampedGradient {
        from_y: f64,
        to_y: f64,
        from_value: f64,
        to_value: f64,
    },
    /// Scans downward from `upper_bound` for the highest cell whose `density` is positive
    FindTopSurface {
        density: DensityFunction,
        upper_bound: DensityFunction,
        lower_bound: i32,
        cell_height: i32,
    },
    Constant(f64),
    Spline(SplineRepr),
    /// The terrain "base 3d noise"
    OldBlendedNoise {
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    },
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

        let kind_key = kind.strip_prefix("minecraft:").unwrap_or(kind.as_str());
        let inline = match kind_key {
            "interpolated" => InlineFunction::Interpolated(field("argument")?),
            "flat_cache" => InlineFunction::FlatCache(field("argument")?),
            "cache_2d" => InlineFunction::Cache2d(field("argument")?),
            "cache_once" => InlineFunction::CacheOnce(field("argument")?),
            "cache_all_in_cell" => InlineFunction::CacheAllInCell(field("argument")?),
            "blend_density" => InlineFunction::BlendDensity(field("argument")?),
            "abs" => InlineFunction::Abs(field("argument")?),
            "square" => InlineFunction::Square(field("argument")?),
            "cube" => InlineFunction::Cube(field("argument")?),
            "half_negative" => InlineFunction::HalfNegative(field("argument")?),
            "quarter_negative" => InlineFunction::QuarterNegative(field("argument")?),
            "squeeze" => InlineFunction::Squeeze(field("argument")?),
            "invert" => InlineFunction::Invert(field("argument")?),
            "add" => InlineFunction::Add(field("argument1")?, field("argument2")?),
            "mul" => InlineFunction::Mul(field("argument1")?, field("argument2")?),
            "min" => InlineFunction::Min(field("argument1")?, field("argument2")?),
            "max" => InlineFunction::Max(field("argument1")?, field("argument2")?),
            "noise" => InlineFunction::Noise {
                noise: noise("noise")?,
                xz_scale: num("xz_scale")?,
                y_scale: num("y_scale")?,
            },
            "shifted_noise" => InlineFunction::ShiftedNoise {
                noise: noise("noise")?,
                xz_scale: num("xz_scale")?,
                y_scale: num("y_scale")?,
                shift_x: field("shift_x")?,
                shift_y: field("shift_y")?,
                shift_z: field("shift_z")?,
            },
            "shift_a" => InlineFunction::ShiftA(noise("argument")?),
            "shift_b" => InlineFunction::ShiftB(noise("argument")?),
            "shift" => InlineFunction::Shift(noise("argument")?),
            "weird_scaled_sampler" => InlineFunction::WeirdScaledSampler {
                rarity_value_mapper: string("rarity_value_mapper")?,
                noise: noise("noise")?,
                input: field("input")?,
            },
            "clamp" => InlineFunction::Clamp {
                input: field("input")?,
                min: num("min")?,
                max: num("max")?,
            },
            "range_choice" => InlineFunction::RangeChoice {
                input: field("input")?,
                min_inclusive: num("min_inclusive")?,
                max_exclusive: num("max_exclusive")?,
                when_in_range: field("when_in_range")?,
                when_out_of_range: field("when_out_of_range")?,
            },
            "interval_select" => {
                let thresholds = value
                    .get("thresholds")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("`{kind}` missing array `thresholds`"))?
                    .iter()
                    .map(|v| {
                        v.as_f64()
                            .ok_or_else(|| format!("`{kind}` has a non-numeric threshold"))
                    })
                    .collect::<Result<Vec<f64>, String>>()?;
                let functions = value
                    .get("functions")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("`{kind}` missing array `functions`"))?
                    .iter()
                    .cloned()
                    .map(Self::from_value)
                    .collect::<Result<Vec<DensityFunction>, String>>()?;
                InlineFunction::IntervalSelect {
                    input: field("input")?,
                    thresholds,
                    functions,
                }
            }
            "y_clamped_gradient" => InlineFunction::YClampedGradient {
                from_y: num("from_y")?,
                to_y: num("to_y")?,
                from_value: num("from_value")?,
                to_value: num("to_value")?,
            },
            "find_top_surface" => InlineFunction::FindTopSurface {
                density: field("density")?,
                upper_bound: field("upper_bound")?,
                lower_bound: num("lower_bound")? as i32,
                cell_height: num("cell_height")? as i32,
            },
            "constant" => InlineFunction::Constant(num("argument")?),
            "spline" => {
                let spline = value
                    .get("spline")
                    .cloned()
                    .ok_or_else(|| "`minecraft:spline` missing `spline`".to_string())?;
                InlineFunction::Spline(
                    serde_json::from_value(spline).map_err(|e| e.to_string())?,
                )
            }
            "old_blended_noise" => InlineFunction::OldBlendedNoise {
                xz_scale: num("xz_scale")?,
                y_scale: num("y_scale")?,
                xz_factor: num("xz_factor")?,
                y_factor: num("y_factor")?,
                smear_scale_multiplier: num("smear_scale_multiplier")?,
            },
            "end_islands" => InlineFunction::EndIslands,
            "blend_alpha" => InlineFunction::BlendAlpha,
            "blend_offset" => InlineFunction::BlendOffset,
            "beardifier" => InlineFunction::Beardifier,
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
    fn parses_interval_select() {
        // The shape Terralith 26.2 overworld/caves/entrances.json uses
        let df = parse(
            r#"{
                "type": "minecraft:interval_select",
                "input": { "type": "minecraft:noise", "noise": "minecraft:spaghetti_3d_rarity", "xz_scale": 2.0, "y_scale": 1.0 },
                "thresholds": [-0.5, 0.0, 0.5],
                "functions": [
                    { "type": "minecraft:mul", "argument1": 0.75, "argument2": "minecraft:spaghetti_3d_1" },
                    1.0,
                    "minecraft:zero",
                    { "type": "minecraft:mul", "argument1": 2.0, "argument2": "minecraft:spaghetti_3d_1" }
                ]
            }"#,
        );
        let DensityFunction::Inline(inner) = df else {
            panic!("expected inline");
        };
        let InlineFunction::IntervalSelect {
            thresholds,
            functions,
            ..
        } = *inner
        else {
            panic!("expected interval_select");
        };
        assert_eq!(thresholds, vec![-0.5, 0.0, 0.5]);
        // functions must be exactly one longer than thresholds
        assert_eq!(functions.len(), thresholds.len() + 1);
        // interval_select must NOT be reported as an unmodeled type
        assert!(
            parse(
                r#"{ "type": "minecraft:interval_select", "input": 0.0, "thresholds": [0.0], "functions": [0.0, 1.0] }"#
            )
            .unknown_kind()
            .is_none()
        );
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
        // A hypothetical future/mod node type we don't model: retained, not failed
        let df = parse(r#"{ "type": "minecraft:some_future_node", "foo": 1.0 }"#);
        assert_eq!(df.unknown_kind(), Some("minecraft:some_future_node"));
    }

    #[test]
    fn parses_find_top_surface_and_invert() {
        let df = parse(
            r#"{ "type": "minecraft:find_top_surface", "cell_height": 8, "lower_bound": -64,
                 "density": 0.0, "upper_bound": { "type": "invert", "argument": 1.0 } }"#,
        );
        let DensityFunction::Inline(inner) = df else {
            panic!("expected inline find_top_surface");
        };
        let InlineFunction::FindTopSurface {
            cell_height,
            lower_bound,
            upper_bound,
            ..
        } = *inner
        else {
            panic!("expected FindTopSurface");
        };
        assert_eq!(cell_height, 8);
        assert_eq!(lower_bound, -64);
        // The `upper_bound` used a namespace-less `invert`
        assert!(matches!(upper_bound, DensityFunction::Inline(b) if matches!(*b, InlineFunction::Invert(_))));
    }

    #[test]
    fn parses_old_blended_noise() {
        // Namespace-less `type`, as datapacks commonly write it
        let df = parse(
            r#"{ "type": "old_blended_noise", "xz_scale": 0.25, "y_scale": 0.125,
                 "xz_factor": 80.0, "y_factor": 160.0, "smear_scale_multiplier": 8.0 }"#,
        );
        assert!(matches!(
            df,
            DensityFunction::Inline(b) if matches!(*b, InlineFunction::OldBlendedNoise { .. })
        ));
    }
}
