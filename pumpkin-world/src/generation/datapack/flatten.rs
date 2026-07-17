//! Runtime flattener: parsed density-function DAGs → the flat `BaseNoiseFunctionComponent` stack
use std::collections::HashMap;

use pumpkin_data::chunk::DoublePerlinNoiseParameters;
use pumpkin_data::noise_router::{
    BaseMultiNoiseRouter, BaseNoiseFunctionComponent, BaseNoiseRouter, BaseNoiseRouters,
    BaseSurfaceEstimator, BinaryData, BinaryOperation, ClampData, ClampedYGradientData,
    FindTopSurfaceData, InterpolatedNoiseSamplerData, NoiseData, RangeChoiceData, RarityValueMapper,
    ShiftedNoiseData, SplinePoint, SplineRepr, UnaryData, UnaryOperation, WeirdScaledData,
    WrapperType,
};
use pumpkin_util::random::xoroshiro128::md5_to_lo_hi;

use crate::generation::noise::perlin::DoublePerlinNoiseSampler;

use super::codec::density_function::{
    DensityFunction, InlineFunction, NoiseParameters, NoiseRef, SplineRepr as CodecSpline,
    SplineValue as CodecSplineValue,
};
use super::codec::noise_settings::NoiseRouter;

/// Resolves references a density-function DAG makes into other registry entries
pub trait WorldgenLookup {
    /// The density function registered under `id` (`"ns:path"`), if any
    fn density_function(&self, id: &str) -> Option<&DensityFunction>;
    /// The noise parameters registered under `id` (`"ns:path"`), if any
    fn noise(&self, id: &str) -> Option<&NoiseParameters>;
    /// Whether the datapack *overrides* the noise `id`
    fn noise_is_datapack_override(&self, _id: &str) -> bool {
        false
    }
}

/// Why a DAG could not be flattened into the target component set
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenError {
    /// A node type the target `BaseNoiseFunctionComponent` enum cannot represent yet
    Unsupported(String),
    /// A `worldgen/density_function` reference that no source provides
    UnresolvedFunction(String),
    /// A `worldgen/noise` reference that no source provides
    UnresolvedNoise(String),
    /// A cycle in the density-function reference graph
    Cycle(String),
}

impl std::fmt::Display for FlattenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(k) => write!(f, "unsupported density-function type `{k}`"),
            Self::UnresolvedFunction(id) => write!(f, "unresolved density_function `{id}`"),
            Self::UnresolvedNoise(id) => write!(f, "unresolved noise `{id}`"),
            Self::Cycle(id) => write!(f, "reference cycle through `{id}`"),
        }
    }
}

/// Datapack noise parameters get synthetic ids starting here
const DATAPACK_NOISE_ID_BASE: usize = 1_000_000;

/// Resolve a `worldgen/noise` reference (by id string) into runtime `DoublePerlinNoiseParameters`
pub fn resolve_named_noise(
    lookup: &dyn WorldgenLookup,
    id: &str,
    next_id: &mut usize,
) -> Option<DoublePerlinNoiseParameters> {
    // A datapack override of a (possibly vanilla) noise wins over the const
    if !lookup.noise_is_datapack_override(id)
        && let Some(params) = DoublePerlinNoiseParameters::id_to_parameters(id)
    {
        return Some(DoublePerlinNoiseParameters::new(
            params.id,
            params.first_octave,
            params.amplitudes,
            params.lo,
            params.hi,
            params.amplitude,
        ));
    }
    let params = lookup.noise(id)?;
    let (lo, hi) = md5_to_lo_hi(id);
    let amplitudes: &'static [f64] = Box::leak(params.amplitudes.clone().into_boxed_slice());
    let amplitude = DoublePerlinNoiseSampler::get_amplitude(amplitudes);
    let nid = *next_id;
    *next_id += 1;
    Some(DoublePerlinNoiseParameters::new(
        nid,
        params.first_octave,
        amplitudes,
        lo,
        hi,
        amplitude,
    ))
}

/// Surface-rule `noise_threshold` datapack noises get synthetic ids here
pub const SURFACE_NOISE_ID_BASE: usize = 2_000_000;

