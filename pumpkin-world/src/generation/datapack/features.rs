//! Datapack-driven feature decoration registry

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;
use tracing::{info, warn};

use pumpkin_data::chunk::Biome;
use pumpkin_data::configured_feature::ConfiguredFeature as ConfiguredFeatureId;
use pumpkin_data::placed_feature::PlacedFeature as PlacedFeatureId;

use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;
use crate::generation::feature::configured_features::ConfiguredFeature;
use crate::generation::feature::indexed_features::IndexedFeatures;
use crate::generation::feature::placed_features::{
    Feature, PLACED_FEATURES, PlacedFeature, PlacedFeatureWrapper,
};

/// Stable `&'static PlacedFeature` identity used to test biome→feature membership
fn placed_feature_ptr(pf: &'static PlacedFeature) -> usize {
    std::ptr::from_ref::<PlacedFeature>(pf) as usize
}

/// Compile-time vanilla biome id → set of its placed-feature `&'static` pointers
static VANILLA_BIOME_FEATURE_PTRS: LazyLock<HashMap<u8, HashSet<usize>>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for id in 0u8..=u8::MAX {
        if let Some(biome) = Biome::from_id(id) {
            let set: HashSet<usize> = biome
                .features
                .iter()
                .flat_map(|step| step.iter())
                .filter_map(|e| PLACED_FEATURES.get(e))
                .map(placed_feature_ptr)
                .collect();
            map.insert(id, set);
        }
    }
    map
});

use super::codec::feature::{FeatureRefResolver, parse_configured_feature, parse_placed_feature};
use super::source::{Category, RawWorldgen};

/// Normalize bare id to namespaced resource location
fn normalize_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// Resolves feature references while parsing datapack features into leaked `'static` runtime values
struct FeatureResolver<'a> {
    configured: &'a HashMap<String, Value>,
    placed: &'a HashMap<String, Value>,
    /// Top-level placed-feature refs → leaked value
    memo: RefCell<HashMap<String, Option<&'static PlacedFeature>>>,
    /// Refs currently being resolved
    in_progress: RefCell<HashSet<String>>,
}

impl FeatureResolver<'_> {
    /// Resolve top-level placed-feature reference to stable `&'static PlacedFeature`
    #[allow(clippy::option_if_let_else)]
    fn resolve_placed(&self, id: &str) -> Option<&'static PlacedFeature> {
        if id.starts_with('#') {
            return None;
        }
        let key = normalize_id(id);
        if let Some(cached) = self.memo.borrow().get(&key) {
            return *cached;
        }
        let guard = format!("pf:{key}");
        if !self.in_progress.borrow_mut().insert(guard.clone()) {
            return None; // cycle
        }
        let result = if let Some(v) = self.placed.get(&key) {
            let parsed = parse_placed_feature(v, self);
            Some(&*Box::leak(Box::new(parsed)))
        } else if let Some(e) = PlacedFeatureId::from_name(&key) {
            PLACED_FEATURES.get(&e)
        } else {
            None
        };
        self.in_progress.borrow_mut().remove(&guard);
        self.memo.borrow_mut().insert(key, result);
        result
    }
}

/// No-op placed feature used as inert fallback for unresolvable inline references
fn empty_placed() -> PlacedFeature {
    PlacedFeature {
        feature: Feature::Inlined(Box::new(ConfiguredFeature::NoOp)),
        placement: Vec::new(),
    }
}

impl FeatureRefResolver for FeatureResolver<'_> {
    fn resolve_configured_ref(&self, id: &str) -> Feature {
        let key = normalize_id(id);
        let guard = format!("cf:{key}");
        if self.in_progress.borrow().contains(&guard) {
            return Feature::Inlined(Box::new(ConfiguredFeature::NoOp)); // cycle
        }
        if let Some(v) = self.configured.get(&key) {
            self.in_progress.borrow_mut().insert(guard.clone());
            let cf = parse_configured_feature(v, self);
            self.in_progress.borrow_mut().remove(&guard);
            return Feature::Inlined(Box::new(cf));
        }
        if let Some(e) = ConfiguredFeatureId::from_name(&key) {
            return Feature::Named(e);
        }
        Feature::Inlined(Box::new(ConfiguredFeature::NoOp))
    }

    fn resolve_placed_wrapper(&self, id: &str) -> PlacedFeatureWrapper {
        let key = normalize_id(id);
        if let Some(v) = self.placed.get(&key) {
            let guard = format!("pfw:{key}");
            if !self.in_progress.borrow_mut().insert(guard.clone()) {
                return PlacedFeatureWrapper::Direct(empty_placed()); // cycle
            }
            let parsed = parse_placed_feature(v, self);
            self.in_progress.borrow_mut().remove(&guard);
            return PlacedFeatureWrapper::Direct(parsed);
        }
        if let Some(e) = PlacedFeatureId::from_name(&key) {
            return PlacedFeatureWrapper::Named(e);
        }
        PlacedFeatureWrapper::Direct(empty_placed())
    }
}

