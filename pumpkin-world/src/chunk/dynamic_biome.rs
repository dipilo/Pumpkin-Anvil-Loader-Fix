use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{LazyLock, RwLock},
};

use pumpkin_data::chunk::Biome;
use pumpkin_util::resource_location::ResourceLocation;
use rayon::prelude::*;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use super::datapack_biome::{self, DatapackBiomeDef};

const MAX_DYNAMIC_BIOME_ID: u8 = 239;

/// The first internal ID handed out to dynamic (datapack) biomes
fn dynamic_biome_start() -> u8 {
    // Count the contiguous run of valid vanilla IDs starting at 0
    (0u16..=u16::from(u8::MAX))
        .take_while(|&id| Biome::from_id(id as u8).is_some())
        .count() as u8
}

/// Runtime registry for modded/datapack biomes not known at compile time
pub static DYNAMIC_BIOMES: LazyLock<RwLock<DynamicBiomeRegistry>> =
    LazyLock::new(|| RwLock::new(DynamicBiomeRegistry::new()));

pub struct DynamicBiomeRegistry {
    name_to_id: HashMap<String, u8>,
    id_to_entry: HashMap<u8, DynamicBiomeEntry>,
    definitions: HashMap<String, DatapackBiomeDef>,
    next_id: u8,
}

#[derive(Clone, Debug)]
pub struct DynamicBiomeEntry {
    pub name: String,
    pub data: Box<[u8]>,
    pub gen_biome: &'static Biome,
}

impl DynamicBiomeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            name_to_id: HashMap::new(),
            id_to_entry: HashMap::new(),
            definitions: HashMap::new(),
            next_id: dynamic_biome_start(),
        }
    }

    /// Load biome definitions from the given datapack sources
    pub fn load_datapack_definitions(&mut self, sources: &[PathBuf]) {
        let definitions = datapack_biome::load_biome_definitions(sources);
        if definitions.is_empty() {
            debug!("No datapack biome definitions found in configured sources");
        } else {
            info!(
                "Loaded {} biome definition(s) from datapacks",
                definitions.len()
            );
        }
        self.definitions = definitions;
    }

    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<u8> {
        self.name_to_id.get(name).copied()
    }

    pub fn register(&mut self, name: &str) -> Option<u8> {
        // Already registered?
        if let Some(&id) = self.name_to_id.get(name) {
            return Some(id);
        }

        // Out of IDs?
        if self.next_id > MAX_DYNAMIC_BIOME_ID {
            warn!(
                "Dynamic biome registry full! Cannot register: {name}. \
                 Consider regenerating biome codegen or increasing MAX_DYNAMIC_BIOME_ID."
            );
            return None;
        }

        let id = self.next_id;
        self.next_id += 1;

        // Build biome NBT data for the client registry
        let (data, gen_biome, source) = self.definitions.get(name).map_or_else(
            || {
                (
                    build_biome_nbt(name),
                    pick_generation_biome(name, None),
                    "heuristic",
                )
            },
            |def| {
                (
                    def.network_nbt.clone(),
                    pick_generation_biome(name, Some(def)),
                    "datapack",
                )
            },
        );

        info!(
            "Registered dynamic biome '{name}' -> internal ID {id} ({source}, \
             generates as '{}')",
            gen_biome.registry_id
        );

        self.name_to_id.insert(name.to_string(), id);
        self.id_to_entry.insert(
            id,
            DynamicBiomeEntry {
                name: name.to_string(),
                data,
                gen_biome,
            },
        );

        Some(id)
    }

    /// Register a modded biome if it's not a vanilla biome
    pub fn register_if_modded(&mut self, name: &str) -> Option<u8> {
        let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
        if pumpkin_data::chunk::Biome::from_name(stripped).is_some() {
            return None;
        }
        self.register(name)
    }

    /// Get all registered dynamic biome entries as protocol registry entries
    /// for the client configuration phase
    #[must_use]
    pub fn get_registry_entries(&self) -> Vec<(ResourceLocation, Box<[u8]>)> {
        let mut entries: Vec<(u8, &DynamicBiomeEntry)> =
            self.id_to_entry.iter().map(|(&id, entry)| (id, entry)).collect();
        entries.sort_unstable_by_key(|(id, _)| *id);
        entries
            .into_iter()
            .map(|(_, entry)| (entry.name.clone(), entry.data.clone()))
            .collect()
    }

    /// Returns true if any dynamic biomes have been registered
    #[must_use]
    pub fn has_entries(&self) -> bool {
        !self.name_to_id.is_empty()
    }

    /// Names of all registered dynamic  biomes
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.name_to_id.keys().cloned().collect()
    }

    /// Get the count of registered dynamic biomes
    #[must_use]
    pub fn len(&self) -> usize {
        self.name_to_id.len()
    }

    /// Returns true if no dynamic biomes are registered
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name_to_id.is_empty()
    }

    /// Pre-register a list of biome names (from world pre-scan)
    pub fn preload_biomes(&mut self, names: &[String]) {
        for name in names {
            let _ = self.register_if_modded(name);
        }
    }

    /// Register every modded biome defined by the loaded datapacks
    pub fn register_datapack_biomes(&mut self) -> usize {
        let mut names: Vec<String> = self.definitions.keys().cloned().collect();
        names.sort_unstable();
        let before = self.name_to_id.len();
        for name in names {
            let _ = self.register_if_modded(&name);
        }
        self.name_to_id.len() - before
    }

    /// True if the world's datapacks provided any biome definitions
    #[must_use]
    pub fn has_definitions(&self) -> bool {
        !self.definitions.is_empty()
    }

    #[must_use]
    pub fn datapack_biome_defs_by_id(&self) -> Vec<(u8, DatapackBiomeDef)> {
        self.definitions
            .iter()
            .filter_map(|(name, def)| {
                let id = self.name_to_id.get(name).copied().or_else(|| {
                    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
                    Biome::from_name(stripped).map(|b| b.id)
                })?;
                Some((id, def.clone()))
            })
            .collect()
    }
}

