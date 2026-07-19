use std::{
    collections::HashMap,
    path::PathBuf,
    pin::Pin,
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use bytes::Bytes;
use pumpkin_data::{Block, BlockStateId, chunk::{ChunkStatus, Biome}, fluid::Fluid};
use pumpkin_nbt::{compound::NbtCompound, nbt_long_array};
use rustc_hash::FxHashMap;
use tokio::sync::Mutex;

use crate::{
    chunk::{
        ChunkEntityData, ChunkReadingError, ChunkSerializingError,
        format::anvil::{SingleChunkDataSerializer, WORLD_DATA_VERSION},
        io::{Dirtiable, file_manager::PathFromLevelFolder},
        dynamic_biome::DYNAMIC_BIOMES,
    },
    block::BlockStateCodec,
    generation::section_coords,
    level::LevelFolder,
    tick::{ScheduledTick, scheduler::ChunkTickScheduler},
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use serde::{Deserialize, Serialize};

use super::{
    ChunkData, ChunkHeightmaps, ChunkLight, ChunkParsingError, ChunkSections,
    palette::{BiomePalette, BlockPalette},
};
pub mod anvil;
pub mod linear;
pub mod pump;

impl SingleChunkDataSerializer for ChunkData {
    #[inline]
    fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>> {
        Box::pin(async move { self.internal_to_bytes() })
    }

    #[inline]
    fn position(&self) -> (i32, i32) {
        (self.x, self.z)
    }
}

impl PathFromLevelFolder for ChunkData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.region_folder.join(file_name)
    }
}

impl Dirtiable for ChunkData {
    #[inline]
    fn mark_dirty(&self, flag: bool) {
        self.dirty.store(flag, Ordering::Relaxed);
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }
}

