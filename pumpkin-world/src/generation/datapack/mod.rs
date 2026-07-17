//! Datapack-driven world generation
pub mod biome_placement;
pub mod codec;
pub mod flatten;
mod source;
mod vanilla;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use pumpkin_data::chunk_gen_settings::{
    GenerationSettings, GenerationShapeConfig, MaterialRule, SequenceMaterialRule,
};
use pumpkin_data::dimension::Dimension;
use pumpkin_data::noise_router::BaseNoiseRouters;
use tracing::{debug, info, warn};

pub use source::{Category, RawWorldgen};

use biome_placement::DatapackBiomeSupplier;
use codec::{
    biome_source::DimensionFile,
    density_function::{DensityFunction, NoiseParameters},
    noise_settings::NoiseSettings,
};
use flatten::{FlattenError, WorldgenLookup, flatten_router};

/// Parsed, indexed world-generation data from a world's datapacks
#[derive(Default)]
pub struct WorldgenData {
    raw: RawWorldgen,
}

impl WorldgenData {
    /// Scan the given datapack sources and index their world-generation JSON
    #[must_use]
    pub fn load(sources: &[PathBuf]) -> Self {
        let raw = source::scan(sources);
        let data = Self { raw };
        data.report_coverage();
        data
    }

    /// The raw JSON index (`namespace:path` → JSON) per category
    #[must_use]
    pub const fn raw(&self) -> &RawWorldgen {
        &self.raw
    }

    /// True if the world's datapacks contribute no worldgen data
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Parse a single noise-settings entry by resource location (`ns:id`)
    #[must_use]
    pub fn noise_settings(&self, id: &str) -> Option<NoiseSettings> {
        let json = self.raw.get(Category::NoiseSettings)?.get(id)?;
        match serde_json::from_str(json) {
            Ok(settings) => Some(settings),
            Err(e) => {
                warn!("Malformed noise_settings `{id}`: {e}");
                None
            }
        }
    }

    /// Build the runtime noise routers for a `worldgen/noise_settings` entry
    pub fn resolve_base_routers(&self, id: &str) -> Result<BaseNoiseRouters, ResolveError> {
        let settings = self
            .noise_settings(id)
            .ok_or_else(|| ResolveError::MissingSettings(id.to_string()))?;
        let registry = ParsedRegistry::from_raw(&self.raw);
        flatten_router(&settings.noise_router, &registry).map_err(ResolveError::Flatten)
    }

    /// Log a one-line-per-category summary and any density-function *types* we do not yet model
    fn report_coverage(&self) {
        if self.raw.is_empty() {
            return;
        }
        info!(
            "Datapack worldgen indexed: {} noise_settings, {} density_function, {} noise, {} dimension",
            self.raw.count(Category::NoiseSettings),
            self.raw.count(Category::DensityFunction),
            self.raw.count(Category::Noise),
            self.raw.count(Category::Dimension),
        );

        // Best-effort scan for unmodeled density-function types
        let mut unknown_kinds: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        if let Some(dfs) = self.raw.get(Category::DensityFunction) {
            for json in dfs.values() {
                if let Ok(df) = serde_json::from_str::<DensityFunction>(json) {
                    collect_unknown_kinds(&df, &mut unknown_kinds);
                }
            }
        }
        if let Some(settings) = self.raw.get(Category::NoiseSettings) {
            for json in settings.values() {
                if let Ok(parsed) = serde_json::from_str::<NoiseSettings>(json) {
                    for (_name, df) in parsed.noise_router.entries() {
                        collect_unknown_kinds(df, &mut unknown_kinds);
                    }
                }
            }
        }

        if !unknown_kinds.is_empty() {
            warn!(
                "Datapack worldgen uses {} density-function type(s) not yet modeled \
                 (retained, ignored for now): {}",
                unknown_kinds.len(),
                unknown_kinds.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
    }
}

/// The datapack worldgen data for the currently-loaded world
static ACTIVE_WORLDGEN: RwLock<Option<WorldgenData>> = RwLock::new(None);

/// Install the active world's datapack worldgen data (call at world load)
pub fn set_active_worldgen(data: WorldgenData) {
    *ACTIVE_WORLDGEN.write().unwrap() = Some(data);
}

/// Clear the active worldgen data
pub fn clear_active_worldgen() {
    *ACTIVE_WORLDGEN.write().unwrap() = None;
}

/// Per-category counts of the active world's indexed datapack worldgen JSON
#[must_use]
pub fn active_worldgen_counts() -> Option<WorldgenCounts> {
    let guard = ACTIVE_WORLDGEN.read().unwrap();
    let data = guard.as_ref()?;
    if data.is_empty() {
        return None;
    }
    Some(WorldgenCounts {
        noise_settings: data.raw.count(Category::NoiseSettings),
        density_functions: data.raw.count(Category::DensityFunction),
        noise: data.raw.count(Category::Noise),
        dimensions: data.raw.count(Category::Dimension),
    })
}

/// Indexed datapack worldgen entry counts (see [`active_worldgen_counts`])
pub struct WorldgenCounts {
    pub noise_settings: usize,
    pub density_functions: usize,
    pub noise: usize,
    pub dimensions: usize,
}

/// The `worldgen/noise_settings` id a dimension generates from
#[must_use]
pub fn dimension_settings_id(dimension: &Dimension) -> Option<&'static str> {
    if *dimension == Dimension::OVERWORLD {
        Some("minecraft:overworld")
    } else if *dimension == Dimension::THE_NETHER {
        Some("minecraft:nether")
    } else if *dimension == Dimension::THE_END {
        Some("minecraft:end")
    } else {
        None
    }
}

/// The `dimension/<id>.json` id whose `biome_source` places biomes for a dimension
#[must_use]
pub fn dimension_json_id(dimension: &Dimension) -> Option<&'static str> {
    if *dimension == Dimension::OVERWORLD {
        Some("minecraft:overworld")
    } else if *dimension == Dimension::THE_NETHER {
        Some("minecraft:the_nether")
    } else if *dimension == Dimension::THE_END {
        Some("minecraft:the_end")
    } else {
        None
    }
}

/// A datapack-driven noise router **and** its matching generation settings for a
/// single dimension, resolved as a consistent unit
pub struct DatapackDimension {
    /// The flattened runtime noise routers
    pub routers: BaseNoiseRouters,
    /// The runtime generation settings
    pub settings: &'static GenerationSettings,
    /// Runtime multi-noise biome placement
    pub biome_placement: Option<&'static DatapackBiomeSupplier>,
}