impl Default for DynamicBiomeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Reset the dynamic biome registry
pub fn clear_dynamic_biomes() {
    let mut registry = DYNAMIC_BIOMES.write().unwrap();
    registry.name_to_id.clear();
    registry.id_to_entry.clear();
    registry.definitions.clear();
    registry.next_id = dynamic_biome_start();
    info!("Dynamic biome registry cleared");
}

/// Resolve any biome ID to a concrete `&'static Biome`
#[must_use]
pub fn resolve_biome(id: u8) -> &'static Biome {
    if let Some(biome) = Biome::from_id(id) {
        return biome;
    }
    DYNAMIC_BIOMES
        .read()
        .unwrap()
        .id_to_entry
        .get(&id)
        .map_or(&Biome::PLAINS, |entry| entry.gen_biome)
}

fn pick_generation_biome(name: &str, def: Option<&DatapackBiomeDef>) -> &'static Biome {
    let lower = name.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    // Cave biomes never reach the surface; map by keyword
    if has(&["cave", "underground", "mantle", "tunnel"]) {
        return if has(&["lush", "jungle", "tropical"]) {
            &Biome::LUSH_CAVES
        } else if has(&["deep_dark", "sculk", "warden"]) {
            &Biome::DEEP_DARK
        } else {
            &Biome::DRIPSTONE_CAVES
        };
    }

    if has(&["ocean", "sea"]) {
        return if has(&["frozen", "icy"]) {
            &Biome::FROZEN_OCEAN
        } else {
            &Biome::OCEAN
        };
    }
    if has(&["beach", "shore", "coast"]) {
        return &Biome::BEACH;
    }
    if has(&["mangrove"]) {
        return &Biome::MANGROVE_SWAMP;
    }
    if has(&["swamp", "bog", "marsh", "wetland", "bayou"]) {
        return &Biome::SWAMP;
    }
    if has(&["badlands", "mesa"]) {
        return &Biome::BADLANDS;
    }

    let temperature = def.map_or(0.5, |d| d.temperature);
    let downfall = def.map_or(0.5, |d| d.downfall);
    let has_precipitation = def.is_none_or(|d| d.has_precipitation);

    let mountainous = has(&["peak", "mountain", "cliff", "highland", "summit", "alps"]);

    // Cold
    if temperature < 0.15 || has(&["frozen", "snow", "ice", "glacier", "tundra", "arctic"]) {
        if mountainous {
            return &Biome::FROZEN_PEAKS;
        }
        return if downfall > 0.5 {
            &Biome::SNOWY_TAIGA
        } else {
            &Biome::SNOWY_PLAINS
        };
    }

    // Hot
    if temperature >= 1.5 || (!has_precipitation && downfall < 0.1 && temperature >= 1.0) {
        if has(&["savanna", "shrubland", "steppe", "brushland"]) {
            return &Biome::SAVANNA;
        }
        if has(&["jungle", "rainforest", "tropical"]) {
            return &Biome::JUNGLE;
        }
        return &Biome::DESERT;
    }

    // Warm/temperate
    if temperature >= 0.9 {
        if has(&["jungle", "rainforest", "tropical"]) {
            return if downfall < 0.6 {
                &Biome::SPARSE_JUNGLE
            } else {
                &Biome::JUNGLE
            };
        }
        if has(&["savanna", "shrubland", "steppe", "brushland"]) {
            return &Biome::SAVANNA;
        }
    }

    if mountainous {
        return if temperature < 0.3 {
            &Biome::GROVE
        } else if downfall < 0.3 {
            &Biome::STONY_PEAKS
        } else {
            &Biome::WINDSWEPT_HILLS
        };
    }

    // Temperate land: pick by moisture / forest signals
    if has(&["dark_forest", "dark forest"]) {
        return &Biome::DARK_FOREST;
    }
    if has(&["taiga", "spruce", "pine", "conifer"]) {
        return &Biome::TAIGA;
    }
    if has(&["meadow", "alpine", "valley", "clearing"]) {
        return &Biome::MEADOW;
    }
    if has(&["forest", "wood", "grove"]) || downfall >= 0.6 {
        return &Biome::FOREST;
    }

    &Biome::PLAINS
}

