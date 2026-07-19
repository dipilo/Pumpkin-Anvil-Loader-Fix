use super::chunk_state::{Chunk, StagedChunkEnum};
use crate::ProtoChunk;
use crate::chunk::ChunkHeightmapType;
use crate::generation::generator;
use crate::generation::height_limit::HeightLimitView;
use crate::generation::proto_chunk::GenerationCache;
use crate::world::{BlockAccessor, WorldPortalExt};
use pumpkin_config::lighting::LightingEngineConfig;
use pumpkin_data::biome::Biome;
use pumpkin_data::block_properties::is_air;
use pumpkin_data::fluid::{Fluid, FluidState};
use pumpkin_data::{Block, BlockState, BlockStateId};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::HeightMap;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use tracing::debug;

pub struct Cache {
    pub x: i32,
    pub z: i32,
    pub size: i32,
    pub chunks: Vec<Chunk>,
}

impl HeightLimitView for Cache {
    fn height(&self) -> u16 {
        let mid = ((self.size * self.size) >> 1) as usize;
        match &self.chunks[mid] {
            Chunk::Proto(chunk) => chunk.height(),
            Chunk::Level(_) => panic!(),
        }
    }

    fn bottom_y(&self) -> i8 {
        let mid = ((self.size * self.size) >> 1) as usize;
        match &self.chunks[mid] {
            Chunk::Proto(chunk) => chunk.bottom_y(),
            Chunk::Level(_) => panic!(),
        }
    }
}

impl BlockAccessor for Cache {
    fn get_block(&self, position: &BlockPos) -> &'static Block {
        GenerationCache::get_block_state(self, &position.0).to_block()
    }

    fn get_block_state(&self, position: &BlockPos) -> &'static BlockState {
        GenerationCache::get_block_state(self, &position.0).to_state()
    }

    fn get_block_state_id(&self, position: &BlockPos) -> BlockStateId {
        GenerationCache::get_block_state(self, &position.0)
    }

    fn get_block_and_state(&self, position: &BlockPos) -> (&'static Block, &'static BlockState) {
        let id = GenerationCache::get_block_state(self, &position.0);
        BlockState::from_id_with_block(id)
    }
}