/// Resolve datapack-driven noise routers **and** generation settings for a dimension
#[must_use]
pub fn resolve_active_dimension(dimension: &Dimension) -> Option<DatapackDimension> {
    let settings_id = dimension_settings_id(dimension)?;
    let guard = ACTIVE_WORLDGEN.read().unwrap();
    let data = guard.as_ref()?;
    if data.is_empty() {
        return None;
    }
    let settings_json = data.noise_settings(settings_id)?;

    // Flatten the router
    let registry = ParsedRegistry::from_raw(&data.raw);
    let routers = match flatten_router(&settings_json.noise_router, &registry) {
        Ok(routers) => routers,
        Err(e) => {
            debug!("Datapack router for {settings_id} falls back to vanilla: {e}");
            return None;
        }
    };

    let vanilla = GenerationSettings::from_dimension(dimension);
    let Some(settings) = build_settings(&settings_json, vanilla, &registry) else {
        debug!("Datapack settings for {settings_id} malformed; falling back to vanilla");
        return None;
    };
    let settings: &'static GenerationSettings = Box::leak(Box::new(settings));

    // Datapack biome placement
    let biome_placement = build_biome_placement(data, dimension).map(|supplier| {
        info!(
            "Using datapack biome placement for {} ({} entries)",
            dimension_json_id(dimension).unwrap_or("?"),
            supplier.len()
        );
        &*Box::leak(Box::new(supplier))
    });

    info!("Using datapack noise router + settings for {settings_id}");
    Some(DatapackDimension {
        routers,
        settings,
        biome_placement,
    })
}

/// Build the runtime datapack biome placement for a dimension
fn build_biome_placement(
    data: &WorldgenData,
    dimension: &Dimension,
) -> Option<DatapackBiomeSupplier> {
    let dim_id = dimension_json_id(dimension)?;
    let json = data.raw.get(Category::Dimension)?.get(dim_id)?;
    let parsed: DimensionFile = match serde_json::from_str(json) {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!("Malformed dimension `{dim_id}`: {e}");
            return None;
        }
    };
    let biomes = parsed.generator.biome_source?.biomes;
    if biomes.is_empty() {
        return None;
    }
    DatapackBiomeSupplier::from_entries(&biomes)
}

/// Build a runtime [`GenerationSettings`] from a parsed datapack `noise_settings`
fn build_settings(
    settings: &NoiseSettings,
    vanilla: &'static GenerationSettings,
    lookup: &dyn crate::generation::datapack::flatten::WorldgenLookup,
) -> Option<GenerationSettings> {
    use crate::block::BlockStateCodec;
    use crate::generation::datapack::codec::surface_rule::build_surface_rule;

    let default_block = serde_json::from_value::<BlockStateCodec>(settings.default_block.clone())
        .ok()?
        .get_state();
    let default_fluid = serde_json::from_value::<BlockStateCodec>(settings.default_fluid.clone())
        .ok()?
        .get_state();

    let shape = GenerationShapeConfig {
        min_y: i8::try_from(settings.noise.min_y).ok()?,
        height: u16::try_from(settings.noise.height).ok()?,
        size_horizontal: u8::try_from(settings.noise.size_horizontal).ok()?,
        size_vertical: u8::try_from(settings.noise.size_vertical).ok()?,
    };

    // Parse the datapack's own `surface_rule`
    let surface_rule = build_surface_rule(&settings.surface_rule, lookup).unwrap_or_else(|| {
        debug!("Datapack surface_rule for this dimension falls back to vanilla surfaces");
        MaterialRule::Sequence(SequenceMaterialRule {
            sequence: std::slice::from_ref(&vanilla.surface_rule),
        })
    });

    Some(GenerationSettings {
        aquifers_enabled: settings.aquifers_enabled,
        ore_veins_enabled: settings.ore_veins_enabled,
        legacy_random_source: settings.legacy_random_source,
        sea_level: settings.sea_level,
        default_fluid,
        shape,
        surface_rule,
        default_block,
    })
}

/// Why building runtime routers for a noise-settings entry failed
#[derive(Debug)]
pub enum ResolveError {
    /// No `worldgen/noise_settings` entry (or it was malformed)
    MissingSettings(String),
    /// The density-function DAG could not be flattened into the target set
    Flatten(FlattenError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSettings(id) => write!(f, "missing noise_settings `{id}`"),
            Self::Flatten(e) => write!(f, "{e}"),
        }
    }
}

/// A [`WorldgenLookup`] over the `density_function` and `noise` registries
struct ParsedRegistry {
    functions: HashMap<String, DensityFunction>,
    noises: HashMap<String, NoiseParameters>,
    noise_overrides: std::collections::HashSet<String>,
}

impl ParsedRegistry {
    fn from_raw(raw: &RawWorldgen) -> Self {
        // Vanilla baseline first, datapack overrides second (last write wins)
        let mut functions = Self::parse_vanilla(
            vanilla::VANILLA_DENSITY_FUNCTION_IDS,
            vanilla::get_vanilla_density_function_json,
            "density_function",
        );
        functions.extend(Self::parse_category(raw, Category::DensityFunction));

        let mut noises = Self::parse_vanilla(
            vanilla::VANILLA_NOISE_IDS,
            vanilla::get_vanilla_noise_json,
            "noise",
        );
        let datapack_noises = Self::parse_category(raw, Category::Noise);
        let disabled = std::env::var("PUMPKIN_DISABLE_NOISE_OVERRIDES").unwrap_or_default();
        let noise_overrides = datapack_noises
            .keys()
            .filter(|k| !disabled.split(',').any(|d| d.trim() == k.as_str()))
            .cloned()
            .collect();
        noises.extend(datapack_noises);

        Self {
            functions,
            noises,
            noise_overrides,
        }
    }

    /// Parse every embedded vanilla entry into an owned map
    fn parse_vanilla<T: serde::de::DeserializeOwned>(
        ids: &[&str],
        get_json: fn(&str) -> Option<&'static str>,
        label: &str,
    ) -> HashMap<String, T> {
        let mut out = HashMap::new();
        for &id in ids {
            let Some(json) = get_json(id) else { continue };
            match serde_json::from_str::<T>(json) {
                Ok(parsed) => {
                    out.insert(id.to_string(), parsed);
                }
                // Vanilla JSON is trusted; a failure here means the codec lags the
                // embedded data version, which is worth surfacing loudly
                Err(e) => warn!("Malformed embedded vanilla {label} `{id}`: {e}"),
            }
        }
        out
    }

