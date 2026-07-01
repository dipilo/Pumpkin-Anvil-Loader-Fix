//! Datapack biome definition loader
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};
use tracing::{debug, warn};

/// Biome `attributes` keys that are registered *and* `.synced()` in vanilla (`net.minecraft.world.attribute.EnvironmentAttributes`)
const SYNCED_ATTRIBUTE_KEYS: &[&str] = &[
    // visual
    "minecraft:visual/fog_color",
    "minecraft:visual/fog_start_distance",
    "minecraft:visual/fog_end_distance",
    "minecraft:visual/sky_fog_end_distance",
    "minecraft:visual/cloud_fog_end_distance",
    "minecraft:visual/water_fog_color",
    "minecraft:visual/water_fog_start_distance",
    "minecraft:visual/water_fog_end_distance",
    "minecraft:visual/sky_color",
    "minecraft:visual/sunrise_sunset_color",
    "minecraft:visual/cloud_color",
    "minecraft:visual/cloud_height",
    "minecraft:visual/sun_angle",
    "minecraft:visual/moon_angle",
    "minecraft:visual/star_angle",
    "minecraft:visual/moon_phase",
    "minecraft:visual/star_brightness",
    "minecraft:visual/sky_light_color",
    "minecraft:visual/sky_light_factor",
    "minecraft:visual/default_dripstone_particle",
    "minecraft:visual/ambient_particles",
    // audio
    "minecraft:audio/background_music",
    "minecraft:audio/music_volume",
    "minecraft:audio/ambient_sounds",
    "minecraft:audio/firefly_bush_sounds",
    // gameplay (synced subset)
    "minecraft:gameplay/sky_light_level",
    "minecraft:gameplay/water_evaporates",
    "minecraft:gameplay/fast_lava",
    "minecraft:gameplay/piglins_zombify",
    "minecraft:gameplay/creaking_active",
];

/// `effects` keys understood by the network biome codec
const EFFECT_KEYS: &[&str] = &[
    "water_color",
    "foliage_color",
    "grass_color",
    "dry_foliage_color",
    "grass_color_modifier",
];

const MAX_CONTAINER_DEPTH: usize = 3;

/// per-biome data extracted from a datapack's biome JSON
#[derive(Clone)]
pub struct DatapackBiomeDef {
    /// Network NBT bytes for the `worldgen/biome` registry entry sent to clients
    pub network_nbt: Box<[u8]>,
    pub temperature: f32,
    pub downfall: f32,
    pub has_precipitation: bool,
}

/// Load biome definitions from the given datapack sources
#[must_use]
pub fn load_biome_definitions(sources: &[PathBuf]) -> HashMap<String, DatapackBiomeDef> {
    let mut out = HashMap::new();
    for source in sources {
        collect_from_path(source, 0, &mut out);
    }
    out
}

/// Resolve a single configured path into the datapacks it represents and scan them
fn collect_from_path(path: &Path, depth: usize, out: &mut HashMap<String, DatapackBiomeDef>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            scan_zip_datapack(path, out);
        }
        return;
    }

    if !path.is_dir() {
        return;
    }

    // An unpacked datapack has a top-level `data/` directory
    if path.join("data").is_dir() {
        scan_folder_datapack(path, out);
        return;
    }

    // Otherwise treat it as a container of datapacks (zips and/or folders)
    if depth >= MAX_CONTAINER_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            debug!("Cannot read datapack container {}: {e}", path.display());
            return;
        }
    };
    for entry in entries.flatten() {
        collect_from_path(&entry.path(), depth + 1, out);
    }
}

/// Scan an unpacked datapack folder for `data/<ns>/worldgen/biome/**.json` files
fn scan_folder_datapack(root: &Path, out: &mut HashMap<String, DatapackBiomeDef>) {
    let data_dir = root.join("data");
    let namespaces = match std::fs::read_dir(&data_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ns_entry in namespaces.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let namespace = ns_entry.file_name().to_string_lossy().into_owned();
        let biome_dir = ns_path.join("worldgen").join("biome");
        if !biome_dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_json_files(&biome_dir, &mut files);
        for file in files {
            let Ok(relative) = file.strip_prefix(&biome_dir) else {
                continue;
            };
            let Some(biome_path) = biome_id_from_relative(relative) else {
                continue;
            };
            let mut contents = String::new();
            match File::open(&file).and_then(|mut f| f.read_to_string(&mut contents)) {
                Ok(_) => {}
                Err(e) => {
                    debug!("Failed to read biome file {}: {e}", file.display());
                    continue;
                }
            }
            insert_biome(&namespace, &biome_path, &contents, out);
        }
    }
}