/// Flattens density-function DAGs into one contiguous component stack
struct Flattener<'a> {
    lookup: &'a dyn WorldgenLookup,
    stack: Vec<BaseNoiseFunctionComponent>,
    /// Index a referenced density function resolved to (memoization)
    ref_index: HashMap<String, usize>,
    /// References currently being expanded, for cycle detection
    in_progress: Vec<String>,
    next_noise_id: usize,
}

/// Leak an owned value to `'static`
fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

impl<'a> Flattener<'a> {
    fn new(lookup: &'a dyn WorldgenLookup) -> Self {
        Self {
            lookup,
            stack: Vec::new(),
            ref_index: HashMap::new(),
            in_progress: Vec::new(),
            next_noise_id: DATAPACK_NOISE_ID_BASE,
        }
    }

    fn push(&mut self, component: BaseNoiseFunctionComponent) -> usize {
        let index = self.stack.len();
        self.stack.push(component);
        index
    }

    /// Flatten one node, returning its index in the stack
    fn flatten(&mut self, df: &DensityFunction) -> Result<usize, FlattenError> {
        match df {
            DensityFunction::Constant(value) => {
                Ok(self.push(BaseNoiseFunctionComponent::Constant { value: *value }))
            }
            DensityFunction::Reference(id) => self.flatten_reference(id),
            DensityFunction::Inline(inner) => self.flatten_inline(inner),
            DensityFunction::Unknown { kind } => Err(FlattenError::Unsupported(kind.clone())),
        }
    }

    fn flatten_reference(&mut self, id: &str) -> Result<usize, FlattenError> {
        if let Some(&index) = self.ref_index.get(id) {
            return Ok(index);
        }
        if self.in_progress.iter().any(|p| p == id) {
            return Err(FlattenError::Cycle(id.to_string()));
        }
        let target = self
            .lookup
            .density_function(id)
            .ok_or_else(|| FlattenError::UnresolvedFunction(id.to_string()))?
            // Clone to release the immutable borrow of `self.lookup` before recurse
            .clone();

        self.in_progress.push(id.to_string());
        let result = self.flatten(&target);
        self.in_progress.pop();

        let index = result?;
        self.ref_index.insert(id.to_string(), index);
        Ok(index)
    }