    fn parse_category<T: serde::de::DeserializeOwned>(
        raw: &RawWorldgen,
        category: Category,
    ) -> HashMap<String, T> {
        let mut out = HashMap::new();
        if let Some(entries) = raw.get(category) {
            for (id, json) in entries {
                match serde_json::from_str::<T>(json) {
                    Ok(parsed) => {
                        out.insert(id.clone(), parsed);
                    }
                    Err(e) => warn!("Malformed {} `{id}`: {e}", category.label()),
                }
            }
        }
        out
    }
}

impl WorldgenLookup for ParsedRegistry {
    fn density_function(&self, id: &str) -> Option<&DensityFunction> {
        lookup_normalized(&self.functions, id)
    }
    fn noise(&self, id: &str) -> Option<&NoiseParameters> {
        lookup_normalized(&self.noises, id)
    }
    fn noise_is_datapack_override(&self, id: &str) -> bool {
        self.noise_overrides.contains(id)
            || (!id.contains(':') && self.noise_overrides.contains(&format!("minecraft:{id}")))
    }
}

/// Look up `id` in a registry keyed by fully-qualified resource location
fn lookup_normalized<'a, T>(map: &'a HashMap<String, T>, id: &str) -> Option<&'a T> {
    if let Some(value) = map.get(id) {
        return Some(value);
    }
    if id.contains(':') {
        None
    } else {
        map.get(&format!("minecraft:{id}"))
    }
}

/// Walk a density-function tree, collecting the `type` strings of any unmodeled nodes
fn collect_unknown_kinds(
    df: &DensityFunction,
    out: &mut std::collections::BTreeSet<String>,
) {
    use codec::density_function::InlineFunction as F;

    if let Some(kind) = df.unknown_kind() {
        out.insert(kind.to_string());
        return;
    }
    let DensityFunction::Inline(inner) = df else {
        return;
    };
    match inner.as_ref() {
        F::Interpolated(a)
        | F::FlatCache(a)
        | F::Cache2d(a)
        | F::CacheOnce(a)
        | F::CacheAllInCell(a)
        | F::BlendDensity(a)
        | F::Abs(a)
        | F::Square(a)
        | F::Cube(a)
        | F::HalfNegative(a)
        | F::QuarterNegative(a)
        | F::Squeeze(a)
        | F::Invert(a) => collect_unknown_kinds(a, out),
        F::Add(a, b) | F::Mul(a, b) | F::Min(a, b) | F::Max(a, b) => {
            collect_unknown_kinds(a, out);
            collect_unknown_kinds(b, out);
        }
        F::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            collect_unknown_kinds(shift_x, out);
            collect_unknown_kinds(shift_y, out);
            collect_unknown_kinds(shift_z, out);
        }
        F::WeirdScaledSampler { input, .. } | F::Clamp { input, .. } => {
            collect_unknown_kinds(input, out);
        }
        F::FindTopSurface {
            density,
            upper_bound,
            ..
        } => {
            collect_unknown_kinds(density, out);
            collect_unknown_kinds(upper_bound, out);
        }
        F::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => {
            collect_unknown_kinds(input, out);
            collect_unknown_kinds(when_in_range, out);
            collect_unknown_kinds(when_out_of_range, out);
        }
        F::IntervalSelect {
            input, functions, ..
        } => {
            collect_unknown_kinds(input, out);
            for function in functions {
                collect_unknown_kinds(function, out);
            }
        }
        F::Spline(spline) => collect_spline_unknown_kinds(spline, out),
        // Leaf / no-argument nodes.
        F::Noise { .. }
        | F::ShiftA(_)
        | F::ShiftB(_)
        | F::Shift(_)
        | F::YClampedGradient { .. }
        | F::Constant(_)
        | F::OldBlendedNoise { .. }
        | F::EndIslands
        | F::BlendAlpha
        | F::BlendOffset
        | F::Beardifier => {}
    }
}