/// Build best-effort biome NBT for a biome with no datapack definition
fn build_biome_nbt(name: &str) -> Box<[u8]> {
    let lower = name.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    let (has_precipitation, temperature, downfall) =
        if has(&["desert", "sands", "oasis", "badlands", "mesa"]) {
            (false, 2.0f32, 0.0f32)
        } else if has(&["savanna", "shrubland", "steppe", "brushland"]) {
            (false, 1.2f32, 0.0f32)
        } else if has(&["jungle", "tropical", "rainforest", "kelp"]) {
            (true, 0.95f32, 0.9f32)
        } else if has(&["swamp", "bog", "marsh", "wetland"]) {
            (true, 0.8f32, 0.9f32)
        } else if has(&["snow", "frozen", "ice", "glacier", "tundra", "siberian"]) {
            (true, -0.5f32, 0.4f32)
        } else if has(&["mountain", "peak", "cliff", "highland", "canyon", "yosemite"]) {
            (true, 0.2f32, 0.3f32)
        } else if has(&["ocean", "sea"]) {
            (true, 0.5f32, 0.5f32)
        } else if has(&["beach", "shore", "cave", "underground"]) {
            (true, 0.8f32, 0.4f32)
        } else if has(&["inferno", "volcanic", "crater"]) {
            (false, 2.0f32, 0.0f32)
        } else {
            (true, 0.8f32, 0.4f32)
        };

    // (sky_color, fog_color, water_fog_color, water_color, foliage_color, grass_color)
    let (sky, fog, water_fog, water, foliage, grass): (
        i32,
        i32,
        i32,
        i32,
        Option<i32>,
        Option<i32>,
    ) = if has(&["desert", "sands", "oasis", "badlands", "mesa"]) {
        (0x6EB1FF, 0xC0D8FF, 0x3F76E4, 0x3F76E4, None, Some(0x90814D))
    } else if has(&["jungle", "tropical", "rainforest"]) {
        (0x77A8FF, 0xC0D8FF, 0x3F76E4, 0x3F76E4, None, Some(0x59C93C))
    } else if has(&["swamp", "bog", "marsh"]) {
        (0x78A7FF, 0xC0D8FF, 0x232317, 0x617B64, None, Some(0x6A7039))
    } else if has(&["snow", "frozen", "ice", "glacier", "tundra"]) {
        (0x80A4FF, 0xC0D8FF, 0x3F76E4, 0x3F76E4, None, Some(0x80B497))
    } else if has(&["cherry", "sakura"]) {
        (0x7BA4FF, 0xC0D8FF, 0x5DB7EF, 0x5DB7EF, Some(0xB6DB61), Some(0xB6DB61))
    } else if has(&["dark", "mushroom"]) {
        (0x78A7FF, 0xC0D8FF, 0x3F76E4, 0x3F76E4, Some(0x59AE30), Some(0x59AE30))
    } else if has(&["inferno", "volcanic"]) {
        (0x6C655B, 0x685F70, 0x3F76E4, 0x3F76E4, None, Some(0x5A4D40))
    } else {
        (0x78A7FF, 0xC0D8FF, 0x3F76E4, 0x3F76E4, None, None)
    };

    let mut attributes = Map::new();
    attributes.insert("minecraft:visual/sky_color".to_string(), Value::from(sky));
    attributes.insert("minecraft:visual/fog_color".to_string(), Value::from(fog));
    attributes.insert(
        "minecraft:visual/water_fog_color".to_string(),
        Value::from(water_fog),
    );

    let mut effects = Map::new();
    effects.insert("water_color".to_string(), Value::from(water));
    if let Some(foliage) = foliage {
        effects.insert("foliage_color".to_string(), Value::from(foliage));
    }
    if let Some(grass) = grass {
        effects.insert("grass_color".to_string(), Value::from(grass));
    }

    let mut root = Map::new();
    root.insert(
        "has_precipitation".to_string(),
        Value::Bool(has_precipitation),
    );
    root.insert("temperature".to_string(), Value::from(temperature));
    root.insert("downfall".to_string(), Value::from(downfall));
    root.insert("attributes".to_string(), Value::Object(attributes));
    root.insert("effects".to_string(), Value::Object(effects));

    let value = Value::Object(root);
    let mut buf = Vec::new();
    match pumpkin_nbt::to_bytes_unnamed(&value, &mut buf) {
        Ok(()) => buf.into_boxed_slice(),
        Err(e) => {
            warn!("Failed to serialize biome NBT for {name}: {e}. Using minimal fallback.");
            let fallback = serde_json::json!({
                "has_precipitation": true,
                "temperature": 0.8f32,
                "downfall": 0.4f32,
                "attributes": { "minecraft:visual/sky_color": 0x78_A7_FF },
                "effects": { "water_color": 0x3F_76_E4 },
            });
            let mut fb_buf = Vec::new();
            pumpkin_nbt::to_bytes_unnamed(&fallback, &mut fb_buf)
                .expect("Minimal fallback NBT must always serialize");
            fb_buf.into_boxed_slice()
        }
    }
}

