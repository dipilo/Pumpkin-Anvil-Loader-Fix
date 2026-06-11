use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    sync::{LazyLock, RwLock},
};

use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::resource_location::ResourceLocation;
use tracing::{debug, info, warn};

/// The next available dynamic biome ID. Vanilla biomes use compile-time IDs
/// We reserve a range for dynamic biomes encountered in Anvil worlds
const DYNAMIC_BIOME_START: u8 = 65;
const MAX_DYNAMIC_BIOME_ID: u8 = 239;

/// Runtime registry for modded/datapack biomes not known at compile time
///
/// Pumpkin's biome system uses compile-time generated code with only vanilla biomes
/// This registry allows mapping modded biome names (e.g. `terralith:alpha_islands`)
/// to runtime IDs so that Anvil worlds with custom biomes load correctly
pub static DYNAMIC_BIOMES: LazyLock<RwLock<DynamicBiomeRegistry>> =
    LazyLock::new(|| RwLock::new(DynamicBiomeRegistry::new()));

pub struct DynamicBiomeRegistry {
    /// Full namespaced name (e.g. "terralith:alpha_islands") -> biome ID
    name_to_id: HashMap<String, u8>,
    /// biome ID -> entry
    id_to_entry: HashMap<u8, DynamicBiomeEntry>,
    next_id: u8,
}

#[derive(Clone, Debug)]
pub struct DynamicBiomeEntry {
    pub name: String,
    /// The network/registry NBT data sent to clients
    /// Contains the biome definition the client uses for rendering
    pub data: Box<[u8]>,
}

impl DynamicBiomeRegistry {
    pub fn new() -> Self {
        Self {
            name_to_id: HashMap::new(),
            id_to_entry: HashMap::new(),
            next_id: DYNAMIC_BIOME_START,
        }
    }

    /// Look up a biome by its full namespaced name (e.g. "terralith:alpha_islands")
    /// Returns None if not registered
    pub fn lookup(&self, name: &str) -> Option<u8> {
        self.name_to_id.get(name).copied()
    }

    /// Register a new modded biome, assigning it the next available dynamic ID
    /// If already registered, returns the existing ID
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

        info!("Registered dynamic biome '{name}' -> internal ID {id}");

        // Build biome NBT data for the client registry
        // This MUST always succeed - the client requires valid NBT for every biome entry
        let data = build_biome_nbt(name);

        self.name_to_id.insert(name.to_string(), id);
        self.id_to_entry.insert(
            id,
            DynamicBiomeEntry {
                name: name.to_string(),
                data,
            },
        );

        Some(id)
    }

    /// Register a modded biome if it's not a vanilla biome
    /// Returns the dynamic ID if registered, None if vanilla or already known
    pub fn register_if_modded(&mut self, name: &str) -> Option<u8> {
        let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
        // Check if it's a vanilla biome — if so, don't register dynamically
        if pumpkin_data::chunk::Biome::from_name(stripped).is_some() {
            return None;
        }
        self.register(name)
    }

    /// Get all registered dynamic biome entries as protocol registry entries
    /// for the client configuration phase
    pub fn get_registry_entries(&self) -> Vec<(ResourceLocation, Box<[u8]>)> {
        self.id_to_entry
            .values()
            .map(|entry| (entry.name.clone(), entry.data.clone()))
            .collect()
    }

    /// Returns true if any dynamic biomes have been registered
    pub fn has_entries(&self) -> bool {
        !self.name_to_id.is_empty()
    }

    /// Get the count of registered dynamic biomes
    pub fn len(&self) -> usize {
        self.name_to_id.len()
    }

    /// Returns true if no dynamic biomes are registered
    pub fn is_empty(&self) -> bool {
        self.name_to_id.is_empty()
    }

    /// Pre-register a list of biome names (from world pre-scan)
    /// Skips vanilla biomes and already-registered entries
    pub fn preload_biomes(&mut self, names: &[String]) {
        for name in names {
            let _ = self.register_if_modded(name);
        }
    }
}

impl Default for DynamicBiomeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Reset the dynamic biome registry.
pub fn clear_dynamic_biomes() {
    let mut registry = DYNAMIC_BIOMES.write().unwrap();
    registry.name_to_id.clear();
    registry.id_to_entry.clear();
    registry.next_id = DYNAMIC_BIOME_START;
    info!("Dynamic biome registry cleared");
}