impl GenerationCache for Cache {
    fn get_chunk_mut(&mut self, chunk_x: i32, chunk_z: i32) -> Option<&mut ProtoChunk> {
        let dx = chunk_x - self.x;
        let dz = chunk_z - self.z;

        if dx < 0 || dx >= self.size || dz < 0 || dz >= self.size {
            return None;
        }

        match &mut self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Proto(chunk) => Some(chunk),
            Chunk::Level(_) => None,
        }
    }

    fn get_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        let dx = chunk_x - self.x;
        let dz = chunk_z - self.z;

        if dx < 0 || dx >= self.size || dz < 0 || dz >= self.size {
            return None;
        }

        match &self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Proto(chunk) => Some(chunk),
            Chunk::Level(_) => None,
        }
    }

    fn try_get_proto_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        let dx = chunk_x - self.x;
        let dz = chunk_z - self.z;

        if dx < 0 || dx >= self.size || dz < 0 || dz >= self.size {
            return None;
        }

        match &self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Proto(chunk) => Some(chunk),
            Chunk::Level(_) => None,
        }
    }

    fn get_center_chunk(&self) -> &ProtoChunk {
        let mid = ((self.size * self.size) >> 1) as usize;
        self.chunks[mid].get_proto_chunk()
    }

    fn get_center_chunk_mut(&mut self) -> &mut ProtoChunk {
        let mid = ((self.size * self.size) >> 1) as usize;
        self.chunks[mid].get_proto_chunk_mut()
    }

    fn get_fluid_and_fluid_state(&self, pos: &Vector3<i32>) -> (Fluid, FluidState) {
        let id = GenerationCache::get_block_state(self, pos);

        let Some(fluid) = Fluid::from_state_id(id) else {
            let block = Block::from_state_id(id);
            if let Some(properties) = block.properties(id) {
                for (name, value) in properties.to_props() {
                    if name == "waterlogged" {
                        if value == "true" {
                            let fluid = Fluid::FLOWING_WATER;
                            let state = fluid.states[0].clone();
                            return (fluid, state);
                        }

                        break;
                    }
                }
            }

            let fluid = Fluid::EMPTY;
            let state = fluid.states[0].clone();

            return (fluid, state);
        };

        //let state = fluid.get_state(id);
        let state = fluid.states[0].clone();

        (fluid.clone(), state)
    }

    fn get_block_state(&self, pos: &Vector3<i32>) -> BlockStateId {
        let dx = (pos.x >> 4) - self.x;
        let dz = (pos.z >> 4) - self.z;
        // debug_assert!(dx < self.size && dz < self.size);
        // debug_assert!(dx >= 0 && dz >= 0);
        if !(dx < self.size && dz < self.size && dx >= 0 && dz >= 0) {
            // breakpoint here
            debug!(
                "illegal get_block_state {pos:?} cache pos ({}, {}) size {}",
                self.x, self.z, self.size
            );
            return BlockStateId::AIR;
        }
        match &self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Level(data) => data
                .section
                .get_block_absolute_y((pos.x & 15) as usize, pos.y, (pos.z & 15) as usize)
                .unwrap_or(BlockStateId::AIR),

            Chunk::Proto(data) => data.get_block_state(pos),
        }
    }
    fn set_block_state(&mut self, pos: &Vector3<i32>, block_state: &BlockState) {
        let dx = (pos.x >> 4) - self.x;
        let dz = (pos.z >> 4) - self.z;
        // debug_assert!(dx < self.size && dz < self.size);
        // debug_assert!(dx >= 0 && dz >= 0);
        if !(dx < self.size && dz < self.size && dx >= 0 && dz >= 0) {
            // breakpoint here
            debug!(
                "illegal set_block_state {pos:?} cache pos ({}, {}) size {}",
                self.x, self.z, self.size
            );
            return;
        }
        match &mut self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Level(data) => {
                data.set_block_absolute_y(
                    (pos.x & 15) as usize,
                    pos.y,
                    (pos.z & 15) as usize,
                    block_state.id,
                );
            }
            Chunk::Proto(data) => {
                data.set_block_state(pos.x, pos.y, pos.z, block_state);
            }
        }
    }

    fn add_block_entity(&mut self, pos: &Vector3<i32>, nbt: NbtCompound) {
        let dx = (pos.x >> 4) - self.x;
        let dz = (pos.z >> 4) - self.z;
        if !(dx < self.size && dz < self.size && dx >= 0 && dz >= 0) {
            debug!(
                "illegal add_block_entity {pos:?} cache pos ({}, {}) size {}",
                self.x, self.z, self.size
            );
            return;
        }

        match &mut self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Level(_) => {
                debug!("add_block_entity on non-proto chunk at {pos:?}");
            }
            Chunk::Proto(data) => {
                data.add_block_entity(nbt);
            }
        }
    }

    fn get_top_y(&self, heightmap: &HeightMap, x: i32, z: i32) -> i32 {
        match heightmap {
            HeightMap::WorldSurfaceWg | HeightMap::WorldSurface => {
                self.top_block_height_exclusive(x, z)
            }
            HeightMap::OceanFloorWg | HeightMap::OceanFloor => {
                self.ocean_floor_height_exclusive(x, z)
            }
            HeightMap::MotionBlocking => self.top_motion_blocking_block_height_exclusive(x, z),
            HeightMap::MotionBlockingNoLeaves => {
                self.top_motion_blocking_block_no_leaves_height_exclusive(x, z)
            }
        }
    }

    fn top_motion_blocking_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        debug_assert!(dx < self.size && dy < self.size);
        debug_assert!(dx >= 0 && dy >= 0);
        match &self.chunks[(dx * self.size + dy) as usize] {
            Chunk::Level(data) => {
                let heightmap = data.heightmap.lock().unwrap();
                let min_y = data.section.min_y;

                heightmap.get(ChunkHeightmapType::MotionBlocking, x, z, min_y)
            }
            Chunk::Proto(data) => data.top_motion_blocking_block_height_exclusive(x, z),
        }
    }

    fn top_motion_blocking_block_no_leaves_height_exclusive(&self, x: i32, z: i32) -> i32 {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        debug_assert!(dx < self.size && dy < self.size);
        debug_assert!(dx >= 0 && dy >= 0);
        match &self.chunks[(dx * self.size + dy) as usize] {
            Chunk::Level(data) => {
                let heightmap = data.heightmap.lock().unwrap();
                let min_y = data.section.min_y;
                heightmap.get(ChunkHeightmapType::MotionBlockingNoLeaves, x, z, min_y)
            }
            Chunk::Proto(data) => data.top_motion_blocking_block_no_leaves_height_exclusive(x, z),
        }
    }

    fn top_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        debug_assert!(dx < self.size && dy < self.size);
        debug_assert!(dx >= 0 && dy >= 0);
        match &self.chunks[(dx * self.size + dy) as usize] {
            Chunk::Level(data) => {
                let heightmap = data.heightmap.lock().unwrap();
                let min_y = data.section.min_y;
                heightmap.get(ChunkHeightmapType::WorldSurface, x, z, min_y) // can we return this?
            }
            Chunk::Proto(data) => data.top_block_height_exclusive(x, z),
        }
    }

    fn ocean_floor_height_exclusive(&self, x: i32, z: i32) -> i32 {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        if dx < 0 || dy < 0 || dx >= self.size || dy >= self.size {
            return 0;
        }
        match &self.chunks[(dx * self.size + dy) as usize] {
            Chunk::Level(_data) => {
                0 // todo missing
            }
            Chunk::Proto(data) => data.ocean_floor_height_exclusive(x, z),
        }
    }

    fn get_biome_for_terrain_gen(&self, x: i32, y: i32, z: i32) -> &'static Biome {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        let (dx, dy) = if dx < 0 || dy < 0 || dx >= self.size || dy >= self.size {
            // Position is outside the cache — fall back to the centre chunk's biome
            let mid = self.size / 2;
            (mid, mid)
        } else {
            (dx, dy)
        };
        match &self.chunks[(dx * self.size + dy) as usize] {
            Chunk::Level(data) => {
                // A fully-loaded neighbor may carry dynamic/datapack biome IDs
                // (>= 65); resolve them to the closest vanilla biome rather than
                // panicking when they bleed into adjacent terrain generation
                crate::chunk::dynamic_biome::resolve_biome(
                    data.section
                        .get_rough_biome_absolute_y((x & 15) as usize, y, (z & 15) as usize)
                        .unwrap_or(0),
                )
            }
            Chunk::Proto(data) => data.get_terrain_gen_biome(x, y, z),
        }
    }

    fn get_terrain_gen_biome_id(&self, x: i32, y: i32, z: i32) -> u8 {
        let dx = (x >> 4) - self.x;
        let dy = (z >> 4) - self.z;
        let (dx, dy) = if dx < 0 || dy < 0 || dx >= self.size || dy >= self.size {
            // Position is outside the cache — fall back to the centre chunk's biome
            let mid = self.size / 2;
            (mid, mid)
        } else {
            (dx, dy)
        };
        match &self.chunks[(dx * self.size + dy) as usize] {
            // A fully-loaded neighbor may already carry the real id
            Chunk::Level(data) => data
                .section
                .get_rough_biome_absolute_y((x & 15) as usize, y, (z & 15) as usize)
                .unwrap_or(0),
            Chunk::Proto(data) => data.get_terrain_gen_biome_id(x, y, z),
        }
    }

    fn get_blending_data(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Option<&crate::generation::blender::blending_data::BlendingData> {
        let dx = chunk_x - self.x;
        let dz = chunk_z - self.z;

        if dx < 0 || dx >= self.size || dz < 0 || dz >= self.size {
            return None;
        }

        match &self.chunks[(dx * self.size + dz) as usize] {
            Chunk::Proto(chunk) => chunk.blending_data.as_ref(),
            Chunk::Level(data) => data.blending_data.as_ref(),
        }
    }

    fn is_air(&self, local_pos: &Vector3<i32>) -> bool {
        is_air(GenerationCache::get_block_state(self, local_pos))
    }
}