/// Pre-scan region files to discover modded biome names before any client connects
pub fn discover_modded_biomes_from_region_files(
    region_folder: &std::path::Path,
) -> Vec<String> {
    let paths: Vec<PathBuf> = match std::fs::read_dir(region_folder) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "mca"))
            .collect(),
        Err(e) => {
            debug!("Cannot read region folder for biome discovery: {e}");
            return Vec::new();
        }
    };
    if paths.is_empty() {
        return Vec::new();
    }

    // Scan region files in parallel, each yielding the non-vanilla
    // biome names it contains, then union the results
    let discovered: std::collections::BTreeSet<String> = paths
        .par_iter()
        .flat_map(|path| match scan_region_file_for_biomes(path) {
            Ok(names) => names
                .into_iter()
                .filter(|name| {
                    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
                    pumpkin_data::chunk::Biome::from_name(stripped).is_none()
                })
                .collect::<Vec<_>>(),
            Err(e) => {
                debug!("Failed to scan region file {} for biomes: {e}", path.display());
                Vec::new()
            }
        })
        .collect();

    let discovered: Vec<String> = discovered.into_iter().collect();

    if !discovered.is_empty() {
        info!(
            "Discovered {} modded biome(s) from {} region file(s): {}",
            discovered.len(),
            paths.len(),
            discovered.join(", ")
        );
    }

    discovered
}