/// Build biome NBT data for client sync
///
/// This function MUST always return valid NBT bytes. The client crashes with
/// "Expected non-null compound tag" if a biome registry entry has no data
///
/// We use best-effort defaults based on the biome name. For production use,
/// you should load actual biome definitions from the world's datapack folder
fn build_biome_nbt(name: &str) -> Box<[u8]> {
    let mut compound = NbtCompound::new();

    let lower = name.to_lowercase();

    // Best-effort temperature/precipitation based on biome name keywords
    let (has_precipitation, temperature, downfall) =
        if lower.contains("desert")
            || lower.contains("sands")
            || lower.contains("oasis")
            || lower.contains("badlands")
            || lower.contains("mesa")
        {
            (false, 2.0_f32, 0.0_f32)
        } else if lower.contains("savanna")
            || lower.contains("shrubland")
            || lower.contains("steppe")
            || lower.contains("brushland")
        {
            (false, 1.2_f32, 0.0_f32)
        } else if lower.contains("jungle")
            || lower.contains("tropical")
            || lower.contains("rainforest")
            || lower.contains("kelp")
        {
            (true, 0.95_f32, 0.9_f32)
        } else if lower.contains("swamp")
            || lower.contains("bog")
            || lower.contains("marsh")
            || lower.contains("wetland")
        {
            (true, 0.8_f32, 0.9_f32)
        } else if lower.contains("snow")
            || lower.contains("frozen")
            || lower.contains("ice")
            || lower.contains("glacier")
            || lower.contains("tundra")
            || lower.contains("siberian")
        {
            (true, -0.5_f32, 0.4_f32)
        } else if lower.contains("mountain")
            || lower.contains("peak")
            || lower.contains("cliff")
            || lower.contains("highland")
            || lower.contains("canyon")
            || lower.contains("yosemite")
        {
            (true, 0.2_f32, 0.3_f32)
        } else if lower.contains("ocean") || lower.contains("sea") {
            (true, 0.5_f32, 0.5_f32)
        } else if lower.contains("beach")
            || lower.contains("shore")
            || lower.contains("cave")
            || lower.contains("underground")
        {
            (true, 0.8_f32, 0.4_f32)
        } else if lower.contains("inferno")
            || lower.contains("volcanic")
            || lower.contains("crater")
        {
            (false, 2.0_f32, 0.0_f32)
        } else {
            // Default to plains-like
            (true, 0.8_f32, 0.4_f32)
        };

    compound.put_string(
        "has_precipitation",
        if has_precipitation {
            "true".to_string()
        } else {
            "false".to_string()
        },
    );
    compound.put_float("temperature", temperature);
    compound.put_float("downfall", downfall);

    // Effects — best-effort color choices
    let (
        water_color,
        water_fog_color,
        fog_color,
        sky_color,
        foliage_color,
        grass_color,
        has_foliage_override,
        has_grass_override,
    ) = if lower.contains("desert")
        || lower.contains("sands")
        || lower.contains("oasis")
        || lower.contains("badlands")
        || lower.contains("mesa")
    {
        (
            0x3F_76_E4_i32,
            0x3F_76_E4_i32,
            0xC0_D8_FF,
            0x6E_B1_FF,
            None,
            Some(0x90_81_4D),
            false,
            true,
        )
    } else if lower.contains("jungle")
        || lower.contains("tropical")
        || lower.contains("rainforest")
    {
        (
            0x3F_76_E4,
            0x3F_76_E4,
            0xC0_D8_FF,
            0x77_A8_FF,
            None,
            Some(0x59_C9_3C),
            false,
            true,
        )
    } else if lower.contains("swamp")
        || lower.contains("bog")
        || lower.contains("marsh")
    {
        (
            0x61_7B_64_i32,
            0x23_23_17,
            0xC0_D8_FF,
            0x78_A7_FF,
            None,
            Some(0x6A_70_39),
            false,
            true,
        )
    } else if lower.contains("snow")
        || lower.contains("frozen")
        || lower.contains("ice")
        || lower.contains("glacier")
        || lower.contains("tundra")
    {
        (
            0x3F_76_E4,
            0x3F_76_E4,
            0xC0_D8_FF,
            0x80_A4_FF,
            None,
            Some(0x80_B4_97),
            false,
            true,
        )
    } else if lower.contains("cherry") || lower.contains("sakura") {
        (
            0x5D_B7_EF,
            0x5D_B7_EF,
            0xC0_D8_FF,
            0x7B_A4_FF,
            Some(0xB6_DB_61),
            Some(0xB6_DB_61),
            true,
            true,
        )
    } else if lower.contains("dark") || lower.contains("mushroom") {
        (
            0x3F_76_E4,
            0x3F_76_E4,
            0xC0_D8_FF,
            0x78_A7_FF,
            Some(0x59_AE_30),
            Some(0x59_AE_30),
            true,
            true,
        )
    } else if lower.contains("inferno") || lower.contains("volcanic") {
        (
            0x3F_76_E4,
            0x3F_76_E4,
            0x68_5F_70,
            0x6C_65_5B,
            None,
            Some(0x5A_4D_40),
            false,
            true,
        )
    } else {
        // Default plains colors
        (
            0x3F_76_E4,
            0x3F_76_E4,
            0xC0_D8_FF,
            0x78_A7_FF,
            None,
            None,
            false,
            false,
        )
    };

    let mut effects = NbtCompound::new();
    effects.put_int("water_color", water_color);
    effects.put_int("water_fog_color", water_fog_color);
    effects.put_int("fog_color", fog_color);
    effects.put_int("sky_color", sky_color);
    if has_foliage_override 
        && let Some(fc) = foliage_color {
            effects.put_int("foliage_color", fc);
        }
    if has_grass_override 
        && let Some(gc) = grass_color {
            effects.put_int("grass_color", gc);
        }
    compound.put("effects", effects);

    // Mood sound (required by some clients)
    let mut mood_sound = NbtCompound::new();
    mood_sound.put_string("sound", "minecraft:ambient.cave".to_string());
    mood_sound.put_int("tick_delay", 6000);
    mood_sound.put_int("block_search_extent", 8);
    mood_sound.put_float("offset", 2.0_f32);
    compound.put("mood_sound", mood_sound);

    let mut buf = Vec::new();
    match pumpkin_nbt::to_bytes(&compound, &mut buf) {
        Ok(()) => buf.into_boxed_slice(),
        Err(e) => {
            // This should never happen with our controlled data, but if it does,
            // create an absolute minimal valid compound to prevent client crashes
            warn!("Failed to serialize biome NBT for {name}: {e}. Using minimal fallback.");
            let mut fallback = NbtCompound::new();
            fallback.put_string("has_precipitation", "true".to_string());
            fallback.put_float("temperature", 0.8_f32);
            fallback.put_float("downfall", 0.4_f32);
            let mut fb_effects = NbtCompound::new();
            fb_effects.put_int("water_color", 0x3F_76_E4);
            fb_effects.put_int("water_fog_color", 0x3F_76_E4);
            fb_effects.put_int("fog_color", 0xC0_D8_FF);
            fb_effects.put_int("sky_color", 0x78_A7_FF);
            fallback.put("effects", fb_effects);
            let mut fb_buf = Vec::new();
            // This absolutely must succeed
            pumpkin_nbt::to_bytes(&fallback, &mut fb_buf)
                .expect("Minimal fallback NBT must always serialize");
            fb_buf.into_boxed_slice()
        }
    }
}