/// A single [`ProtoChunk`] acting as its own 1×1 [`GenerationCache`]
impl GenerationCache for ProtoChunk {
    fn get_chunk_mut(&mut self, chunk_x: i32, chunk_z: i32) -> Option<&mut ProtoChunk> {
        (chunk_x == self.x && chunk_z == self.z).then_some(self)
    }

    fn get_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        (chunk_x == self.x && chunk_z == self.z).then_some(self)
    }

    fn try_get_proto_chunk(&self, chunk_x: i32, chunk_z: i32) -> Option<&ProtoChunk> {
        (chunk_x == self.x && chunk_z == self.z).then_some(self)
    }

    fn get_center_chunk(&self) -> &ProtoChunk {
        self
    }

    fn get_center_chunk_mut(&mut self) -> &mut ProtoChunk {
        self
    }

    fn get_fluid_and_fluid_state(&self, pos: &Vector3<i32>) -> (Fluid, FluidState) {
        let id = GenerationCache::get_block_state(self, pos);

        let Some(fluid) = Fluid::from_state_id(id) else {
            let block = Block::from_state_id(id);
            if let Some(properties) = block.properties(id) {
                for (name, value) in properties.to_props() {
                    if name == "waterlogged" {
                        if value == "true" {
                            let fluid = Fluid::FLOWING_WATER;
                            let state = fluid.states[0].clone();
                            return (fluid, state);
                        }

                        break;
                    }
                }
            }

            let fluid = Fluid::EMPTY;
            let state = fluid.states[0].clone();

            return (fluid, state);
        };

        let state = fluid.states[0].clone();

        (fluid.clone(), state)
    }

    fn get_block_state(&self, pos: &Vector3<i32>) -> BlockStateId {
        if (pos.x >> 4) != self.x || (pos.z >> 4) != self.z {
            return BlockStateId::AIR;
        }
        Self::get_block_state(self, pos)
    }

    fn set_block_state(&mut self, pos: &Vector3<i32>, block_state: &BlockState) {
        if (pos.x >> 4) != self.x || (pos.z >> 4) != self.z {
            return;
        }
        Self::set_block_state(self, pos.x, pos.y, pos.z, block_state);
    }

    fn add_block_entity(&mut self, pos: &Vector3<i32>, nbt: NbtCompound) {
        if (pos.x >> 4) != self.x || (pos.z >> 4) != self.z {
            return;
        }
        Self::add_block_entity(self, nbt);
    }

    fn get_top_y(&self, heightmap: &HeightMap, x: i32, z: i32) -> i32 {
        Self::get_top_y(self, heightmap, x, z)
    }

    fn top_motion_blocking_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        Self::top_motion_blocking_block_height_exclusive(self, x, z)
    }

    fn top_motion_blocking_block_no_leaves_height_exclusive(&self, x: i32, z: i32) -> i32 {
        Self::top_motion_blocking_block_no_leaves_height_exclusive(self, x, z)
    }

    fn top_block_height_exclusive(&self, x: i32, z: i32) -> i32 {
        Self::top_block_height_exclusive(self, x, z)
    }

    fn ocean_floor_height_exclusive(&self, x: i32, z: i32) -> i32 {
        Self::ocean_floor_height_exclusive(self, x, z)
    }

    fn get_biome_for_terrain_gen(&self, x: i32, y: i32, z: i32) -> &'static Biome {
        self.get_terrain_gen_biome(x, y, z)
    }

    fn get_terrain_gen_biome_id(&self, x: i32, y: i32, z: i32) -> u8 {
        Self::get_terrain_gen_biome_id(self, x, y, z)
    }

    fn get_blending_data(
        &self,
        chunk_x: i32,
        chunk_z: i32,
    ) -> Option<&crate::generation::blender::blending_data::BlendingData> {
        if chunk_x == self.x && chunk_z == self.z {
            self.blending_data.as_ref()
        } else {
            None
        }
    }

    fn is_air(&self, local_pos: &Vector3<i32>) -> bool {
        is_air(GenerationCache::get_block_state(self, local_pos))
    }
}