impl ChunkData {
    /// Build `ChunkData` from an already-deserialized `ChunkNbt`
    /// Extracted so the Anvil named-palette path can construct a `ChunkNbt`
    /// programmatically and reuse all existing chunk-assembly logic
    pub(crate) fn from_chunk_nbt(
        chunk_data: ChunkNbt,
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        if chunk_data.x_pos != position.x || chunk_data.z_pos != position.y {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for chunk {},{} but got it for {},{}!",
                position.x, position.y, chunk_data.x_pos, chunk_data.z_pos,
            )));
        }
        let min_y_section = chunk_data.min_y_section;
        let max_y_section = chunk_data
            .sections
            .iter()
            .map(|s| s.y)
            .max()
            .unwrap_or(min_y_section as i8);

        let section_count = (max_y_section as i32 - min_y_section + 1).max(0) as usize;
        let mut block_lights = vec![LightContainer::Empty(0); section_count];
        let mut sky_lights = vec![LightContainer::Empty(0); section_count];
        let mut block_palettes = vec![BlockPalette::default(); section_count];
        let mut biome_palettes = vec![BiomePalette::default(); section_count];

        for section in chunk_data.sections {
            let index = (section.y as i32 - min_y_section) as usize;
            if index >= section_count {
                continue;
            }

            // When loading light data, missing data should default to 0 (no light)
            block_lights[index] = section
                .block_light
                .map_or(LightContainer::Empty(0), LightContainer::Full);
            sky_lights[index] = section
                .sky_light
                .map_or(LightContainer::Empty(0), LightContainer::Full);

            // Convert NBT to Palettes
            // If a section lacks block_states/biomes, it defaults to empty (all air/default biome)
            // If the palette data is corrupt or contains unsupported entries, this will
            // log a warning and default those entries rather than failing the whole chunk
            // This is intentional for partial-corruption robustness; full deserialization
            // failures (e.g. unknown NBT schema) are caught above
            block_palettes[index] = section
                .block_states
                .map(BlockPalette::from_disk_nbt)
                .unwrap_or_default();
            biome_palettes[index] = section
                .biomes
                .map(BiomePalette::from_disk_nbt)
                .unwrap_or_default();
        }

        // Assemble the LightEngine
        let light_engine = ChunkLight {
            block_light: block_lights.into_boxed_slice(),
            sky_light: sky_lights.into_boxed_slice(),
        };

        // Assemble the ChunkSections
        let min_y = section_coords::section_to_block(chunk_data.min_y_section);
        let (random_tick_sections, randomly_ticking_mask) =
            ChunkSections::build_random_tick_sections_cache(&block_palettes);
        let section = ChunkSections {
            count: block_palettes.len(),
            block_sections: RwLock::new(block_palettes.into_boxed_slice()),
            random_tick_sections: RwLock::new(random_tick_sections),
            randomly_ticking_mask: std::sync::atomic::AtomicU32::new(randomly_ticking_mask),
            biome_sections: RwLock::new(biome_palettes.into_boxed_slice()),
            min_y,
        };
        Ok(Self {
            section,
            heightmap: std::sync::Mutex::new(chunk_data.heightmaps),
            x: position.x,
            z: position.y,
            // This chunk is read from disk, so it has not been modified
            dirty: AtomicBool::new(false),
            block_ticks: ChunkTickScheduler::from_iter(chunk_data.block_ticks),
            fluid_ticks: ChunkTickScheduler::from_iter(chunk_data.fluid_ticks),
            pending_block_entities: {
                let mut block_entities = FxHashMap::default();
                for nbt in chunk_data.block_entities {
                    if let Some(x) = nbt.get_int("x")
                        && let Some(y) = nbt.get_int("y")
                        && let Some(z) = nbt.get_int("z")
                    {
                        block_entities.insert(BlockPos::new(x, y, z), nbt);
                    }
                }
                std::sync::Mutex::new(block_entities)
            },
            light_engine: std::sync::Mutex::new(light_engine),
            light_populated: AtomicBool::new(chunk_data.light_correct),
            status: chunk_data.status.unwrap_or(ChunkStatus::Full),
            blending_data: None,
        })
    }

    pub fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        // Vanilla Anvil / datapack-world format
        let anvil_err = match pumpkin_nbt::from_bytes::<anvil::AnvilChunkRoot>(
            std::io::Cursor::new(chunk_data),
        ) {
            Ok(anvil_root) => {
                let chunk_nbt = convert_anvil_root_to_chunk_nbt(anvil_root);
                return Self::from_chunk_nbt(chunk_nbt, position);
            }
            Err(e) => e,
        };

        // Pumpkin's native format
        let native_err = match pumpkin_nbt::from_bytes::<ChunkNbt>(
            std::io::Cursor::new(chunk_data),
        ) {
            Ok(chunk_nbt) => return Self::from_chunk_nbt(chunk_nbt, position),
            Err(e) => e,
        };

        // Unnamed-root native compound
        if let Ok(chunk_nbt) =
            pumpkin_nbt::from_bytes_unnamed::<ChunkNbt>(std::io::Cursor::new(chunk_data))
        {
            return Self::from_chunk_nbt(chunk_nbt, position);
        }

        Err(ChunkParsingError::ErrorDeserializingChunk(format!(
            "anvil-format parse failed ({anvil_err}); native-format parse failed ({native_err})"
        )))
    }

    fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        fn extract_light_ref(light: Option<&LightContainer>) -> Option<&[u8]> {
            match light {
                Some(LightContainer::Full(data)) => Some(data.as_ref()),
                _ => None,
            }
        }

        let is_light_correct = self
            .light_populated
            .load(std::sync::atomic::Ordering::Relaxed);

        let block_entities_nbt = {
            let entities_guard = self.pending_block_entities.lock().unwrap();
            entities_guard.values().cloned().collect::<Vec<_>>()
        };

        let light_lock = self.light_engine.lock().unwrap();
        let heightmap_lock = self.heightmap.lock().unwrap();
        let block_lock = self.section.block_sections.read().unwrap();
        let biome_lock = self.section.biome_sections.read().unwrap();

        let min_section_y = (self.section.min_y >> 4) as i8;

        let sections = (0..self.section.count)
            .map(|i| ChunkSectionNbtRef {
                y: i as i8 + min_section_y,
                block_states: Some(block_lock[i].to_disk_nbt()),
                biomes: Some(biome_lock[i].to_disk_nbt()),
                block_light: extract_light_ref(light_lock.block_light.get(i)),
                sky_light: extract_light_ref(light_lock.sky_light.get(i)),
            })
            .collect::<Vec<_>>();

        let nbt_ref = ChunkNbtRef {
            data_version: WORLD_DATA_VERSION,
            x_pos: self.x,
            z_pos: self.z,
            min_y_section: section_coords::block_to_section(self.section.min_y),
            status: Some(&self.status),
            heightmaps: &heightmap_lock,
            sections,
            block_ticks: &self.block_ticks.to_vec(),
            fluid_ticks: &self.fluid_ticks.to_vec(),
            block_entities: &block_entities_nbt,
            light_correct: is_light_correct,
        };

        let mut result = Vec::new();
        pumpkin_nbt::to_bytes(&nbt_ref, &mut result)
            .map_err(ChunkSerializingError::ErrorSerializingChunk)?;

        Ok(result.into())
    }
}