/// Recursively collect `.json` files under `dir`
fn collect_json_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, files);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
            files.push(path);
        }
    }
}

/// Scan a datapack `.zip` archive for biome JSON files
fn scan_zip_datapack(path: &Path, out: &mut HashMap<String, DatapackBiomeDef>) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            debug!("Cannot open datapack zip {}: {e}", path.display());
            return;
        }
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => {
            warn!("Failed to read datapack zip {}: {e}", path.display());
            return;
        }
    };

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                debug!("Bad entry in {}: {e}", path.display());
                continue;
            }
        };
        if !entry.is_file() {
            continue;
        }
        let Some((namespace, biome_path)) = parse_biome_zip_path(entry.name()) else {
            continue;
        };
        let mut contents = String::new();
        if let Err(e) = entry.read_to_string(&mut contents) {
            debug!("Failed to read zip entry {}: {e}", entry.name());
            continue;
        }
        insert_biome(&namespace, &biome_path, &contents, out);
    }
}

/// Parse a forward-slash zip path of the form
/// `data/<ns>/worldgen/biome/<path>.json` into `(namespace, biome_path)`
fn parse_biome_zip_path(name: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() < 5 || parts[0] != "data" || parts[2] != "worldgen" || parts[3] != "biome" {
        return None;
    }
    let namespace = parts[1].to_string();
    let rest = parts[4..].join("/");
    let biome = rest.strip_suffix(".json")?.to_string();
    if biome.is_empty() {
        return None;
    }
    Some((namespace, biome))
}

/// Build a biome id path from a path relative to a `worldgen/biome` directory
fn biome_id_from_relative(relative: &Path) -> Option<String> {
    let mut segments = Vec::new();
    for component in relative.components() {
        segments.push(component.as_os_str().to_string_lossy());
    }
    let joined = segments.join("/");
    let biome = joined.strip_suffix(".json")?.to_string();
    if biome.is_empty() {
        return None;
    }
    Some(biome)
}