impl Cache {
    #[must_use]
    pub fn new(x: i32, z: i32, size: i32) -> Self {
        Self {
            x,
            z,
            size,
            chunks: Vec::with_capacity((size * size) as usize),
        }
    }
    pub fn advance(
        &mut self,
        stage: StagedChunkEnum,
        generator: &generator::WorldGenerator,
        block_registry: &dyn WorldPortalExt,
        lighting_config: &LightingEngineConfig,
    ) {
        let mid = ((self.size * self.size) >> 1) as usize;
        match &self.chunks[mid] {
            Chunk::Level(_) => return,
            Chunk::Proto(chunk) if chunk.stage >= stage => return,
            Chunk::Proto(_) => {}
        }
        match stage {
            StagedChunkEnum::Empty => panic!("empty stage"),
            StagedChunkEnum::StructureStart => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .set_structure_starts(noise_gen);
                }
                generator::WorldGenerator::Flat(_) => {}
            },
            StagedChunkEnum::StructureReferences => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .set_structure_references(noise_gen);
                }
                generator::WorldGenerator::Flat(_) => {}
            },
            StagedChunkEnum::Biomes => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .step_to_biomes(noise_gen);
                }
                generator::WorldGenerator::Flat(flat_gen) => {
                    flat_gen.step_to_biomes(self.chunks[mid].get_proto_chunk_mut());
                }
            },
            StagedChunkEnum::Noise => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .step_to_noise(noise_gen);
                }
                generator::WorldGenerator::Flat(flat_gen) => {
                    flat_gen.step_to_noise(self.chunks[mid].get_proto_chunk_mut());
                }
            },
            StagedChunkEnum::Surface => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .step_to_surface(noise_gen);
                }
                generator::WorldGenerator::Flat(flat_gen) => {
                    flat_gen.step_to_surface(self.chunks[mid].get_proto_chunk_mut());
                }
            },
            StagedChunkEnum::Carvers => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    self.chunks[mid]
                        .get_proto_chunk_mut()
                        .step_to_carvers(noise_gen);
                }
                generator::WorldGenerator::Flat(flat_gen) => {
                    flat_gen.step_to_carvers(self.chunks[mid].get_proto_chunk_mut());
                }
            },
            StagedChunkEnum::Features => match generator {
                generator::WorldGenerator::Noise(noise_gen) => {
                    ProtoChunk::generate_features_and_structure(
                        self,
                        block_registry,
                        &noise_gen.random_config,
                    );
                }
                generator::WorldGenerator::Flat(_) => {
                    self.chunks[mid].get_proto_chunk_mut().stage = StagedChunkEnum::Features;
                }
            },
            StagedChunkEnum::Lighting => {
                let mut engine = crate::lighting::LightEngine::new();
                engine.initialize_light(self, lighting_config);
                // Only set stage to Lighting if it wasn't already at Lighting or higher
                // (initialize_light may short-circuit for already-lit chunks)
                let chunk = self.chunks[mid].get_proto_chunk_mut();
                if chunk.stage < StagedChunkEnum::Lighting {
                    chunk.stage = StagedChunkEnum::Lighting;
                }
                // Engine's internal state is cleared by initialize_light() and will be dropped here
                drop(engine);
            }
            StagedChunkEnum::Spawn => {
                ProtoChunk::spawn_mobs(self, block_registry);
            }
            StagedChunkEnum::Full => {
                let chunk = self.chunks[mid].get_proto_chunk_mut();
                debug_assert_eq!(chunk.stage, StagedChunkEnum::Spawn);
                chunk.stage = StagedChunkEnum::Full;
                self.chunks[mid].upgrade_to_level_chunk(generator.dimension(), lighting_config);
            }
            StagedChunkEnum::None => {}
        }
    }
}