    #[allow(clippy::too_many_lines)]
    fn flatten_inline(&mut self, inline: &InlineFunction) -> Result<usize, FlattenError> {
        use InlineFunction as F;

        // Wrappers
        let wrapper = |this: &mut Self, arg: &DensityFunction, kind: WrapperType| {
            let input_index = this.flatten(arg)?;
            Ok(this.push(BaseNoiseFunctionComponent::Wrapper {
                input_index,
                wrapper: kind,
            }))
        };
        // Unary math ops
        let unary = |this: &mut Self, arg: &DensityFunction, op: UnaryOperation| {
            let input_index = this.flatten(arg)?;
            Ok(this.push(BaseNoiseFunctionComponent::Unary {
                input_index,
                data: leak(UnaryData { operation: op }),
            }))
        };
        // Binary math ops
        let binary = |this: &mut Self,
                      a: &DensityFunction,
                      b: &DensityFunction,
                      op: BinaryOperation| {
            let argument1_index = this.flatten(a)?;
            let argument2_index = this.flatten(b)?;
            Ok(this.push(BaseNoiseFunctionComponent::Binary {
                argument1_index,
                argument2_index,
                data: leak(BinaryData { operation: op }),
            }))
        };

        match inline {
            F::Interpolated(a) => wrapper(self, a, WrapperType::Interpolated),
            F::FlatCache(a) => wrapper(self, a, WrapperType::CacheFlat),
            F::Cache2d(a) => wrapper(self, a, WrapperType::Cache2D),
            F::CacheOnce(a) => wrapper(self, a, WrapperType::CacheOnce),
            F::CacheAllInCell(a) => wrapper(self, a, WrapperType::CellCache),
            F::BlendDensity(a) => {
                let input_index = self.flatten(a)?;
                Ok(self.push(BaseNoiseFunctionComponent::BlendDensity { input_index }))
            }
            F::Abs(a) => unary(self, a, UnaryOperation::Abs),
            F::Square(a) => unary(self, a, UnaryOperation::Square),
            F::Cube(a) => unary(self, a, UnaryOperation::Cube),
            F::HalfNegative(a) => unary(self, a, UnaryOperation::HalfNegative),
            F::QuarterNegative(a) => unary(self, a, UnaryOperation::QuarterNegative),
            F::Squeeze(a) => unary(self, a, UnaryOperation::Squeeze),
            F::Invert(a) => unary(self, a, UnaryOperation::Invert),
            F::Add(a, b) => binary(self, a, b, BinaryOperation::Add),
            F::Mul(a, b) => binary(self, a, b, BinaryOperation::Mul),
            F::Min(a, b) => binary(self, a, b, BinaryOperation::Min),
            F::Max(a, b) => binary(self, a, b, BinaryOperation::Max),
            F::Noise {
                noise,
                xz_scale,
                y_scale,
            } => {
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::Noise {
                    data: leak(NoiseData {
                        noise_id,
                        xz_scale: *xz_scale,
                        y_scale: *y_scale,
                    }),
                }))
            }
            F::ShiftedNoise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let shift_x_index = self.flatten(shift_x)?;
                let shift_y_index = self.flatten(shift_y)?;
                let shift_z_index = self.flatten(shift_z)?;
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::ShiftedNoise {
                    shift_x_index,
                    shift_y_index,
                    shift_z_index,
                    data: leak(ShiftedNoiseData {
                        xz_scale: *xz_scale,
                        y_scale: *y_scale,
                        noise_id,
                    }),
                }))
            }
            F::ShiftA(noise) => {
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::ShiftA { noise_id }))
            }
            F::ShiftB(noise) => {
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::ShiftB { noise_id }))
            }
            F::Shift(noise) => {
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::Shift { noise_id }))
            }
            F::WeirdScaledSampler {
                rarity_value_mapper,
                noise,
                input,
            } => {
                let mapper = match rarity_value_mapper.as_str() {
                    "type_1" => RarityValueMapper::Tunnels,
                    "type_2" => RarityValueMapper::Caves,
                    other => {
                        return Err(FlattenError::Unsupported(format!(
                            "weird_scaled_sampler rarity mapper `{other}`"
                        )));
                    }
                };
                let input_index = self.flatten(input)?;
                let noise_id = self.resolve_noise(noise)?;
                Ok(self.push(BaseNoiseFunctionComponent::WeirdScaledSampler {
                    input_index,
                    data: leak(WeirdScaledData { noise_id, mapper }),
                }))
            }
            F::Clamp { input, min, max } => {
                let input_index = self.flatten(input)?;
                Ok(self.push(BaseNoiseFunctionComponent::Clamp {
                    input_index,
                    data: leak(ClampData {
                        min_value: *min,
                        max_value: *max,
                    }),
                }))
            }
            F::RangeChoice {
                input,
                min_inclusive,
                max_exclusive,
                when_in_range,
                when_out_of_range,
            } => {
                let input_index = self.flatten(input)?;
                let when_in_range_index = self.flatten(when_in_range)?;
                let when_out_range_index = self.flatten(when_out_of_range)?;
                Ok(self.push(BaseNoiseFunctionComponent::RangeChoice {
                    input_index,
                    when_in_range_index,
                    when_out_range_index,
                    data: leak(RangeChoiceData {
                        min_inclusive: *min_inclusive,
                        max_exclusive: *max_exclusive,
                    }),
                }))
            }
            F::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                // Children must be pushed before the parent (indices point back)
                let input_index = self.flatten(input)?;
                let mut function_indices = Vec::with_capacity(functions.len());
                for function in functions {
                    function_indices.push(self.flatten(function)?);
                }
                let functions_indices: &'static [usize] =
                    Box::leak(function_indices.into_boxed_slice());
                let thresholds: &'static [f64] =
                    Box::leak(thresholds.clone().into_boxed_slice());
                Ok(self.push(BaseNoiseFunctionComponent::IntervalSelect {
                    input_index,
                    thresholds,
                    functions_indices,
                }))
            }
            F::YClampedGradient {
                from_y,
                to_y,
                from_value,
                to_value,
            } => Ok(self.push(BaseNoiseFunctionComponent::ClampedYGradient {
                data: leak(ClampedYGradientData {
                    from_y: *from_y,
                    to_y: *to_y,
                    from_value: *from_value,
                    to_value: *to_value,
                }),
            })),
            F::FindTopSurface {
                density,
                upper_bound,
                lower_bound,
                cell_height,
            } => {
                let density_index = self.flatten(density)?;
                let upper_bound_index = self.flatten(upper_bound)?;
                Ok(self.push(BaseNoiseFunctionComponent::FindTopSurface {
                    density_index,
                    upper_bound_index,
                    data: leak(FindTopSurfaceData {
                        lower_bound: *lower_bound,
                        cell_height: *cell_height,
                    }),
                }))
            }
            F::Constant(value) => {
                Ok(self.push(BaseNoiseFunctionComponent::Constant { value: *value }))
            }
            F::Spline(spline) => {
                let spline = self.flatten_spline(spline)?;
                Ok(self.push(BaseNoiseFunctionComponent::Spline { spline }))
            }
            F::OldBlendedNoise {
                xz_scale,
                y_scale,
                xz_factor,
                y_factor,
                smear_scale_multiplier,
            } => {
                // The interpreter multiplies `scaled_xz_scale` by 684.412 directly,
                // and folds `xz_factor / y_factor` into the y term; matching
                // codegen, `scaled_y_scale = y_scale * y_factor / xz_factor` so the
                // effective y multiplier stays `684.412 * y_scale`
                let scaled_y_scale = if *xz_factor == 0.0 {
                    *y_scale
                } else {
                    *y_scale * *y_factor / *xz_factor
                };
                Ok(self.push(BaseNoiseFunctionComponent::InterpolatedNoiseSampler {
                    data: leak(InterpolatedNoiseSamplerData {
                        scaled_xz_scale: *xz_scale,
                        scaled_y_scale,
                        xz_factor: *xz_factor,
                        y_factor: *y_factor,
                        smear_scale_multiplier: *smear_scale_multiplier,
                    }),
                }))
            }
            F::EndIslands => Ok(self.push(BaseNoiseFunctionComponent::EndIslands)),
            F::BlendAlpha => Ok(self.push(BaseNoiseFunctionComponent::BlendAlpha)),
            F::BlendOffset => Ok(self.push(BaseNoiseFunctionComponent::BlendOffset)),
            F::Beardifier => Ok(self.push(BaseNoiseFunctionComponent::Beardifier)),
        }
    }

    /// Convert a parsed spline into the leaked `&'static SplineRepr` the component stack references
    fn flatten_spline(
        &mut self,
        spline: &CodecSpline,
    ) -> Result<&'static SplineRepr, FlattenError> {
        let location_function_index = self.flatten(&spline.coordinate)?;
        let mut points = Vec::with_capacity(spline.points.len());
        for point in &spline.points {
            let value: &'static SplineRepr = match &point.value {
                CodecSplineValue::Constant(v) => leak(SplineRepr::Fixed { value: *v }),
                CodecSplineValue::Spline(nested) => self.flatten_spline(nested)?,
            };
            points.push(SplinePoint {
                location: point.location,
                value,
                derivative: point.derivative,
            });
        }
        let points: &'static [SplinePoint] = Box::leak(points.into_boxed_slice());
        Ok(leak(SplineRepr::Standard {
            location_function_index,
            points,
        }))
    }

    /// Resolve a noise reference into runtime `DoublePerlinNoiseParameters`
    fn resolve_noise(
        &mut self,
        noise: &NoiseRef,
    ) -> Result<DoublePerlinNoiseParameters, FlattenError> {
        match noise {
            NoiseRef::Reference(id) => {
                // A datapack override of a (possibly vanilla) noise wins over the compile-time const
                if !self.lookup.noise_is_datapack_override(id)
                    && let Some(params) = DoublePerlinNoiseParameters::id_to_parameters(id)
                {
                    return Ok(DoublePerlinNoiseParameters::new(
                        params.id,
                        params.first_octave,
                        params.amplitudes,
                        params.lo,
                        params.hi,
                        params.amplitude,
                    ));
                }
                let params = self
                    .lookup
                    .noise(id)
                    .ok_or_else(|| FlattenError::UnresolvedNoise(id.clone()))?
                    .clone();
                Ok(self.build_noise(id, &params))
            }
            // Seed inline noise from a stable hash of its own shape
            NoiseRef::Inline(params) => {
                let key = format!("inline:{}:{:?}", params.first_octave, params.amplitudes);
                Ok(self.build_noise(&key, params))
            }
        }
    }

    fn build_noise(&mut self, id_for_seed: &str, params: &NoiseParameters) -> DoublePerlinNoiseParameters {
        let (lo, hi) = md5_to_lo_hi(id_for_seed);
        let amplitudes: &'static [f64] = Box::leak(params.amplitudes.clone().into_boxed_slice());
        let amplitude = DoublePerlinNoiseSampler::get_amplitude(amplitudes);
        let id = self.next_noise_id;
        self.next_noise_id += 1;
        DoublePerlinNoiseParameters::new(id, params.first_octave, amplitudes, lo, hi, amplitude)
    }

    /// Flatten a set of router entries into a fresh leaked stack
    fn flatten_stack(
        lookup: &dyn WorldgenLookup,
        entries: &[&DensityFunction],
    ) -> Result<(&'static [BaseNoiseFunctionComponent], Vec<usize>), FlattenError> {
        let mut flattener = Flattener::new(lookup);
        let mut indices = Vec::with_capacity(entries.len());
        for entry in entries {
            indices.push(flattener.flatten(entry)?);
        }
        let stack: &'static [BaseNoiseFunctionComponent] =
            Box::leak(flattener.stack.into_boxed_slice());
        Ok((stack, indices))
    }
}

