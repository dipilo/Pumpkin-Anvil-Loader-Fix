//! Datapack-driven carver registry

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use serde_json::Value;
use tracing::{info, warn};

use pumpkin_data::carver::{CANYON, CAVE, CAVE_EXTRA_UNDERGROUND, CarverConfig, NETHER_CAVE};

use crate::chunk::dynamic_biome::DYNAMIC_BIOMES;

use super::codec::carver::parse_configured_carver;
use super::source::{Category, RawWorldgen};

/// Normalize bare id to namespaced resource location
fn normalize_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

/// Compile-time vanilla carver of a given id, if any
fn vanilla_carver(id: &str) -> Option<&'static CarverConfig> {
    match id {
        "minecraft:cave" => Some(&CAVE),
        "minecraft:cave_extra_underground" => Some(&CAVE_EXTRA_UNDERGROUND),
        "minecraft:canyon" => Some(&CANYON),
        "minecraft:nether_cave" => Some(&NETHER_CAVE),
        _ => None,
    }
}

/// Datapack-driven per-biome carver lists, keyed by biome id
pub struct DatapackCarverRegistry {
    biome_carvers: HashMap<u8, Vec<&'static CarverConfig>>,
}

impl DatapackCarverRegistry {
    /// Build from world's indexed worldgen JSON + registered datapack biome definitions
    #[must_use]
    pub fn build(raw: &RawWorldgen) -> Option<Self> {
        let biome_defs = DYNAMIC_BIOMES.read().unwrap().datapack_biome_defs_by_id();
        if biome_defs.iter().all(|(_, def)| def.carver_ids.is_empty()) {
            return None;
        }

        let configured = parse_value_map(raw, Category::ConfiguredCarver);

        // Datapack carver defs parsed + leaked once
        let mut memo: HashMap<String, Option<&'static CarverConfig>> = HashMap::new();
        let mut resolve = |id: &str| -> Option<&'static CarverConfig> {
            if id.starts_with('#') {
                return None; // carver tags unsupported
            }
            let key = normalize_id(id);
            if let Some(cached) = memo.get(&key) {
                return *cached;
            }
            let resolved = configured.get(&key).map_or_else(
                || vanilla_carver(&key),
                |v| parse_configured_carver(v).map(|c| &*Box::leak(Box::new(c))),
            );
            memo.insert(key, resolved);
            resolved
        };

        // Parsing untrusted datapack block states/props can panic; silence the hook
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let mut biome_carvers: HashMap<u8, Vec<&'static CarverConfig>> = HashMap::new();
        let mut total = 0usize;
        for (id, def) in &biome_defs {
            if def.carver_ids.is_empty() {
                continue;
            }
            let carvers: Vec<&'static CarverConfig> = def
                .carver_ids
                .iter()
                .filter_map(|ref_id| resolve(ref_id))
                .collect();
            total += carvers.len();
            biome_carvers.insert(*id, carvers);
        }

        std::panic::set_hook(prev_hook);

        info!(
            "Datapack carvers: resolved {total} carver(s) across {} biome(s) \
             ({} configured carver def(s) indexed)",
            biome_carvers.len(),
            configured.len(),
        );
        if biome_carvers.is_empty() {
            return None;
        }
        Some(Self { biome_carvers })
    }
}

/// Parse a raw category (`id` → JSON text) into a map of `id` → parsed `Value`
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

/// Active world's datapack carver registry
static ACTIVE_CARVERS: RwLock<Option<DatapackCarverRegistry>> = RwLock::new(None);

/// Lock-free mirror of `ACTIVE_CARVERS.is_some()` for the carve stage
static CARVERS_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Install datapack carver registry
pub fn set_active_carvers(registry: DatapackCarverRegistry) {
    *ACTIVE_CARVERS.write().unwrap() = Some(registry);
    CARVERS_ACTIVE.store(true, Ordering::Release);
}

/// Clear active datapack carver registry
pub fn clear_active_carvers() {
    *ACTIVE_CARVERS.write().unwrap() = None;
    CARVERS_ACTIVE.store(false, Ordering::Release);
}

/// True if datapack-driven carving is active for the current world
#[must_use]
pub fn has_active_carvers() -> bool {
    CARVERS_ACTIVE.load(Ordering::Acquire)
}

/// The datapack carver list for a biome id, if datapack-driven
#[must_use]
pub fn biome_carvers(biome_id: u8) -> Option<Vec<&'static CarverConfig>> {
    let guard = ACTIVE_CARVERS.read().unwrap();
    let registry = guard.as_ref()?;
    registry.biome_carvers.get(&biome_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Index real datapack, register its biomes, and build the carver registry, asserting carvers resolve across biomes 
    /// Ignored by default; set `WORLDGEN_PACK_DIR` to a datapacks folder and run with `--ignored --nocapture`
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr)]
    fn builds_carver_registry_from_real_pack() {
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
            DatapackCarverRegistry::build(data.raw()).expect("carver registry should build");

        let biomes = registry.biome_carvers.len();
        let total: usize = registry.biome_carvers.values().map(Vec::len).sum();
        eprintln!("carver registry: {biomes} biomes with carver lists, {total} carvers resolved");
        assert!(biomes > 0, "expected biomes to carry carver lists");
        assert!(total > 0, "expected carvers to resolve");

        clear_dynamic_biomes();
    }
}