#[cfg(test)]
mod proto_chunk_cache_tests {
    //! Exercises single-chunk [`GenerationCache`] impl on [`ProtoChunk`] used by jigsaw feature placement
    use crate::chunk_system::chunk_state::StagedChunkEnum;
    use crate::generation::proto_chunk::GenerationCache;
    use crate::generation::{generator::WorldGenerator, get_world_gen, proto_chunk::ProtoChunk};
    use pumpkin_data::Block;
    use pumpkin_data::dimension::Dimension;
    use pumpkin_util::math::vector3::Vector3;
    use pumpkin_util::world_seed::Seed;

    fn gen_chunk(cx: i32, cz: i32) -> ProtoChunk {
        let world_gen = get_world_gen(Seed(0), Dimension::OVERWORLD, false, Vec::new(), String::new());
        let mut chunk = ProtoChunk::new(cx, cz, &world_gen);
        let WorldGenerator::Noise(generator) = &*world_gen else {
            unreachable!()
        };
        chunk.step_to_biomes(generator);
        chunk.stage = StagedChunkEnum::StructureReferences;
        chunk.step_to_noise(generator);
        chunk
    }

    #[test]
    fn chunk_lookups_resolve_only_own_column() {
        let (cx, cz) = (3, 5);
        let mut chunk = gen_chunk(cx, cz);

        assert!(GenerationCache::get_chunk(&chunk, cx, cz).is_some());
        assert!(GenerationCache::get_chunk(&chunk, cx + 1, cz).is_none());
        assert!(GenerationCache::try_get_proto_chunk(&chunk, cx, cz + 1).is_none());
        assert!(GenerationCache::get_chunk_mut(&mut chunk, cx, cz).is_some());
        assert!(GenerationCache::get_chunk_mut(&mut chunk, cx, cz + 2).is_none());
    }