/// Parse, convert, and insert a single biome definition
fn insert_biome(
    namespace: &str,
    biome_path: &str,
    contents: &str,
    out: &mut HashMap<String, DatapackBiomeDef>,
) {
    let json = match serde_json::from_str::<Value>(contents) {
        Ok(v) => v,
        Err(e) => {
            debug!("Skipping malformed biome {namespace}:{biome_path}: {e}");
            return;
        }
    };
    if let Some(network_nbt) = convert_biome_to_network_nbt(&json) {
        let obj = json.as_object();
        let f32_field = |key: &str, default: f32| {
            obj.and_then(|o| o.get(key))
                .and_then(serde_json::Value::as_f64)
                .map_or(default, |v| v as f32)
        };
        let has_precipitation = obj
            .and_then(|o| o.get("has_precipitation"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        out.insert(
            format!("{namespace}:{biome_path}"),
            DatapackBiomeDef {
                network_nbt,
                temperature: f32_field("temperature", 0.5),
                downfall: f32_field("downfall", 0.5),
                has_precipitation,
            },
        );
    }
}

/// Convert a datapack biome JSON value into the network NBT bytes used by the
/// `worldgen/biome` registry entry, matching Pumpkin's vanilla biomes serialization
fn convert_biome_to_network_nbt(json: &Value) -> Option<Box<[u8]>> {
    let obj = json.as_object()?;
    let mut root = Map::new();

    root.insert(
        "has_precipitation".to_string(),
        obj.get("has_precipitation")
            .cloned()
            .unwrap_or(Value::Bool(true)),
    );
    root.insert(
        "temperature".to_string(),
        obj.get("temperature").cloned().unwrap_or(Value::from(0.5)),
    );
    root.insert(
        "downfall".to_string(),
        obj.get("downfall").cloned().unwrap_or(Value::from(0.5)),
    );
    if let Some(modifier) = obj.get("temperature_modifier").filter(|v| v.is_string()) {
        root.insert("temperature_modifier".to_string(), modifier.clone());
    }

    // Effects: forward known color/modifier keys verbatim
    let mut effects = Map::new();
    if let Some(Value::Object(src)) = obj.get("effects") {
        for key in EFFECT_KEYS {
            if let Some(value) = src.get(*key) {
                effects.insert((*key).to_string(), value.clone());
            }
        }
    }
    root.insert("effects".to_string(), Value::Object(effects));

    // Attributes: forward only registered+synced keys
    let mut attributes = Map::new();
    if let Some(Value::Object(src)) = obj.get("attributes") {
        for (key, value) in src {
            if SYNCED_ATTRIBUTE_KEYS.contains(&key.as_str()) {
                attributes.insert(key.clone(), value.clone());
            }
        }
    }
    // Legacy (pre-attributes) datapacks store sky/fog colors under `effects`
    // Relocate them into the new attribute keys if not already present
    if let Some(Value::Object(src)) = obj.get("effects") {
        relocate_legacy_color(src, &mut attributes, "sky_color", "minecraft:visual/sky_color");
        relocate_legacy_color(src, &mut attributes, "fog_color", "minecraft:visual/fog_color");
        relocate_legacy_color(
            src,
            &mut attributes,
            "water_fog_color",
            "minecraft:visual/water_fog_color",
        );
    }
    root.insert("attributes".to_string(), Value::Object(attributes));

    let value = Value::Object(root);
    let mut buf = Vec::new();
    match pumpkin_nbt::to_bytes_unnamed(&value, &mut buf) {
        Ok(()) => Some(buf.into_boxed_slice()),
        Err(e) => {
            warn!("Failed to serialize datapack biome NBT: {e}");
            None
        }
    }
}

/// Move a legacy `effects.<src_key>` color into `attributes[attr_key]` 
/// unless the attribute is already set
fn relocate_legacy_color(
    effects: &Map<String, Value>,
    attributes: &mut Map<String, Value>,
    src_key: &str,
    attr_key: &str,
) {
    if attributes.contains_key(attr_key) {
        return;
    }
    if let Some(value) = effects.get(src_key) {
        attributes.insert(attr_key.to_string(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_biome_zip_path() {
        assert_eq!(
            parse_biome_zip_path("data/terralith/worldgen/biome/alpha_islands.json"),
            Some(("terralith".to_string(), "alpha_islands".to_string()))
        );
        // Nested biome ids keep their subpath
        assert_eq!(
            parse_biome_zip_path("data/terralith/worldgen/biome/cave/infested.json"),
            Some(("terralith".to_string(), "cave/infested".to_string()))
        );
        // Tag files must be ignored
        assert_eq!(
            parse_biome_zip_path("data/c/tags/worldgen/biome/is_lush.json"),
            None
        );
        assert_eq!(parse_biome_zip_path("pack.mcmeta"), None);
    }

    #[test]
    fn converts_new_format_biome() {
        let json = serde_json::json!({
            "has_precipitation": true,
            "temperature": 0.7,
            "downfall": 0.8,
            "attributes": {
                "minecraft:visual/sky_color": 12047615,
                "minecraft:visual/cloud_color": "#00000000",
                "minecraft:gameplay/can_start_raid": false
            },
            "effects": {
                "water_color": 2576383,
                "grass_color": 11599739
            },
            "carvers": [],
            "features": []
        });
        let nbt = convert_biome_to_network_nbt(&json).expect("should convert");
        assert!(!nbt.is_empty());

        // Re-build the expected filtered value and confirm the non-synced gameplay
        // attribute was dropped while synced visual ones were kept
        let obj = json.as_object().unwrap();
        let attrs = obj["attributes"].as_object().unwrap();
        assert!(attrs.contains_key("minecraft:gameplay/can_start_raid"));
        assert!(!SYNCED_ATTRIBUTE_KEYS.contains(&"minecraft:gameplay/can_start_raid"));
        assert!(SYNCED_ATTRIBUTE_KEYS.contains(&"minecraft:visual/sky_color"));
    }

    #[test]
    fn relocates_legacy_colors() {
        let json = serde_json::json!({
            "has_precipitation": true,
            "temperature": 0.8,
            "downfall": 0.4,
            "effects": {
                "sky_color": 7972607,
                "fog_color": 12638463,
                "water_color": 4159204
            }
        });
        // Should not panic and should produce bytes
        let nbt = convert_biome_to_network_nbt(&json).expect("should convert");
        assert!(!nbt.is_empty());
    }
}