/// Scan a single region file for modded biome names
fn scan_region_file_for_biomes(path: &std::path::Path) -> Result<Vec<String>, std::io::Error> {
    use flate2::read::ZlibDecoder;

    const SECTOR_BYTES: usize = 4096;
    const CHUNK_COUNT: usize = 1024;

    let mut file = std::fs::File::open(path)?;
    let mut header = vec![0u8; SECTOR_BYTES * 2];
    file.read_exact(&mut header)?;

    let mut location_bytes = &header[..SECTOR_BYTES];
    let mut names = Vec::new();

    for _i in 0..CHUNK_COUNT {
        // Read location table entry (4 bytes)
        let location = u32::from_be_bytes([
            location_bytes[0],
            location_bytes[1],
            location_bytes[2],
            location_bytes[3],
        ]);
        location_bytes = &location_bytes[4..];

        let sector_offset = (location >> 8) as usize;
        let sector_count = (location & 0xFF) as usize;

        if sector_offset == 0 || sector_count == 0 {
            continue; // Chunk not present
        }

        // Seek to chunk data
        let data_offset = sector_offset * SECTOR_BYTES;
        if file.seek(SeekFrom::Start(data_offset as u64)).is_err() {
            continue;
        }

        // Read chunk header (length + compression)
        let mut chunk_header = [0u8; 5];
        if file.read_exact(&mut chunk_header).is_err() {
            continue;
        }

        let length = u32::from_be_bytes([chunk_header[0], chunk_header[1], chunk_header[2], chunk_header[3]]) as usize;
        let compression = chunk_header[4];

        if length == 0 || length > 10_000_000 {
            continue; // Sanity check
        }

        let payload_length = length.saturating_sub(1);
        let mut compressed = vec![0u8; payload_length];
        if file.read_exact(&mut compressed).is_err() {
            continue;
        }

        // Decompress
        let decompressed = match compression {
            1 => {
                // GZip
                let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
                let mut out = Vec::new();
                if decoder.read_to_end(&mut out).is_err() {
                    continue;
                }
                out
            }
            2 => {
                // ZLib
                let mut decoder = ZlibDecoder::new(&compressed[..]);
                let mut out = Vec::new();
                if decoder.read_to_end(&mut out).is_err() {
                    continue;
                }
                out
            }
            4 => {
                // LZ4
                let mut decoder = lz4_java_wrc::Lz4BlockInput::new(&compressed[..]);
                let mut out = Vec::new();
                if decoder.read_to_end(&mut out).is_err() {
                    continue;
                }
                out
            }
            _ => continue, // Unsupported or uncompressed
        };

        // Extract biome names from the NBT data
        // Try named root first, then unnamed
        let chunk_names = extract_biome_names_from_nbt(&decompressed);
        for name in chunk_names {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    Ok(names)
}

#[derive(serde::Deserialize)]
struct BiomeScanRoot {
    #[serde(default)]
    sections: Vec<BiomeScanSection>,
}

#[derive(serde::Deserialize)]
struct BiomeScanSection {
    #[serde(default)]
    biomes: Option<BiomeScanBiomes>,
}

#[derive(serde::Deserialize)]
struct BiomeScanBiomes {
    #[serde(default)]
    palette: Vec<String>,
}

/// Extract biome palette names from chunk NBT data
fn extract_biome_names_from_nbt(data: &[u8]) -> Vec<String> {
    let root = pumpkin_nbt::from_bytes::<BiomeScanRoot>(std::io::Cursor::new(data))
        .or_else(|_| pumpkin_nbt::from_bytes_unnamed::<BiomeScanRoot>(std::io::Cursor::new(data)));

    let Ok(root) = root else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for section in root.sections {
        if let Some(biomes) = section.biomes {
            for name in biomes.palette {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_start_sits_immediately_after_vanilla_biomes() {
        let start = dynamic_biome_start();
        assert!(start > 0, "expected at least one vanilla biome");
        assert!(
            Biome::from_id(start).is_none(),
            "dynamic start {start} collides with a vanilla biome"
        );
        assert!(
            Biome::from_id(start - 1).is_some(),
            "there should be no gap before the dynamic start {start}"
        );
    }

    #[test]
    fn registry_entries_are_ordered_by_internal_id() {
        let mut registry = DynamicBiomeRegistry::new();
        let base = dynamic_biome_start();
        let order = ["zeta:a", "alpha:b", "mu:c", "beta:d", "nu:e"];
        for name in order {
            registry.register(name);
        }

        let entries = registry.get_registry_entries();
        assert_eq!(entries.len(), order.len());

        for (i, (name, _data)) in entries.iter().enumerate() {
            assert_eq!(name, order[i], "entry {i} is out of internal-id order");
            assert_eq!(registry.lookup(name), Some(base + i as u8));
        }
    }

    #[test]
    fn register_datapack_biomes_registers_modded_only_in_sorted_order() {
        let mut registry = DynamicBiomeRegistry::new();
        let base = dynamic_biome_start();
        let def = |t| DatapackBiomeDef {
            network_nbt: Box::<[u8]>::default(),
            temperature: t,
            downfall: 0.5,
            has_precipitation: true,
            feature_ids: Vec::new(),
            carver_ids: Vec::new(),
        };
        // Includes a vanilla `minecraft:` override that must be skipped
        registry.definitions.insert("terralith:zebra".into(), def(0.5));
        registry.definitions.insert("terralith:apple".into(), def(0.5));
        registry.definitions.insert("minecraft:plains".into(), def(0.5));

        let count = registry.register_datapack_biomes();

        assert_eq!(count, 2, "only the two modded biomes should register");
        // Registered in sorted name order → deterministic ids
        assert_eq!(registry.lookup("terralith:apple"), Some(base));
        assert_eq!(registry.lookup("terralith:zebra"), Some(base + 1));
        assert_eq!(registry.lookup("minecraft:plains"), None);
        assert!(registry.has_definitions());
    }

    /// Times the (now parallel + biome-only) region scan on a real world
    /// Ignored by default; point it at a folder and run:
    /// `BIOME_SCAN_DIR="<path to .../region>" cargo test -p pumpkin-world -- --ignored --nocapture times_region_scan`
    #[test]
    #[ignore = "requires a local world; manual benchmark"]
    #[allow(clippy::print_stderr)]
    fn times_region_scan() {
        let dir = std::env::var("BIOME_SCAN_DIR").unwrap_or_else(|_| {
            r"C:\Users\sgibe\AppData\Roaming\Modularium\Minecraft\packs\26.2\saves\26_2\dimensions\minecraft\overworld\region".to_string()
        });
        let path = std::path::Path::new(&dir);
        if !path.is_dir() {
            eprintln!("skipping: {dir} not present");
            return;
        }
        let start = std::time::Instant::now();
        let discovered = discover_modded_biomes_from_region_files(path);
        eprintln!(
            "scanned {dir} in {:?}: {} modded biome(s)",
            start.elapsed(),
            discovered.len()
        );
    }
}