/// Parse raw category (`id` → JSON text) into map of `id` → parsed `Value`
fn parse_value_map(raw: &RawWorldgen, category: Category) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    if let Some(entries) = raw.get(category) {
        for (id, json) in entries {
            match serde_json::from_str::<Value>(json) {
                Ok(v) => {
                    out.insert(id.clone(), v);
                }
                Err(e) => warn!("Malformed {} `{id}`: {e}", category.label()),
            }
        }
    }
    out
}

/// Datapack-driven per-biome decoration
pub struct DatapackFeatureRegistry {
    /// Global per-step feature ordering
    indexed: Arc<IndexedFeatures>,
    /// Datapac biome id → set of its placed-feature `&'static` pointers
    biome_feature_ptrs: HashMap<u8, HashSet<usize>>,
}

impl DatapackFeatureRegistry {
    /// Build registry from world's indexed worldgen JSON + currently registered datapack biome definitions
    #[must_use]
    pub fn build(raw: &RawWorldgen) -> Option<Self> {
        let biome_defs = DYNAMIC_BIOMES.read().unwrap().datapack_biome_defs_by_id();
        if biome_defs.iter().all(|(_, def)| def.feature_ids.is_empty()) {
            return None;
        }

        let configured = parse_value_map(raw, Category::ConfiguredFeature);
        let placed = parse_value_map(raw, Category::PlacedFeature);
        let resolver = FeatureResolver {
            configured: &configured,
            placed: &placed,
            memo: RefCell::new(HashMap::new()),
            in_progress: RefCell::new(HashSet::new()),
        };

        // Parsing untrusted datapack block states can panic in `Block::from_properties`
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let mut biome_features: HashMap<u8, Vec<Vec<&'static PlacedFeature>>> = HashMap::new();
        let mut placed_total = 0usize;
        let mut biomes_with_features = 0usize;
        for (id, def) in &biome_defs {
            if def.feature_ids.is_empty() {
                continue;
            }
            let steps: Vec<Vec<&'static PlacedFeature>> = def
                .feature_ids
                .iter()
                .map(|step| {
                    step.iter()
                        .filter_map(|ref_id| resolver.resolve_placed(ref_id))
                        .collect::<Vec<_>>()
                })
                .collect();
            placed_total += steps.iter().map(Vec::len).sum::<usize>();
            biomes_with_features += 1;
            biome_features.insert(*id, steps);
        }

        std::panic::set_hook(prev_hook);

        // Per-biome set of feature pointers
        let biome_feature_ptrs: HashMap<u8, HashSet<usize>> = biome_features
            .iter()
            .map(|(id, steps)| {
                let set = steps
                    .iter()
                    .flatten()
                    .map(|&pf| placed_feature_ptr(pf))
                    .collect::<HashSet<usize>>();
                (*id, set)
            })
            .collect();

        // Build global per-step feature ordering
        let mut ordered_sources: Vec<(u8, Vec<Vec<&'static PlacedFeature>>)> = Vec::new();
        for id in 0u8..=u8::MAX {
            if let Some(steps) = biome_features.get(&id) {
                ordered_sources.push((id, steps.clone()));
            } else if let Some(biome) = Biome::from_id(id) {
                let steps: Vec<Vec<&'static PlacedFeature>> = biome
                    .features
                    .iter()
                    .map(|step| step.iter().filter_map(|e| PLACED_FEATURES.get(e)).collect())
                    .collect();
                ordered_sources.push((id, steps));
            }
        }
        let indexed = Arc::new(IndexedFeatures::build(&ordered_sources));

        info!(
            "Datapack decoration: resolved {placed_total} placed feature(s) across \
             {biomes_with_features} datapack biome(s), {} global feature step(s) \
             ({} configured / {} placed feature defs indexed)",
            indexed.num_steps(),
            configured.len(),
            placed.len(),
        );
        Some(Self {
            indexed,
            biome_feature_ptrs,
        })
    }
}

/// Active world's datapack feature registry
static ACTIVE_FEATURES: RwLock<Option<DatapackFeatureRegistry>> = RwLock::new(None);

/// Lock-free mirror of `ACTIVE_FEATURES.is_some()`
static FEATURES_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Install datapack feature registry
pub fn set_active_features(registry: DatapackFeatureRegistry) {
    *ACTIVE_FEATURES.write().unwrap() = Some(registry);
    FEATURES_ACTIVE.store(true, Ordering::Release);
}

/// Clear active datapack feature registry
pub fn clear_active_features() {
    *ACTIVE_FEATURES.write().unwrap() = None;
    FEATURES_ACTIVE.store(false, Ordering::Release);
}

/// True if datapack-driven decoration is active for the current world
#[must_use]
pub fn has_active_features() -> bool {
    FEATURES_ACTIVE.load(Ordering::Acquire)
}

/// Does the biome `biome_id` list the placed feature identified by `feature_ptr`?
#[must_use]
pub fn biome_lists_feature_ptr(biome_id: u8, feature_ptr: usize) -> bool {
    if let Some(registry) = ACTIVE_FEATURES.read().unwrap().as_ref()
        && let Some(set) = registry.biome_feature_ptrs.get(&biome_id)
    {
        return set.contains(&feature_ptr);
    }
    VANILLA_BIOME_FEATURE_PTRS
        .get(&biome_id)
        .is_some_and(|set| set.contains(&feature_ptr))
}

/// True if any of `biome_ids` is a datapack biome carrying a feature list
#[must_use]
pub fn any_datapack_biome(biome_ids: &[u8]) -> bool {
    let guard = ACTIVE_FEATURES.read().unwrap();
    let Some(registry) = guard.as_ref() else {
        return false;
    };
    biome_ids
        .iter()
        .any(|id| registry.biome_feature_ptrs.contains_key(id))
}

/// Snapshot active global feature ordering for decoration loop
#[must_use]
pub fn snapshot_indexed_features() -> Option<Arc<IndexedFeatures>> {
    ACTIVE_FEATURES
        .read()
        .unwrap()
        .as_ref()
        .map(|registry| registry.indexed.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Index a real datapack, register its biomes, and build the decoration registry 
    /// Asserting many placed features resolve across biomes
    /// Ignored by default; set `WORLDGEN_PACK_DIR` to a datapacks folder and run with `--ignored --nocapture`
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn builds_registry_from_real_pack() {
        use crate::chunk::dynamic_biome::{DYNAMIC_BIOMES, clear_dynamic_biomes};

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        clear_dynamic_biomes();
        {
            let mut reg = DYNAMIC_BIOMES.write().unwrap();
            reg.load_datapack_definitions(std::slice::from_ref(&dir));
            reg.register_datapack_biomes()
        };
        let data = crate::generation::datapack::WorldgenData::load(&[dir]);
        let registry =
            DatapackFeatureRegistry::build(data.raw()).expect("registry should build for the pack");

        // Datapack biomes carrying a feature list
        let biomes = registry.biome_feature_ptrs.len();
        let modded_biomes = registry
            .biome_feature_ptrs
            .keys()
            .filter(|&&id| id >= 65)
            .count();
        // Total features across global per-step ordering
        let steps = registry.indexed.num_steps();
        let total: usize = (0..steps)
            .map(|step| {
                let mut n = 0;
                while registry.indexed.feature_at(step, n).is_some() {
                    n += 1;
                }
                n
            })
            .sum();
        eprintln!(
            "datapack feature registry: {biomes} datapack biomes with feature lists \
             ({modded_biomes} modded); global ordering has {total} features across {steps} steps"
        );

        assert!(biomes > 0, "expected biomes to carry datapack feature lists");
        assert!(
            total > 100,
            "expected many features in the global ordering, got {total}"
        );

        clear_dynamic_biomes();
    }

    /// `FeatureSorter` port respects per-biome feature ordering
    ///  and de-duplicates shared features into single global index per step
    #[test]
    fn indexed_features_orders_and_dedups() {
        use crate::generation::feature::placed_features::{Feature, PlacedFeature};

        fn leak(name: pumpkin_data::configured_feature::ConfiguredFeature) -> &'static PlacedFeature {
            Box::leak(Box::new(PlacedFeature {
                feature: Feature::Named(name),
                placement: Vec::new(),
            }))
        }
        let a = leak(ConfiguredFeatureId::Acacia);
        let b = leak(ConfiguredFeatureId::AmethystGeode);
        let c = leak(ConfiguredFeatureId::BasaltBlobs);

        // Biome 0: [a, b] at step 0; Biome 1: [b, c] at step 0
        // A valid global order is a, b, c (a before b before c)
        // b is shared → one index
        let sources = vec![
            (0u8, vec![vec![a, b]]),
            (1u8, vec![vec![b, c]]),
        ];
        let indexed = IndexedFeatures::build(&sources);
        let pa = std::ptr::from_ref::<PlacedFeature>(a) as usize;
        let pb = std::ptr::from_ref::<PlacedFeature>(b) as usize;
        let pc = std::ptr::from_ref::<PlacedFeature>(c) as usize;
        let ia = indexed.index_of(0, pa).unwrap();
        let ib = indexed.index_of(0, pb).unwrap();
        let ic = indexed.index_of(0, pc).unwrap();
        assert!(ia < ib && ib < ic, "expected a<b<c, got {ia},{ib},{ic}");
        // A chunk with both biomes runs all three, ascending by global index
        assert_eq!(indexed.step_feature_indices(&[0, 1], 0), vec![ia, ib, ic]);
        // A chunk with only biome 1 runs just b and c, still by their global index
        assert_eq!(indexed.step_feature_indices(&[1], 0), vec![ib, ic]);
    }
}