fn collect_spline_unknown_kinds(
    spline: &codec::density_function::SplineRepr,
    out: &mut std::collections::BTreeSet<String>,
) {
    use codec::density_function::SplineValue;
    collect_unknown_kinds(&spline.coordinate, out);
    for point in &spline.points {
        match &point.value {
            SplineValue::Constant(_) => {}
            SplineValue::Spline(nested) => collect_spline_unknown_kinds(nested, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sources_yield_empty_data() {
        let data = WorldgenData::load(&[]);
        assert!(data.is_empty());
        assert!(data.noise_settings("minecraft:overworld").is_none());
    }

    #[test]
    fn embedded_vanilla_baseline_is_populated() {
        // The vanilla worldgen JSON is embedded
        let registry = ParsedRegistry::from_raw(&RawWorldgen::default());
        assert!(
            registry.density_function("minecraft:shift_x").is_some(),
            "vanilla shift_x should be embedded"
        );
        assert!(
            registry.noise("minecraft:offset").is_some(),
            "vanilla offset noise should be embedded"
        );
        assert!(!vanilla::VANILLA_DENSITY_FUNCTION_IDS.is_empty());
        assert!(!vanilla::VANILLA_NOISE_IDS.is_empty());
    }

    #[test]
    fn flattens_a_router_referencing_vanilla_density_functions() {
        use pumpkin_data::noise_router::BaseNoiseFunctionComponent;

        let json = r#"{
            "noise": { "min_y": -64, "height": 384, "size_horizontal": 1, "size_vertical": 2 },
            "default_block": { "Name": "minecraft:stone" },
            "default_fluid": { "Name": "minecraft:water" },
            "sea_level": 63,
            "noise_router": {
                "barrier": 0.0, "fluid_level_floodedness": 0.0, "fluid_level_spread": 0.0,
                "lava": 0.0, "temperature": "minecraft:shift_x", "vegetation": "minecraft:shift_z",
                "continents": 0.0, "erosion": 0.0, "depth": "minecraft:y", "ridges": 0.0,
                "preliminary_surface_level": "minecraft:zero",
                "final_density": "minecraft:shift_x",
                "vein_toggle": 0.0, "vein_ridged": 0.0, "vein_gap": 0.0
            },
            "surface_rule": { "type": "minecraft:block", "result_state": { "Name": "minecraft:stone" } }
        }"#;
        let settings: NoiseSettings = serde_json::from_str(json).expect("parse settings");
        let registry = ParsedRegistry::from_raw(&RawWorldgen::default());
        let routers = flatten::flatten_router(&settings.noise_router, &registry)
            .expect("vanilla references resolve through the embedded baseline");

        // shift_x's leaf is ShiftA over the `offset` noise
        assert!(
            routers
                .noise
                .full_component_stack
                .iter()
                .any(|c| matches!(c, BaseNoiseFunctionComponent::ShiftA { .. })),
            "expected a resolved ShiftA from minecraft:shift_x"
        );
    }

    /// Validates parsing against the real Terralith/Tectonic/CliffTree packs
    /// Ignored by default (depends on local datapacks)
    /// Point the `WORLDGEN_PACK_DIR` env var at a folder of datapacks (.zip or extracted) and run with:
    /// `WORLDGEN_PACK_DIR="<path to datapacks>" cargo test -p pumpkin-world -- --ignored --nocapture parses_real_terralith`
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn parses_real_terralith() {
        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder to run this test");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }
        let data = WorldgenData::load(&[dir]);
        assert!(!data.is_empty(), "should index Terralith worldgen");

        // Terralith overrides minecraft:overworld noise settings; it must parse and
        // every router entry must decode (constant / reference / inline / unknown)
        let overworld = data
            .noise_settings("minecraft:overworld")
            .expect("terralith overrides minecraft:overworld noise_settings");
        assert_eq!(overworld.noise_router.entries().len(), 15);

        // Every density_function entry must parse without panicking
        let dfs = data.raw().get(Category::DensityFunction).unwrap();
        let mut parsed = 0usize;
        for (id, json) in dfs {
            serde_json::from_str::<DensityFunction>(json)
                .unwrap_or_else(|e| panic!("failed to parse density function {id}: {e}"));
            parsed += 1;
        }
        eprintln!("parsed {parsed} density functions across all packs");

        // Attempt to resolve the overworld noise router end-to-end against the embedded vanilla baseline
        match data.resolve_base_routers("minecraft:overworld") {
            Ok(routers) => eprintln!(
                "resolved overworld router: {} noise / {} surface / {} multi_noise components",
                routers.noise.full_component_stack.len(),
                routers.surface_estimator.full_component_stack.len(),
                routers.multi_noise.full_component_stack.len(),
            ),
            Err(e) => eprintln!("overworld router falls back to vanilla: {e}"),
        }
    }

    /// End-to-end: installing datapack worldgen makes `VanillaGenerator` build its
    /// noise router from the datapack instead of the compile-time vanilla `const`
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn generator_uses_datapack_router() {
        use crate::generation::{
            Seed,
            generator::{GeneratorInit, VanillaGenerator},
        };

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder to run this test");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        // With datapack data installed, the overworld generator uses its router
        set_active_worldgen(WorldgenData::load(&[dir]));
        let resolved = resolve_active_dimension(&Dimension::OVERWORLD).is_some();
        let datapack_gen = VanillaGenerator::new(Seed(42), Dimension::OVERWORLD);
        let datapack_len = datapack_gen.base_router.noise.full_component_stack.len();

        // Without it, the same generator falls back to the vanilla `const`
        clear_active_worldgen();
        let vanilla_gen = VanillaGenerator::new(Seed(42), Dimension::OVERWORLD);
        let vanilla_len = vanilla_gen.base_router.noise.full_component_stack.len();

        eprintln!(
            "datapack noise stack: {datapack_len}, vanilla: {vanilla_len} (overworld resolved: {resolved})"
        );
        if resolved {
            assert_ne!(
                datapack_len, vanilla_len,
                "a resolved datapack router should differ from the vanilla router"
            );
        } else {
            eprintln!(
                "overworld fell back to vanilla (modded/unsupported density-function types); \
                 datapack terrain not driven for this pack"
            );
        }
        clear_active_worldgen();
    }

    /// The datapack's own `surface_rule` parses into a runtime
    /// `MaterialRule` whose biome conditions reference real modded biome ids
    /// (>=65) and whose `noise_threshold` conditions resolve the pack's custom noises
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn datapack_surface_rule_resolves_modded_biomes() {
        use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;
        use crate::generation::datapack::codec::surface_rule::build_surface_rule;
        use pumpkin_data::chunk_gen_settings::{MaterialCondition, MaterialRule};

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder to run this test");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        // Register modded biomes so names resolve to their runtime ids
        {
            let mut registry = DYNAMIC_BIOMES.write().unwrap();
            registry.load_datapack_definitions(&[dir.clone()]);
            registry.register_datapack_biomes();
        }
        let data = WorldgenData::load(&[dir]);
        let settings = data
            .noise_settings("minecraft:overworld")
            .expect("overworld noise_settings present in pack");
        let registry = ParsedRegistry::from_raw(&data.raw);

        let rule = build_surface_rule(&settings.surface_rule, &registry)
            .expect("datapack surface_rule should build (not fall back)");

        // Recursively collect referenced biome ids + count noise_threshold conditions
        fn walk(rule: &MaterialRule, biomes: &mut std::collections::BTreeSet<u8>, noise: &mut usize) {
            match rule {
                MaterialRule::Sequence(seq) => seq.sequence.iter().for_each(|r| walk(r, biomes, noise)),
                MaterialRule::Condition(cond) => {
                    walk_cond(&cond.if_true, biomes, noise);
                    walk(cond.then_run, biomes, noise);
                }
                MaterialRule::Block(_) | MaterialRule::Badlands(_) => {}
            }
        }
        fn walk_cond(
            cond: &MaterialCondition,
            biomes: &mut std::collections::BTreeSet<u8>,
            noise: &mut usize,
        ) {
            match cond {
                MaterialCondition::Biome(b) => biomes.extend(b.biome_is.iter().copied()),
                MaterialCondition::NoiseThreshold(_) => *noise += 1,
                MaterialCondition::Not(n) => walk_cond(n.invert, biomes, noise),
                _ => {}
            }
        }

        let mut biomes = std::collections::BTreeSet::new();
        let mut noise = 0;
        walk(&rule, &mut biomes, &mut noise);
        let modded: Vec<u8> = biomes.iter().copied().filter(|&id| id >= 65).collect();
        eprintln!(
            "surface_rule: {} distinct biome ids ({} modded), {noise} noise_threshold conditions",
            biomes.len(),
            modded.len()
        );

        assert!(
            noise > 50,
            "Terralith surface_rule should carry many custom-noise conditions, got {noise}"
        );
        assert!(
            !modded.is_empty(),
            "surface_rule biome conditions should resolve to real modded ids (>=65)"
        );

        clear_active_worldgen();
        crate::chunk::dynamic_biome::clear_dynamic_biomes();
    }

    /// Generating a chunk with datapack biomes installed places real modded biome ids
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn datapack_places_modded_biomes() {
        use crate::ProtoChunk;
        use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;
        use crate::generation::{
            Seed, biome_coords,
            generator::{GeneratorInit, VanillaGenerator, WorldGenerator},
            noise::router::multi_noise_sampler::{
                MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
            },
            positions::chunk_pos,
        };

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder to run this test");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        // Loading the datapack registers its modded biomes and installs its worldgen
        {
            let mut registry = DYNAMIC_BIOMES.write().unwrap();
            registry.load_datapack_definitions(&[dir.clone()]);
            registry.register_datapack_biomes();
        }
        set_active_worldgen(WorldgenData::load(&[dir]));

        // `ProtoChunk::new` takes the `WorldGenerator` enum
        let world_gen = WorldGenerator::Noise(Box::new(VanillaGenerator::new(
            Seed(-12969086726167675i64 as u64),
            Dimension::OVERWORLD,
        )));
        let WorldGenerator::Noise(generator) = &world_gen else {
            unreachable!("just constructed a Noise generator")
        };
        assert!(
            generator.datapack_biome_supplier.is_some(),
            "datapack biome placement should be active"
        );

        // Sample a spread of chunks; count how many distinct biome ids are modded
        let mut modded = std::collections::BTreeSet::new();
        let mut vanilla = std::collections::BTreeSet::new();
        for (cx, cz) in [(0, 0), (8, 8), (-8, 5), (20, -13), (-30, -30)] {
            let mut chunk = ProtoChunk::new(cx, cz, &world_gen);
            let start_x = chunk_pos::start_block_x(cx);
            let start_z = chunk_pos::start_block_z(cz);
            let options = MultiNoiseSamplerBuilderOptions::new(
                biome_coords::from_block(start_x),
                biome_coords::from_block(start_z),
                biome_coords::from_block(16) as usize,
            );
            let mut sampler =
                MultiNoiseSampler::generate(&generator.base_router.multi_noise, &options);
            chunk.populate_biomes(generator, &mut sampler);
            for &id in &chunk.flat_biome_map {
                if id >= 65 {
                    modded.insert(id);
                } else {
                    vanilla.insert(id);
                }
            }
        }
        eprintln!(
            "placed {} distinct modded biome id(s), {} vanilla id(s)",
            modded.len(),
            vanilla.len()
        );
        clear_active_worldgen();
        assert!(
            !modded.is_empty(),
            "expected at least one modded (datapack) biome to be placed"
        );
    }

    /// Golden-chunk biome parity
    #[test]
    #[ignore = "golden-chunk parity vs a real world; set GOLDEN_WORLD_DIR"]
    #[allow(clippy::print_stderr, clippy::cast_precision_loss)]
    fn golden_chunk_biome_parity() {
        use crate::ProtoChunk;
        use crate::chunk::ChunkData;
        use crate::chunk::dynamic_biome::{DYNAMIC_BIOMES, clear_dynamic_biomes};
        use crate::chunk::format::anvil::SingleChunkDataSerializer;
        use crate::generation::{
            Seed, biome_coords,
            generator::{GeneratorInit, VanillaGenerator, WorldGenerator},
            noise::router::multi_noise_sampler::{
                MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
            },
            positions::chunk_pos,
        };
        use pumpkin_data::chunk::Biome;
        use pumpkin_util::math::vector2::Vector2;
        use std::io::Read;

        const SEED: i64 = -12969086726167675;

        let Some(world) = std::env::var_os("GOLDEN_WORLD_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set GOLDEN_WORLD_DIR to the world root");
            return;
        };
        let datapacks = world.join("datapacks");
        let region_dir = world.join("dimensions/minecraft/overworld/region");
        if !region_dir.is_dir() {
            eprintln!("skipping: {region_dir:?} not present");
            return;
        }

        // Register modded biomes + install datapack worldgen
        clear_dynamic_biomes();
        {
            let mut reg = DYNAMIC_BIOMES.write().unwrap();
            reg.load_datapack_definitions(&[datapacks.clone()]);
            reg.register_datapack_biomes();
        }
        set_active_worldgen(WorldgenData::load(&[datapacks]));

        // id -> name for readable mismatch reports
        let reverse: HashMap<u8, String> = {
            let reg = DYNAMIC_BIOMES.read().unwrap();
            reg.names()
                .into_iter()
                .filter_map(|n| reg.lookup(&n).map(|id| (id, n)))
                .collect()
        };
        let name_of = |id: u8| -> String {
            if let Some(n) = reverse.get(&id) {
                return n.clone();
            }
            Biome::from_id(id).map_or_else(|| format!("id{id}"), |b| b.registry_id.to_string())
        };

        let world_gen = WorldGenerator::Noise(Box::new(VanillaGenerator::new(
            Seed(SEED as u64),
            Dimension::OVERWORLD,
        )));
        let WorldGenerator::Noise(generator) = &world_gen else {
            unreachable!("just constructed a Noise generator")
        };
        assert!(
            generator.datapack_biome_supplier.is_some(),
            "datapack biome placement must be active for this comparison"
        );

        /// Decode a `.mca` region file into `(chunk_x, chunk_z) -> decompressed NBT`
        fn read_region(path: &std::path::Path, rx: i32, rz: i32) -> HashMap<(i32, i32), Vec<u8>> {
            let mut out = HashMap::new();
            let Ok(data) = std::fs::read(path) else {
                return out;
            };
            if data.len() < 4096 {
                return out;
            }
            for idx in 0..1024usize {
                let e = idx * 4;
                let entry = u32::from_be_bytes([data[e], data[e + 1], data[e + 2], data[e + 3]]);
                let offset = (entry >> 8) as usize;
                if offset == 0 {
                    continue;
                }
                let byte_off = offset * 4096;
                if byte_off + 5 > data.len() {
                    continue;
                }
                let length = u32::from_be_bytes([
                    data[byte_off],
                    data[byte_off + 1],
                    data[byte_off + 2],
                    data[byte_off + 3],
                ]) as usize;
                if length == 0 || byte_off + 4 + length > data.len() {
                    continue;
                }
                let comp = data[byte_off + 5 - 1];
                let payload = &data[byte_off + 5..byte_off + 4 + length];
                let mut decompressed = Vec::new();
                let ok = match comp {
                    1 => flate2::read::GzDecoder::new(payload)
                        .read_to_end(&mut decompressed)
                        .is_ok(),
                    2 => flate2::read::ZlibDecoder::new(payload)
                        .read_to_end(&mut decompressed)
                        .is_ok(),
                    3 => {
                        decompressed = payload.to_vec();
                        true
                    }
                    _ => false, // LZ4/custom: skip
                };
                if !ok {
                    continue;
                }
                let cx = rx * 32 + (idx as i32 & 31);
                let cz = rz * 32 + (idx as i32 >> 5);
                out.insert((cx, cz), decompressed);
            }
            out
        }

        let regions = [
            ("r.0.0.mca", 0, 0),
            ("r.0.-1.mca", 0, -1),
            ("r.1.0.mca", 1, 0),
            ("r.1.-1.mca", 1, -1),
        ];

        let mut total = 0u64;
        let mut matches = 0u64;
        let mut surf_total = 0u64;
        let mut surf_match = 0u64;
        let mut chunks = 0u64;
        let mut skipped_protochunks = 0u64;
        let mut ref_hist: HashMap<u8, u64> = HashMap::new();
        let mut gen_hist: HashMap<u8, u64> = HashMap::new();
        // Per-(ref,gen) confusion tally to see systematic swaps
        let mut confusion: HashMap<(u8, u8), u64> = HashMap::new();
        let mut examples: Vec<String> = Vec::new();
        // For ref=plains mismatches: tie vs genuine-nearer, mean depth, mean gap
        let mut plains_tie: u64 = 0;
        let mut plains_genuine: u64 = 0;
        let mut plains_gap_sum: i64 = 0;
        let mut plains_miss_temp_sum: i64 = 0;
        let mut plains_miss_n: i64 = 0;
        // Per-axis contribution to the plains fitness gap (which axis loses plains)
        let mut axis_gap: [i64; 7] = [0; 7];
        let mut detail: Vec<String> = Vec::new();

        for (name, rx, rz) in regions {
            let region = read_region(&region_dir.join(name), rx, rz);
            for ((cx, cz), nbt) in &region {
                let bytes = bytes::Bytes::from(nbt.clone());
                let Ok(ref_chunk) = ChunkData::from_bytes(&bytes, Vector2::new(*cx, *cz)) else {
                    continue;
                };
                // Skip chunks MC hasn't populated biomes for yet
                use pumpkin_data::chunk::ChunkStatus;
                if (ref_chunk.status as usize) < (ChunkStatus::Biomes as usize) {
                    skipped_protochunks += 1;
                    continue;
                }
                let ref_proto = ProtoChunk::from_chunk_data(&ref_chunk, &world_gen);

                let mut gen_proto = ProtoChunk::new(*cx, *cz, &world_gen);
                let start_x = chunk_pos::start_block_x(*cx);
                let start_z = chunk_pos::start_block_z(*cz);
                let options = MultiNoiseSamplerBuilderOptions::new(
                    biome_coords::from_block(start_x),
                    biome_coords::from_block(start_z),
                    biome_coords::from_block(16) as usize,
                );
                let mut sampler =
                    MultiNoiseSampler::generate(&generator.base_router.multi_noise, &options);
                gen_proto.populate_biomes(generator, &mut sampler);

                chunks += 1;
                if chunks > 400 {
                    break;
                }
                // Full 3D column match
                for (&r, &g) in ref_proto
                    .flat_biome_map
                    .iter()
                    .zip(gen_proto.flat_biome_map.iter())
                {
                    total += 1;
                    if r == g {
                        matches += 1;
                    }
                }
                // Surface-band match (y=80, biome-y=20)
                let sbx = biome_coords::from_block(start_x);
                let sbz = biome_coords::from_block(start_z);
                // Fresh sampler for probing the climate vector at ref=plains mismatches
                let mut probe = MultiNoiseSampler::generate(
                    &generator.base_router.multi_noise,
                    &MultiNoiseSamplerBuilderOptions::new(sbx, sbz, biome_coords::from_block(16) as usize),
                );
                let plains_id = pumpkin_data::chunk::Biome::PLAINS.id;
                for bz in 0..4 {
                    for bx in 0..4 {
                        let r = ref_proto.get_biome_id(sbx + bx, 20, sbz + bz);
                        let g = gen_proto.get_biome_id(sbx + bx, 20, sbz + bz);
                        if r == plains_id && r != g {
                            let point = probe.sample(sbx + bx, 20, sbz + bz).convert_to_list();
                            let supplier = generator.datapack_biome_supplier.unwrap();
                            let (_best, best_dist, plains_dist) =
                                supplier.debug_nearest(&point, plains_id);
                            if plains_dist == best_dist {
                                plains_tie += 1;
                            } else {
                                plains_genuine += 1;
                                plains_gap_sum += plains_dist - best_dist;
                            }
                            let gap = supplier.debug_axis_gap(&point, plains_id);
                            for i in 0..7 {
                                axis_gap[i] += gap[i];
                            }
                            plains_miss_temp_sum += point[4]; // depth axis
                            plains_miss_n += 1;
                            if detail.len() < 8 {
                                detail.push(format!(
                                    "chunk({cx},{cz}) blk({},{}) biome_yz=({},20,{}) pt[T{} H{} C{} E{} D{} W{}] gen={}",
                                    start_x + bx * 4,
                                    start_z + bz * 4,
                                    sbx + bx,
                                    sbz + bz,
                                    point[0], point[1], point[2], point[3], point[4], point[5],
                                    name_of(g),
                                ));
                            }
                        }
                        surf_total += 1;
                        *ref_hist.entry(r).or_default() += 1;
                        *gen_hist.entry(g).or_default() += 1;
                        if r == g {
                            surf_match += 1;
                        } else {
                            *confusion.entry((r, g)).or_default() += 1;
                            if examples.len() < 20 {
                                examples.push(format!(
                                    "chunk({cx},{cz}) col({},{}): ref={} gen={}",
                                    start_x + bx * 4,
                                    start_z + bz * 4,
                                    name_of(r),
                                    name_of(g)
                                ));
                            }
                        }
                    }
                }
            }
            if chunks > 400 {
                break;
            }
        }

        let pct = |m: u64, t: u64| if t == 0 { 0.0 } else { 100.0 * m as f64 / t as f64 };
        eprintln!("=== GOLDEN-CHUNK BIOME PARITY ===");
        eprintln!(
            "biome supplier entries: {} (raw dimension biomes: {})",
            generator.datapack_biome_supplier.unwrap().len(),
            {
                // Raw entry count from the winning dimension file
                let guard = ACTIVE_WORLDGEN.read().unwrap();
                guard
                    .as_ref()
                    .and_then(|d| d.raw().get(Category::Dimension))
                    .and_then(|m| m.get("minecraft:overworld"))
                    .and_then(|js| serde_json::from_str::<codec::biome_source::DimensionFile>(js).ok())
                    .and_then(|df| df.generator.biome_source)
                    .map_or(0, |bs| bs.biomes.len())
            }
        );
        eprintln!("chunks compared: {chunks} (skipped {skipped_protochunks} un-biomed proto-chunks)");
        eprintln!("full 3D cells:  {matches}/{total} = {:.2}%", pct(matches, total));
        eprintln!(
            "surface (y=80): {surf_match}/{surf_total} = {:.2}%",
            pct(surf_match, surf_total)
        );

        let hist = |label: &str, h: &HashMap<u8, u64>| {
            let mut v: Vec<_> = h.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            eprintln!("--- {label} surface biome histogram (top 15) ---");
            for (id, c) in v.into_iter().take(15) {
                eprintln!("  {:>6}  {} ({:.1}%)", c, name_of(*id), pct(*c, surf_total));
            }
        };
        hist("REFERENCE", &ref_hist);
        hist("GENERATED", &gen_hist);

        let mut conf: Vec<_> = confusion.into_iter().collect();
        conf.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("top surface mismatches (ref -> gen : count):");
        for ((r, g), c) in conf.into_iter().take(20) {
            eprintln!("  {} -> {} : {c}", name_of(r), name_of(g));
        }
        eprintln!("examples:");
        for e in &examples {
            eprintln!("  {e}");
        }
        eprintln!("ref=plains mismatches ({plains_miss_n}):");
        eprintln!("  tie with plains (RTree/order could pick either): {plains_tie}");
        eprintln!("  plains genuinely farther (climate/sampling issue): {plains_genuine}");
        if plains_genuine > 0 {
            eprintln!(
                "    mean fitness gap (plains_dist - best_dist): {}",
                plains_gap_sum / plains_genuine as i64
            );
        }
        eprintln!("  sample misses (ranges = (lo,hi) x10000, order T,H,C,E,D,W,offset):");
        for d in &detail {
            eprintln!("    {d}");
        }
        if plains_miss_n > 0 {
            eprintln!("  mean depth axis at misses: {}", plains_miss_temp_sum / plains_miss_n);
            let names = ["temp", "humid", "cont", "erosion", "depth", "weird", "offset"];
            eprintln!("  per-axis fitness-gap share (what makes plains lose):");
            let total: i64 = axis_gap.iter().sum::<i64>().max(1);
            for i in 0..7 {
                eprintln!(
                    "    {:>7}: {} ({:.1}%)",
                    names[i],
                    axis_gap[i],
                    100.0 * axis_gap[i] as f64 / total as f64
                );
            }
        }

        clear_active_worldgen();
        clear_dynamic_biomes();
        assert!(total > 0, "no biome cells compared; check GOLDEN_WORLD_DIR");
    }

    /// Localizer
    #[test]
    #[ignore = "requires local datapacks; set GOLDEN_WORLD_DIR (or WORLDGEN_PACK_DIR)"]
    #[allow(clippy::print_stderr)]
    fn climate_axis_compare() {
        use crate::chunk::dynamic_biome::{DYNAMIC_BIOMES, clear_dynamic_biomes};
        use crate::generation::{
            Seed, biome_coords,
            generator::{GeneratorInit, VanillaGenerator},
            noise::router::multi_noise_sampler::{
                MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
            },
        };

        const SEED: i64 = -12969086726167675;

        let dir = std::env::var_os("GOLDEN_WORLD_DIR")
            .map(|w| PathBuf::from(w).join("datapacks"))
            .or_else(|| std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from));
        let Some(datapacks) = dir else {
            eprintln!("skipping: set GOLDEN_WORLD_DIR or WORLDGEN_PACK_DIR");
            return;
        };
        if !datapacks.is_dir() {
            eprintln!("skipping: {datapacks:?} not present");
            return;
        }

        clear_dynamic_biomes();
        {
            let mut reg = DYNAMIC_BIOMES.write().unwrap();
            reg.load_datapack_definitions(&[datapacks.clone()]);
            reg.register_datapack_biomes();
        }

        // Datapack router
        set_active_worldgen(WorldgenData::load(&[datapacks]));
        let dp = VanillaGenerator::new(Seed(SEED as u64), Dimension::OVERWORLD);
        assert!(dp.uses_datapack_terrain, "datapack terrain must be active");
        // Vanilla router (same seed)
        clear_active_worldgen();
        let van = VanillaGenerator::new(Seed(SEED as u64), Dimension::OVERWORLD);

        let sample_at = |router: &VanillaGenerator, bx: i32, bz: i32| {
            let opts = MultiNoiseSamplerBuilderOptions::new(bx, bz, 1);
            let mut s = MultiNoiseSampler::generate(&router.base_router.multi_noise, &opts);
            s.sample(bx, 16, bz)
        };

        eprintln!("=== CLIMATE AXIS COMPARE (datapack vs vanilla), quantized x10000 ===");
        eprintln!("blk(x,z)          | cont(dp/van)   temp(dp/van)   ero(dp/van)   depth(dp/van)  weird(dp/van)  humid(dp/van)");
        for (bxb, bzb) in [
            (0, 0), (64, 0), (0, 64), (128, 128), (256, 0), (0, 256),
            (512, 512), (-256, -256), (1024, 0), (200, -300),
        ] {
            let bx = biome_coords::from_block(bxb);
            let bz = biome_coords::from_block(bzb);
            let d = sample_at(&dp, bx, bz);
            let v = sample_at(&van, bx, bz);
            eprintln!(
                "blk({bxb:>5},{bzb:>5}) | {:>7}/{:<7} {:>6}/{:<6} {:>6}/{:<6} {:>6}/{:<6} {:>6}/{:<6} {:>6}/{:<6}",
                d.continentalness, v.continentalness,
                d.temperature, v.temperature,
                d.erosion, v.erosion,
                d.depth, v.depth,
                d.weirdness, v.weirdness,
                d.humidity, v.humidity,
            );
        }
        clear_dynamic_biomes();
    }

    /// Dump the raw JSON the scanner indexed for a set of density-function ids
    #[test]
    #[ignore = "requires local datapacks; set GOLDEN_WORLD_DIR (or WORLDGEN_PACK_DIR)"]
    #[allow(clippy::print_stderr)]
    fn dump_indexed_functions() {
        let dir = std::env::var_os("GOLDEN_WORLD_DIR")
            .map(|w| PathBuf::from(w).join("datapacks"))
            .or_else(|| std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from));
        let Some(datapacks) = dir else {
            eprintln!("skipping: set GOLDEN_WORLD_DIR or WORLDGEN_PACK_DIR");
            return;
        };
        if !datapacks.is_dir() {
            eprintln!("skipping: {datapacks:?} not present");
            return;
        }
        let data = WorldgenData::load(&[datapacks]);
        let raw = data.raw();

        eprintln!("=== indexed noise_settings/overworld ===");
        if let Some(js) = raw.get(Category::NoiseSettings).and_then(|m| m.get("minecraft:overworld")) {
            eprintln!("{}", &js[..js.len().min(200)]);
        }

        let ids = [
            "minecraft:overworld/noise_router/temperature",
            "minecraft:overworld/temperature",
            "tectonic:__constants/noise/temperature",
            "minecraft:overworld/noise_router/continents",
            "tectonic:__constants/noise/continents",
        ];
        eprintln!("\n=== indexed density_function JSON ===");
        for id in ids {
            match raw.get(Category::DensityFunction).and_then(|m| m.get(id)) {
                Some(js) => eprintln!("--- {id} ---\n{}", &js[..js.len().min(400)]),
                None => eprintln!("--- {id} --- (NOT INDEXED)"),
            }
        }

        eprintln!("\n=== a few indexed tectonic noise params ===");
        for id in [
            "minecraft:temperature",
            "minecraft:continentalness",
        ] {
            match raw.get(Category::Noise).and_then(|m| m.get(id)) {
                Some(js) => eprintln!("--- {id} ---\n{}", &js[..js.len().min(200)]),
                None => eprintln!("--- {id} --- (NOT INDEXED)"),
            }
        }
    }

    /// Probe intermediate density functions in tectonic's continents chain
    #[test]
    #[ignore = "requires local datapacks; set GOLDEN_WORLD_DIR (or WORLDGEN_PACK_DIR)"]
    #[allow(clippy::print_stderr)]
    fn probe_continents_chain() {
        use crate::generation::noise::router::multi_noise_sampler::{
            MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
        };
        use crate::generation::noise::router::proto_noise_router::ProtoNoiseRouters;
        use crate::generation::{GlobalRandomConfig, biome_coords};
        use codec::noise_settings::NoiseSettings;

        const SEED: i64 = -12969086726167675;

        let dir = std::env::var_os("GOLDEN_WORLD_DIR")
            .map(|w| PathBuf::from(w).join("datapacks"))
            .or_else(|| std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from));
        let Some(datapacks) = dir else {
            eprintln!("skipping: set GOLDEN_WORLD_DIR or WORLDGEN_PACK_DIR");
            return;
        };
        if !datapacks.is_dir() {
            return;
        }
        let data = WorldgenData::load(&[datapacks]);
        let registry = ParsedRegistry::from_raw(&data.raw);
        let random_config = GlobalRandomConfig::new(SEED as u64, false);

        // Build a minimal noise_settings whose `continents` slot is `probe`
        let sample_probe = |probe: &str, positions: &[(i32, i32)]| -> Vec<i64> {
            let json = format!(
                r#"{{
                    "noise": {{ "min_y": -64, "height": 384, "size_horizontal": 1, "size_vertical": 2 }},
                    "default_block": {{ "Name": "minecraft:stone" }},
                    "default_fluid": {{ "Name": "minecraft:water" }},
                    "sea_level": 63,
                    "noise_router": {{
                        "barrier": 0.0, "fluid_level_floodedness": 0.0, "fluid_level_spread": 0.0,
                        "lava": 0.0, "temperature": 0.0, "vegetation": 0.0,
                        "continents": {probe},
                        "erosion": 0.0, "depth": 0.0, "ridges": 0.0,
                        "preliminary_surface_level": 0.0, "final_density": 0.0,
                        "vein_toggle": 0.0, "vein_ridged": 0.0, "vein_gap": 0.0
                    }},
                    "surface_rule": {{ "type": "minecraft:block", "result_state": {{ "Name": "minecraft:stone" }} }}
                }}"#
            );
            let settings: NoiseSettings = serde_json::from_str(&json).expect("parse probe settings");
            let base = flatten::flatten_router(&settings.noise_router, &registry)
                .expect("probe router flattens");
            let proto = ProtoNoiseRouters::generate(&base, &random_config);
            positions
                .iter()
                .map(|&(bxb, bzb)| {
                    let bx = biome_coords::from_block(bxb);
                    let bz = biome_coords::from_block(bzb);
                    let opts = MultiNoiseSamplerBuilderOptions::new(bx, bz, 1);
                    let mut s = MultiNoiseSampler::generate(&proto.multi_noise, &opts);
                    s.sample(bx, 16, bz).continentalness
                })
                .collect()
        };

        let positions = [(0, 0), (64, 0), (128, 128), (-256, -256), (200, -300)];
        eprintln!("positions: {positions:?}");
        eprintln!("(values quantized x10000)\n");
        for probe in [
            "\"tectonic:__constants/noise/continents\"",
            "{ \"type\": \"minecraft:abs\", \"argument\": \"tectonic:__constants/noise/continents\" }",
            "\"tectonic:noise/raw_continents\"",
            "\"tectonic:island_selector\"",
            "\"tectonic:continent_selector\"",
            "\"tectonic:noise/raw_islands\"",
            "\"tectonic:noise/full_continents\"",
            "\"minecraft:overworld/noise_router/continents\"",
            // vanilla continents for reference
            "\"minecraft:overworld/continents\"",
        ] {
            let vals = sample_probe(probe, &positions);
            eprintln!("{:>55} -> {:?}", probe.replace('"', ""), vals);
        }
    }
}