impl PathFromLevelFolder for ChunkEntityData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.entities_folder.join(file_name)
    }
}

impl Dirtiable for ChunkEntityData {
    #[inline]
    fn mark_dirty(&self, flag: bool) {
        self.dirty.store(flag, Ordering::Relaxed);
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }
}

impl SingleChunkDataSerializer for ChunkEntityData {
    #[inline]
    fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>> {
        Box::pin(async move { self.internal_to_bytes().await })
    }

    #[inline]
    fn position(&self) -> (i32, i32) {
        (self.x, self.z)
    }
}

impl ChunkEntityData {
    fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        let is_named = chunk_data.len() >= 3
            && chunk_data[0] == 0x0a
            && chunk_data[1] == 0x00
            && chunk_data[2] == 0x00;
        let chunk_entity_data =  if is_named {
            pumpkin_nbt::from_bytes::<EntityNbt>(std::io::Cursor::new(chunk_data))
        } else {
            pumpkin_nbt::from_bytes::<EntityNbt>(std::io::Cursor::new(chunk_data))
                .or_else(|_| pumpkin_nbt::from_bytes_unnamed::<EntityNbt>(std::io::Cursor::new(chunk_data)))
        }
        .map_err(|e| ChunkParsingError::ErrorDeserializingChunk(e.to_string()))?;

        if chunk_entity_data.position[0] != position.x
            || chunk_entity_data.position[1] != position.y
        {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for entity chunk {},{} but got it for {},{}!",
                position.x,
                position.y,
                chunk_entity_data.position[0],
                chunk_entity_data.position[1],
            )));
        }

        Ok(Self {
            x: position.x,
            z: position.y,
            data: Mutex::new(chunk_entity_data.entities),
            dirty: AtomicBool::new(false),
        })
    }

    async fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        let nbt = EntityNbt {
            data_version: WORLD_DATA_VERSION,
            position: [self.x, self.z],
            entities: self.data.lock().await.clone(),
        };

        let mut result = Vec::new();
        pumpkin_nbt::to_bytes(&nbt, &mut result)
            .map_err(ChunkSerializingError::ErrorSerializingChunk)?;
        Ok(result.into())
    }
}

/// Convert an `AnvilChunkRoot` (named block/biome palettes) into the internal
/// `ChunkNbt` format (numeric palettes).  Unknown blocks default to air
/// unknown biomes default to plains (id 0)
fn convert_anvil_root_to_chunk_nbt(anvil: anvil::AnvilChunkRoot) -> ChunkNbt {
    let sections = anvil
        .sections
        .into_iter()
        .map(|sec| {
            let block_states = sec.block_states.map(|bs| {
                let palette: Box<[BlockStateId]> = bs
                    .palette
                    .into_iter()
                    .map(|entry| resolve_anvil_block_entry(&entry.name, entry.properties))
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                ChunkSectionBlockStates {
                    data: bs.data,
                    palette,
                }
            });

            let biomes = sec.biomes.map(|bio| {
                let palette: Box<[u8]> = bio
                    .palette
                    .into_iter()
                    .map(|name| resolve_anvil_biome_entry(&name))
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                ChunkSectionBiomes {
                    data: bio.data,
                    palette,
                }
            });

            ChunkSectionNBT {
                block_states,
                biomes,
                block_light: sec.block_light,
                sky_light: sec.sky_light,
                y: sec.y,
            }
        })
        .collect();

    ChunkNbt {
        data_version: anvil.data_version,
        x_pos: anvil.x_pos,
        z_pos: anvil.z_pos,
        min_y_section: anvil.min_y_section,
        status: anvil.status,
        sections,
        heightmaps: anvil.heightmaps,
        block_ticks: anvil.block_ticks,
        fluid_ticks: anvil.fluid_ticks,
        block_entities: anvil.block_entities,
        light_correct: anvil.light_correct,
    }
}

