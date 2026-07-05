//! Datapack-driven world generation
pub mod codec;
mod source;

use std::path::PathBuf;

use tracing::{info, warn};

pub use source::{Category, RawWorldgen};

use codec::{density_function::DensityFunction, noise_settings::NoiseSettings};

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
    ///
    /// Returns `None` if the entry is absent or malformed (logged)
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
        // across all parsed density functions and noise-settings routers
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
        | F::Squeeze(a) => collect_unknown_kinds(a, out),
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
        F::Spline(spline) => collect_spline_unknown_kinds(spline, out),
        // Leaf / no-argument nodes.
        F::Noise { .. }
        | F::ShiftA(_)
        | F::ShiftB(_)
        | F::Shift(_)
        | F::YClampedGradient { .. }
        | F::Constant(_)
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
    }
}