    #[test]
    fn out_of_chunk_reads_return_air_without_aliasing() {
        let (cx, cz) = (3, 5);
        let chunk = gen_chunk(cx, cz);

        // In-bounds: trait accessor agrees with the inherent one
        let in_pos = Vector3::new(cx * 16 + 8, 64, cz * 16 + 8);
        assert_eq!(
            GenerationCache::get_block_state(&chunk, &in_pos),
            ProtoChunk::get_block_state(&chunk, &in_pos)
        );

        // Neighbouring chunk shares same local (x & 15, z & 15);
        // inherent accessor would alias it into this column, trait accessor must not
        let neighbour_pos = Vector3::new((cx + 1) * 16 + 8, 64, cz * 16 + 8);
        assert_eq!(
            GenerationCache::get_block_state(&chunk, &neighbour_pos),
            pumpkin_data::BlockStateId::AIR
        );
        assert!(GenerationCache::is_air(&chunk, &neighbour_pos));
    }

    #[test]
    fn out_of_chunk_writes_are_dropped() {
        let (cx, cz) = (3, 5);
        let mut chunk = gen_chunk(cx, cz);

        let in_pos = Vector3::new(cx * 16 + 8, 70, cz * 16 + 8);
        let before = GenerationCache::get_block_state(&chunk, &in_pos);

        // Writing to neighbouring chunk at the same local column must not touch this chunk
        let neighbour_pos = Vector3::new((cx + 2) * 16 + 8, 70, cz * 16 + 8);
        GenerationCache::set_block_state(&mut chunk, &neighbour_pos, Block::BEDROCK.default_state);
        assert_eq!(
            GenerationCache::get_block_state(&chunk, &in_pos),
            before,
            "out-of-chunk write must not alias into this chunk"
        );

        // An in-bounds write is applied
        GenerationCache::set_block_state(&mut chunk, &in_pos, Block::BEDROCK.default_state);
        assert_eq!(
            GenerationCache::get_block_state(&chunk, &in_pos),
            Block::BEDROCK.default_state.id
        );
    }
}