/// Flatten a parsed datapack noise router into the runtime [`BaseNoiseRouters`]
pub fn flatten_router(
    router: &NoiseRouter,
    lookup: &dyn WorldgenLookup,
) -> Result<BaseNoiseRouters, FlattenError> {
    // The full-density router: the ten entries the noise pipeline evaluates
    let (noise_stack, noise_idx) = Flattener::flatten_stack(
        lookup,
        &[
            &router.barrier,
            &router.fluid_level_floodedness,
            &router.fluid_level_spread,
            &router.lava,
            &router.erosion,
            &router.depth,
            &router.final_density,
            &router.vein_toggle,
            &router.vein_ridged,
            &router.vein_gap,
        ],
    )?;

    // The surface estimator has no named index field: gets its own single-entry stack
    let (surface_stack, _) =
        Flattener::flatten_stack(lookup, &[&router.preliminary_surface_level])?;

    // The multi-noise (climate) router: the six axes biome placement samples
    let (multi_stack, multi_idx) = Flattener::flatten_stack(
        lookup,
        &[
            &router.temperature,
            &router.vegetation,
            &router.continents,
            &router.erosion,
            &router.depth,
            &router.ridges,
        ],
    )?;

    Ok(BaseNoiseRouters {
        noise: BaseNoiseRouter {
            full_component_stack: noise_stack,
            barrier_noise: noise_idx[0],
            fluid_level_floodedness_noise: noise_idx[1],
            fluid_level_spread_noise: noise_idx[2],
            lava_noise: noise_idx[3],
            erosion: noise_idx[4],
            depth: noise_idx[5],
            final_density: noise_idx[6],
            vein_toggle: noise_idx[7],
            vein_ridged: noise_idx[8],
            vein_gap: noise_idx[9],
        },
        surface_estimator: BaseSurfaceEstimator {
            full_component_stack: surface_stack,
        },
        multi_noise: BaseMultiNoiseRouter {
            full_component_stack: multi_stack,
            temperature: multi_idx[0],
            vegetation: multi_idx[1],
            continents: multi_idx[2],
            erosion: multi_idx[3],
            depth: multi_idx[4],
            ridges: multi_idx[5],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::datapack::codec::noise_settings::NoiseSettings;

    /// A registry backed by hand-supplied maps, for testing the flattener in isolation from datapack scanning
    #[derive(Default)]
    struct MapLookup {
        functions: HashMap<String, DensityFunction>,
        noises: HashMap<String, NoiseParameters>,
    }

    impl WorldgenLookup for MapLookup {
        fn density_function(&self, id: &str) -> Option<&DensityFunction> {
            self.functions.get(id)
        }
        fn noise(&self, id: &str) -> Option<&NoiseParameters> {
            self.noises.get(id)
        }
    }

    fn df(json: &str) -> DensityFunction {
        serde_json::from_str(json).expect("parse density function")
    }

    #[test]
    fn shared_reference_is_flattened_once() {
        // add(ref, ref) where both args point at the same function
        let mut lookup = MapLookup::default();
        lookup
            .functions
            .insert("test:leaf".to_string(), df("0.25"));
        let root = df(
            r#"{ "type": "minecraft:add", "argument1": "test:leaf", "argument2": "test:leaf" }"#,
        );

        let (stack, indices) =
            Flattener::flatten_stack(&lookup, &[&root]).expect("flatten");
        // Constant leaf (shared) + the add node
        assert_eq!(stack.len(), 2);
        let root_index = indices[0];
        match &stack[root_index] {
            BaseNoiseFunctionComponent::Binary {
                argument1_index,
                argument2_index,
                ..
            } => assert_eq!(argument1_index, argument2_index),
            other => panic!("expected binary root, got {}", variant(other)),
        }
    }

    #[test]
    fn reference_cycle_is_reported() {
        let mut lookup = MapLookup::default();
        lookup
            .functions
            .insert("test:a".to_string(), df("\"test:b\""));
        lookup
            .functions
            .insert("test:b".to_string(), df("\"test:a\""));
        let result = Flattener::flatten_stack(&lookup, &[&df("\"test:a\"")]);
        let err = result.err().expect("expected a cycle error");
        assert!(matches!(err, FlattenError::Cycle(_)), "got {err:?}");
    }

    #[test]
    fn unsupported_node_triggers_fallback() {
        let lookup = MapLookup::default();
        // A node type we don't model
        let unknown = df(r#"{ "type": "minecraft:some_future_node", "foo": 1.0 }"#);
        assert!(matches!(
            Flattener::flatten_stack(&lookup, &[&unknown]),
            Err(FlattenError::Unsupported(_))
        ));
    }

    #[test]
    fn weird_scaled_sampler_resolves() {
        let lookup = MapLookup::default();
        let ws = df(
            r#"{ "type": "weird_scaled_sampler", "rarity_value_mapper": "type_2",
                 "noise": "minecraft:cave_entrance", "input": 0.0 }"#,
        );
        let (stack, indices) = Flattener::flatten_stack(&lookup, &[&ws]).expect("flatten");
        match &stack[indices[0]] {
            BaseNoiseFunctionComponent::WeirdScaledSampler { .. } => {}
            other => panic!("expected WeirdScaledSampler, got {}", variant(other)),
        }
    }

    #[test]
    fn shift_and_find_top_surface_resolve() {
        let lookup = MapLookup::default();
        let shift = df(r#"{ "type": "shift", "argument": "minecraft:offset" }"#);
        let (stack, indices) = Flattener::flatten_stack(&lookup, &[&shift]).expect("flatten shift");
        assert!(matches!(
            stack[indices[0]],
            BaseNoiseFunctionComponent::Shift { .. }
        ));

        let fts = df(
            r#"{ "type": "find_top_surface", "cell_height": 8, "lower_bound": -64,
                 "density": 0.0, "upper_bound": 1.0 }"#,
        );
        let (stack, indices) = Flattener::flatten_stack(&lookup, &[&fts]).expect("flatten fts");
        assert!(matches!(
            stack[indices[0]],
            BaseNoiseFunctionComponent::FindTopSurface { .. }
        ));
    }

    #[test]
    fn vanilla_noise_reference_reuses_constant() {
        let lookup = MapLookup::default();
        let n = df(
            r#"{ "type": "minecraft:noise", "noise": "minecraft:cave_entrance", "xz_scale": 1.0, "y_scale": 1.0 }"#,
        );
        let (stack, _) = Flattener::flatten_stack(&lookup, &[&n]).expect("flatten");
        let expected = DoublePerlinNoiseParameters::id_to_parameters("minecraft:cave_entrance")
            .expect("vanilla noise");
        match &stack[0] {
            BaseNoiseFunctionComponent::Noise { data } => {
                assert_eq!(data.noise_id.lo, expected.lo);
                assert_eq!(data.noise_id.hi, expected.hi);
                assert_eq!(data.noise_id.id, expected.id);
            }
            other => panic!("expected noise, got {}", variant(other)),
        }
    }

    #[test]
    fn datapack_noise_seeds_from_md5_of_id() {
        let mut lookup = MapLookup::default();
        lookup.noises.insert(
            "test:custom".to_string(),
            NoiseParameters {
                first_octave: -7,
                amplitudes: vec![1.0, 1.0],
            },
        );
        let n = df(
            r#"{ "type": "minecraft:noise", "noise": "test:custom", "xz_scale": 1.0, "y_scale": 1.0 }"#,
        );
        let (stack, _) = Flattener::flatten_stack(&lookup, &[&n]).expect("flatten");
        let (lo, hi) = md5_to_lo_hi("test:custom");
        match &stack[0] {
            BaseNoiseFunctionComponent::Noise { data } => {
                assert_eq!(data.noise_id.lo, lo);
                assert_eq!(data.noise_id.hi, hi);
                assert_eq!(data.noise_id.first_octave, -7);
                assert!(data.noise_id.id >= DATAPACK_NOISE_ID_BASE);
            }
            other => panic!("expected noise, got {}", variant(other)),
        }
    }

    #[test]
    fn flattens_a_full_minimal_router() {
        // Every entry vanilla-resolvable or a constant; the whole router flattens
        let json = r#"{
            "noise": { "min_y": -64, "height": 384, "size_horizontal": 1, "size_vertical": 2 },
            "default_block": { "Name": "minecraft:stone" },
            "default_fluid": { "Name": "minecraft:water" },
            "sea_level": 63,
            "noise_router": {
                "barrier": 0.0, "fluid_level_floodedness": 0.0, "fluid_level_spread": 0.0,
                "lava": 0.0, "temperature": 0.0, "vegetation": 0.0, "continents": 0.0,
                "erosion": 0.0, "depth": 0.0, "ridges": 0.0, "preliminary_surface_level": 0.0,
                "final_density": { "type": "minecraft:add", "argument1": 1.0, "argument2": -1.0 },
                "vein_toggle": 0.0, "vein_ridged": 0.0, "vein_gap": 0.0
            },
            "surface_rule": { "type": "minecraft:block", "result_state": { "Name": "minecraft:stone" } }
        }"#;
        let settings: NoiseSettings = serde_json::from_str(json).expect("parse settings");
        let lookup = MapLookup::default();
        let routers = flatten_router(&settings.noise_router, &lookup).expect("flatten router");
        assert_eq!(routers.noise.full_component_stack.len(), 12);
        assert!(matches!(
            routers.noise.full_component_stack[routers.noise.final_density],
            BaseNoiseFunctionComponent::Binary { .. }
        ));
    }

    /// Human-readable variant name for assertion messages
    fn variant(c: &BaseNoiseFunctionComponent) -> &'static str {
        match c {
            BaseNoiseFunctionComponent::Constant { .. } => "Constant",
            BaseNoiseFunctionComponent::Binary { .. } => "Binary",
            BaseNoiseFunctionComponent::Noise { .. } => "Noise",
            _ => "other",
        }
    }
}