/// Pre-scan region files to discover modded biome names before any client connects
/// This ensures all dynamic biomes are registered in time for the registry sync
///
/// This function efficiently scans Anvil region files without fully parsing chunks
/// It reads the chunk headers, decompresses chunk data, and extracts biome palette entries
pub fn discover_modded_biomes_from_region_files(
    region_folder: &std::path::Path,
) -> Vec<String> {
    let mut discovered = Vec::new();

    let entries = match std::fs::read_dir(region_folder) {
        Ok(e) => e,
        Err(e) => {
            debug!("Cannot read region folder for biome discovery: {e}");
            return discovered;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "mca") {
            continue;
        }

        match scan_region_file_for_biomes(&path) {
            Ok(names) => {
                for name in names {
                    // Only keep modded (non-vanilla) biomes
                    let stripped = name.strip_prefix("minecraft:").unwrap_or(&name);
                    if pumpkin_data::chunk::Biome::from_name(stripped).is_none()
                        && !discovered.contains(&name)
                    {
                        discovered.push(name);
                    }
                }
            }
            Err(e) => {
                debug!("Failed to scan region file {} for biomes: {e}", path.display());
            }
        }
    }

    if !discovered.is_empty() {
        info!(
            "Discovered {} modded biome(s) from region files: {}",
            discovered.len(),
            discovered.join(", ")
        );
    }

    discovered
}

/// Scan a single region file for modded biome names
/// This reads the region file header, then for each present chunk,
/// decompresses and parses just enough NBT to extract biome palette entries
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

/// Extract biome palette names from chunk NBT data
/// This is a lightweight extraction that doesn't fully deserialize the chunk
fn extract_biome_names_from_nbt(data: &[u8]) -> Vec<String> {
    let mut names = Vec::new();

    // Try parsing as AnvilChunkRoot (named NBT)
    if let Ok(root) = pumpkin_nbt::from_bytes::<super::format::anvil::AnvilChunkRoot>(
        std::io::Cursor::new(data),
    ) {
        for section in &root.sections {
            if let Some(biomes) = &section.biomes {
                for name in &biomes.palette {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
        return names;
    }

    // Try unnamed root
    if let Ok(root) = pumpkin_nbt::from_bytes_unnamed::<super::format::anvil::AnvilChunkRoot>(
        std::io::Cursor::new(data),
    ) {
        for section in &root.sections {
            if let Some(biomes) = &section.biomes {
                for name in &biomes.palette {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
    }

    names
}