fn resolve_anvil_block_entry(name: &str, properties: Option<HashMap<String, String>>) -> BlockStateId {
    let Some(block) = Block::from_name(name).or_else(|| {
        // Some vanilla chunks write names without the minecraft: prefix
        Block::from_name(&format!("minecraft:{name}"))
    }) else {
        return Block::AIR.default_state.id;
    };

    properties.map_or(block.default_state.id, |props| {
        let codec = BlockStateCodec {
            name: block,
            properties: Some(props),
        };
        codec.get_state_id()
    })
}

fn resolve_anvil_biome_entry(name: &str) -> u8 {
    // 1. Try vanilla biome lookup (existing behavior)
    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(biome) = Biome::from_name(stripped) {
        return biome.id;
    }

    // 2. Try the dynamic/modded biome registry
    {
        let registry = DYNAMIC_BIOMES.read().unwrap();
        if let Some(id) = registry.lookup(name) {
            return id;
        }
    }

    // 3. Register this unknown biome dynamically
    {
        let mut registry = DYNAMIC_BIOMES.write().unwrap();
        if let Some(id) = registry.register(name) {
            return id;
        }
    }

    // 4. Registry full — log and fall back to plains
    tracing::warn!("Unknown biome '{name}' and dynamic registry full, falling back to plains");
    Biome::PLAINS.id
}

#[derive(Serialize, Deserialize)]
struct ChunkSectionNBT {
    #[serde(skip_serializing_if = "Option::is_none")]
    block_states: Option<ChunkSectionBlockStates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    biomes: Option<ChunkSectionBiomes>,
    #[serde(rename = "BlockLight", skip_serializing_if = "Option::is_none")]
    block_light: Option<Box<[u8]>>,
    #[serde(rename = "SkyLight", skip_serializing_if = "Option::is_none")]
    sky_light: Option<Box<[u8]>>,
    #[serde(rename = "Y")]
    y: i8,
}

#[derive(Serialize)]
struct ChunkSectionNbtRef<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    block_states: Option<ChunkSectionBlockStates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    biomes: Option<ChunkSectionBiomes>,
    #[serde(rename = "BlockLight", skip_serializing_if = "Option::is_none")]
    block_light: Option<&'a [u8]>,
    #[serde(rename = "SkyLight", skip_serializing_if = "Option::is_none")]
    sky_light: Option<&'a [u8]>,
    #[serde(rename = "Y")]
    y: i8,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChunkSectionBiomes {
    #[serde(
        serialize_with = "nbt_long_array",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) data: Option<Box<[i64]>>,
    pub(crate) palette: Box<[u8]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChunkSectionBlockStates {
    #[serde(
        serialize_with = "nbt_long_array",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) data: Option<Box<[i64]>>,
    #[serde(with = "block_state_checked")]
    pub(crate) palette: Box<[BlockStateId]>,
}

mod block_state_checked {
    use pumpkin_data::BlockStateId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        value: &[BlockStateId],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|v| BlockStateId::as_u16(*v))
            .collect::<Vec<u16>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<[BlockStateId]>, D::Error> {
        let raw = <Box<[u16]> as Deserialize>::deserialize(deserializer)?;
        Ok(raw.iter().map(|v| BlockStateId::new_or_air(*v)).collect())
    }
}

#[derive(Debug, Clone)]
pub enum LightContainer {
    Empty(u8),
    Full(Box<[u8]>),
}

impl LightContainer {
    pub const DIM: usize = 16;
    pub const ARRAY_SIZE: usize = Self::DIM * Self::DIM * Self::DIM / 2;

    #[must_use]
    pub fn new_empty(default: u8) -> Self {
        assert!(default <= 15, "Default value must be between 0 and 15");
        Self::Empty(default)
    }

    #[must_use]
    pub fn new(data: Box<[u8]>) -> Self {
        assert!(
            data.len() == Self::ARRAY_SIZE,
            "Data length must be {}",
            Self::ARRAY_SIZE
        );
        Self::Full(data)
    }

    #[must_use]
    pub fn new_filled(default: u8) -> Self {
        assert!(default <= 15, "Default value must be between 0 and 15");
        let value = default << 4 | default;
        Self::Full([value; Self::ARRAY_SIZE].into())
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty(_))
    }

    const fn index(x: usize, y: usize, z: usize) -> usize {
        y * 16 * 16 + z * 16 + x
    }

    #[must_use]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                data[index >> 1] >> (4 * (index & 1)) & 0x0F
            }
            Self::Empty(default) => *default,
        }
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, value: u8) {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                let mask = 0x0F << (4 * (index & 1));
                data[index >> 1] &= !mask;
                data[index >> 1] |= value << (4 * (index & 1));
            }
            Self::Empty(default) => {
                if value != *default {
                    *self = Self::new_filled(*default);
                    self.set(x, y, z, value);
                }
            }
        }
    }

    pub fn fill(&mut self, value: u8) {
        *self = Self::new_filled(value);
    }
}

impl Default for LightContainer {
    fn default() -> Self {
        Self::new_empty(15)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ChunkNbt {
    data_version: i32,
    #[serde(rename = "xPos")]
    x_pos: i32,
    #[serde(rename = "zPos")]
    z_pos: i32,
    #[serde(rename = "yPos")]
    min_y_section: i32,
    #[serde(default)]
    status: Option<ChunkStatus>,
    #[serde(rename = "sections")]
    sections: Vec<ChunkSectionNBT>,
    #[serde(default)]
    heightmaps: ChunkHeightmaps,
    #[serde(rename = "block_ticks", default)]
    block_ticks: Vec<ScheduledTick<&'static Block>>,
    #[serde(rename = "fluid_ticks", default)]
    fluid_ticks: Vec<ScheduledTick<&'static Fluid>>,
    #[serde(rename = "block_entities", default)]
    block_entities: Vec<NbtCompound>,
    #[serde(rename = "isLightOn", default)]
    light_correct: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ChunkNbtRef<'a> {
    data_version: i32,
    #[serde(rename = "xPos")]
    x_pos: i32,
    #[serde(rename = "zPos")]
    z_pos: i32,
    #[serde(rename = "yPos")]
    min_y_section: i32,
    status: Option<&'a ChunkStatus>,
    #[serde(rename = "sections")]
    sections: Vec<ChunkSectionNbtRef<'a>>,
    heightmaps: &'a ChunkHeightmaps,
    #[serde(rename = "block_ticks")]
    block_ticks: &'a [ScheduledTick<&'static Block>],
    #[serde(rename = "fluid_ticks")]
    fluid_ticks: &'a [ScheduledTick<&'static Fluid>],
    #[serde(rename = "block_entities")]
    block_entities: &'a [NbtCompound],
    #[serde(rename = "isLightOn", default)]
    light_correct: bool,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct EntityNbt {
    data_version: i32,
    position: [i32; 2],
    entities: Vec<NbtCompound>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkData, ChunkLight, ChunkSections};
    use crate::tick::scheduler::ChunkTickScheduler;
    use pumpkin_data::{Block, chunk::ChunkStatus};

    fn empty_chunk(x: i32, z: i32, min_y: i32, sections: usize) -> ChunkData {
        ChunkData {
            section: ChunkSections::new(sections, min_y),
            heightmap: std::sync::Mutex::default(),
            x,
            z,
            block_ticks: ChunkTickScheduler::default(),
            fluid_ticks: ChunkTickScheduler::default(),
            pending_block_entities: std::sync::Mutex::new(FxHashMap::default()),
            light_engine: std::sync::Mutex::new(ChunkLight::default()),
            light_populated: AtomicBool::new(false),
            status: ChunkStatus::Full,
            blending_data: None,
            dirty: AtomicBool::new(true),
        }
    }

    /// A regression broke every `.pump` chunk with a misleading `missing field DataVersion`
    /// whoops
    #[test]
    fn native_chunk_round_trips_through_bytes() {
        let min_y = -64;
        let chunk = empty_chunk(3, -5, min_y, 24);
        let stone = Block::STONE.default_state.id;
        chunk
            .section
            .set_block_absolute_y(1, min_y + 2, 2, stone);

        let bytes = chunk.internal_to_bytes().expect("serialize");

        // Written payload is a named root compound with an empty name
        assert_eq!(&bytes[..3], &[0x0a, 0x00, 0x00], "expected named root");

        let read = ChunkData::internal_from_bytes(&bytes, Vector2::new(3, -5))
            .expect("native chunk must deserialize (regression: missing field DataVersion)");

        assert_eq!(read.x, 3);
        assert_eq!(read.z, -5);
        assert_eq!(
            read.section.get_block_absolute_y(1, min_y + 2, 2),
            Some(stone),
            "block placed before save must survive the round-trip"
        );
    }
}
