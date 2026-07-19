use pumpkin_data::dimension::Dimension;

use crate::ProtoChunk;
use crate::generation::generator::WorldGenerator;
use crate::world::WorldPortalExt;
use pumpkin_config::lighting::LightingEngineConfig;

use super::{Cache, Chunk, StagedChunkEnum};

pub fn generate_single_chunk(
    _dimension: &Dimension,
    _biome_mixer_seed: i64,
    generator: &WorldGenerator,
    block_registry: &dyn WorldPortalExt,
    chunk_x: i32,
    chunk_z: i32,
    target_stage: StagedChunkEnum,
) -> Chunk {
    let radius = target_stage.get_direct_radius();

    let mut cache = Cache::new(chunk_x - radius, chunk_z - radius, radius * 2 + 1);

    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let new_x = chunk_x + dx;
            let new_z = chunk_z + dz;

            let proto_chunk = Box::new(ProtoChunk::new(new_x, new_z, generator));

            cache.chunks.push(Chunk::Proto(proto_chunk));
        }
    }

    let stages = [
        StagedChunkEnum::Biomes,
        StagedChunkEnum::StructureStart,
        StagedChunkEnum::StructureReferences,
        StagedChunkEnum::Noise,
        StagedChunkEnum::Surface,
        StagedChunkEnum::Carvers,
        StagedChunkEnum::Features,
        StagedChunkEnum::Lighting,
        StagedChunkEnum::Spawn,
        StagedChunkEnum::Full,
    ];

    for &stage in &stages {
        if stage as u8 > target_stage as u8 {
            break;
        }

        cache.advance(
            stage,
            generator,
            block_registry,
            &LightingEngineConfig::Default,
        );
    }

    let mid = ((cache.size * cache.size) >> 1) as usize;
    cache.chunks.swap_remove(mid)
}

#[cfg(test)]
mod tests {
    use crate::biome::hash_seed;
    use crate::chunk_system::{StagedChunkEnum, generate_single_chunk};
    use crate::generation::get_world_gen;
    use crate::world::WorldPortalExt;
    use pumpkin_data::BlockStateId;
    use pumpkin_data::dimension::Dimension;
    use pumpkin_util::world_seed::Seed;
    use std::sync::Arc;

    struct BlockRegistry;
    impl WorldPortalExt for BlockRegistry {
        fn can_place_at(
            &self,
            _block: &pumpkin_data::Block,
            _state: &pumpkin_data::BlockState,
            _block_accessor: &dyn crate::world::BlockAccessor,
            _block_pos: &pumpkin_util::math::position::BlockPos,
        ) -> bool {
            true
        }

        fn mirror(
            &self,
            block: &pumpkin_data::Block,
            state_id: BlockStateId,
            mirror: pumpkin_data::Mirror,
        ) -> &'static pumpkin_data::BlockState {
            block.mirror(state_id, mirror)
        }

        fn rotate(
            &self,
            block: &pumpkin_data::Block,
            state_id: BlockStateId,
            rotation: pumpkin_data::Rotation,
        ) -> &'static pumpkin_data::BlockState {
            block.rotate(state_id, rotation)
        }

        fn spawn_mobs_for_chunk_generation(
            &self,
            _cache: &mut dyn crate::generation::proto_chunk::GenerationCache,
            _biome: &'static pumpkin_data::chunk::Biome,
            _chunk_x: i32,
            _chunk_z: i32,
        ) {
        }
    }

    #[test]
    fn generate_chunk_should_return() {
        let dimension = Dimension::OVERWORLD;
        let seed = Seed(42);
        let block_registry = Arc::new(BlockRegistry);
        let world_gen = get_world_gen(seed, dimension.clone(), false, Vec::new(), String::new());
        let biome_mixer_seed = hash_seed(world_gen.seed());

        let _ = generate_single_chunk(
            &dimension,
            biome_mixer_seed,
            &world_gen,
            block_registry.as_ref(),
            0,
            0,
            StagedChunkEnum::Full,
        );
    }

    /// generating chunks through the Features stage places datapack-driven decoration
    /// Ignored by default; set `WORLDGEN_PACK_DIR` to a datapacks folder
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr, clippy::too_many_lines)]
    fn datapack_features_place_blocks_in_generated_chunks() {
        use crate::chunk::dynamic_biome::{DYNAMIC_BIOMES, clear_dynamic_biomes};
        use crate::generation::datapack::features::{
            DatapackFeatureRegistry, clear_active_features, set_active_features,
        };
        use crate::generation::datapack::{
            WorldgenData, clear_active_worldgen, set_active_worldgen,
        };
        use pumpkin_util::math::vector3::Vector3;
        use std::path::PathBuf;

        // The test world's seed (Terralith 26.2 + tectonic + terratonic)
        const SEED: i64 = -12_969_086_726_167_675;

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        clear_dynamic_biomes();
        clear_active_features();
        {
            let mut reg = DYNAMIC_BIOMES.write().unwrap();
            reg.load_datapack_definitions(std::slice::from_ref(&dir));
            reg.register_datapack_biomes()
        };
        let data = WorldgenData::load(&[dir]);
        if let Some(features) = DatapackFeatureRegistry::build(data.raw()) {
            set_active_features(features);
        }
        set_active_worldgen(data);

        let dimension = Dimension::OVERWORLD;
        let block_registry = Arc::new(BlockRegistry);
        let world_gen = get_world_gen(
            Seed(SEED as u64),
            dimension.clone(),
            false,
            Vec::new(),
            String::new(),
        );
        let biome_mixer_seed = hash_seed(world_gen.seed());

        // Blocks only reachable via decoration
        // Histogram of queer surface blocks captures placed vegetation/ores/trees
        let is_terrain = |name: &str| {
            matches!(
                name,
                "air" | "cave_air" | "stone" | "deepslate" | "water" | "dirt" | "grass_block"
                    | "gravel" | "sand" | "sandstone" | "bedrock" | "granite" | "diorite"
                    | "andesite" | "tuff" | "snow_block" | "snow" | "packed_ice" | "ice"
                    | "coarse_dirt" | "podzol" | "calcite" | "netherrack" | "moss_block"
            )
        };

        let mut decoration_blocks = 0u64;
        let mut chunks_scanned = 0u32;
        let mut datapack_driven_chunks = 0u32;
        let mut block_hist: std::collections::HashMap<&'static str, u64> =
            std::collections::HashMap::new();
        'outer: for cx in -2..4 {
            for cz in -2..4 {
                let chunk = generate_single_chunk(
                    &dimension,
                    biome_mixer_seed,
                    &world_gen,
                    block_registry.as_ref(),
                    cx,
                    cz,
                    StagedChunkEnum::Features,
                );
                let super::Chunk::Proto(chunk) = chunk else {
                    continue;
                };
                chunks_scanned += 1;
                // Confirm chunk's biomes are actually datapack-driven
                let biomes: Vec<u8> = {
                    let mut v = chunk.flat_biome_map.to_vec();
                    v.sort_unstable();
                    v.dedup();
                    v
                };
                if crate::generation::datapack::features::any_datapack_biome(&biomes) {
                    datapack_driven_chunks += 1;
                }
                for x in (cx * 16)..(cx * 16 + 16) {
                    for z in (cz * 16)..(cz * 16 + 16) {
                        for y in 55..140 {
                            let name = chunk
                                .get_block_state(&Vector3::new(x, y, z))
                                .to_block_id()
                                .to_block()
                                .name;
                            if !is_terrain(name) {
                                decoration_blocks += 1;
                                *block_hist.entry(name).or_default() += 1;
                            }
                        }
                    }
                }
                if decoration_blocks > 500 {
                    break 'outer;
                }
            }
        }
        eprintln!(
            "datapack decoration placed {decoration_blocks} decoration block(s) across \
             {chunks_scanned} chunk(s) ({datapack_driven_chunks} datapack-driven):"
        );
        let mut hist: Vec<_> = block_hist.into_iter().collect();
        hist.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (name, count) in hist.into_iter().take(15) {
            eprintln!("  {count:>6}  {name}");
        }

        clear_active_worldgen();
        clear_active_features();
        clear_dynamic_biomes();
        assert!(
            datapack_driven_chunks > 0,
            "sampled chunks were not datapack-driven; check biome placement"
        );
        assert!(
            decoration_blocks > 0,
            "expected datapack decoration to place vegetation/feature blocks in modded biomes"
        );
    }

    /// Regression placed a shit ton of floating water blocks per `terralith:yellowstone` chunk
    /// `terralith:yellowstone/lakes` writes its `count` and `environment_scan` placement modifiers with bare type ids
    /// The codec dropped those, so water was dumped onto surface heightmap
    /// Locates yellowstone in the test world and asserts none remain
    /// Ignored by default; set `WORLDGEN_PACK_DIR` to a datapacks folder
    #[test]
    #[ignore = "requires local datapacks; set WORLDGEN_PACK_DIR"]
    #[allow(clippy::print_stderr, clippy::too_many_lines)]
    fn datapack_features_place_no_floating_water() {
        use crate::chunk::dynamic_biome::{DYNAMIC_BIOMES, clear_dynamic_biomes};
        use crate::generation::datapack::features::{
            DatapackFeatureRegistry, clear_active_features, set_active_features,
        };
        use crate::generation::datapack::{
            WorldgenData, clear_active_worldgen, set_active_worldgen,
        };
        use pumpkin_util::math::vector3::Vector3;
        use std::path::PathBuf;

        const SEED: i64 = -12_969_086_726_167_675;

        let Some(dir) = std::env::var_os("WORLDGEN_PACK_DIR").map(PathBuf::from) else {
            eprintln!("skipping: set WORLDGEN_PACK_DIR to a datapacks folder");
            return;
        };
        if !dir.is_dir() {
            eprintln!("skipping: {dir:?} not present");
            return;
        }

        clear_dynamic_biomes();
        clear_active_features();
        {
            let mut reg = DYNAMIC_BIOMES.write().unwrap();
            reg.load_datapack_definitions(std::slice::from_ref(&dir));
            reg.register_datapack_biomes()
        };
        let data = WorldgenData::load(&[dir]);
        if let Some(features) = DatapackFeatureRegistry::build(data.raw()) {
            set_active_features(features);
        }
        set_active_worldgen(data);

        let dimension = Dimension::OVERWORLD;
        let block_registry = Arc::new(BlockRegistry);
        let world_gen = get_world_gen(
            Seed(SEED as u64),
            dimension.clone(),
            false,
            Vec::new(),
            String::new(),
        );
        let biome_mixer_seed = hash_seed(world_gen.seed());

        let name_at = |chunk: &crate::ProtoChunk, x: i32, y: i32, z: i32| -> &'static str {
            chunk
                .get_block_state(&Vector3::new(x, y, z))
                .to_block_id()
                .to_block()
                .name
        };
        let is_air_name = |n: &str| matches!(n, "air" | "cave_air" | "void_air");

        // The biome we're hunting.
        let ys_id = DYNAMIC_BIOMES
            .read()
            .unwrap()
            .lookup("terralith:yellowstone")
            .expect("yellowstone should be registered");
        eprintln!("terralith:yellowstone id = {ys_id}");

        // Locate yellowstone
        let t0 = std::time::Instant::now();
        let mut ys_locs: Vec<(i32, i32)> = Vec::new();
        let mut gens = 0u32;
        'locate: for cx in (-260i32..260).step_by(4) {
            for cz in (-260i32..260).step_by(4) {
                gens += 1;
                let chunk = generate_single_chunk(
                    &dimension,
                    biome_mixer_seed,
                    &world_gen,
                    block_registry.as_ref(),
                    cx,
                    cz,
                    StagedChunkEnum::Biomes,
                );
                if let super::Chunk::Proto(c) = chunk
                    && c.flat_biome_map.contains(&ys_id)
                {
                    ys_locs.push((cx, cz));
                    if ys_locs.len() >= 12 {
                        break 'locate;
                    }
                }
                if gens >= 6000 {
                    break 'locate;
                }
            }
        }
        eprintln!(
            "locate: scanned {gens} chunks (Biomes) in {:?}, found {} yellowstone chunks: {:?}",
            t0.elapsed(),
            ys_locs.len(),
            ys_locs
        );

        assert!(
            !ys_locs.is_empty(),
            "could not locate any terralith:yellowstone chunk to test"
        );

        // Generate located yellowstone chunks through Features and assert there's no hovering water
        let mut hover = 0u64;
        for &(cx, cz) in ys_locs.iter().take(8) {
            let chunk = generate_single_chunk(
                &dimension,
                biome_mixer_seed,
                &world_gen,
                block_registry.as_ref(),
                cx,
                cz,
                StagedChunkEnum::Features,
            );
            let super::Chunk::Proto(chunk) = chunk else {
                continue;
            };
            let min_y = chunk.bottom_y() as i32;
            let top_y = min_y + chunk.height() as i32;
            for lx in 0..16 {
                for lz in 0..16 {
                    let x = cx * 16 + lx;
                    let z = cz * 16 + lz;
                    for y in (min_y + 1)..(top_y - 1) {
                        if name_at(&chunk, x, y, z) == "water"
                            && is_air_name(name_at(&chunk, x, y - 1, z))
                        {
                            hover += 1;
                            if hover <= 8 {
                                eprintln!("  floating water @ ({x},{y},{z})");
                            }
                        }
                    }
                }
            }
        }

        clear_active_worldgen();
        clear_active_features();
        clear_dynamic_biomes();

        assert_eq!(
            hover, 0,
            "found {hover} hovering water block(s) in yellowstone chunks; \
             the datapack feature codec is dropping placement modifiers again"
        );
    }

    #[test]
    fn configured_seed_generates_vanilla_ancient_city_chunk() {
        let dimension = Dimension::OVERWORLD;
        let seed = Seed(1_782_124_772_053_846_960);
        let block_registry = Arc::new(BlockRegistry);
        let world_gen = get_world_gen(seed, dimension.clone(), false, Vec::new(), String::new());
        let biome_mixer_seed = hash_seed(world_gen.seed());

        let chunk = generate_single_chunk(
            &dimension,
            biome_mixer_seed,
            &world_gen,
            block_registry.as_ref(),
            31,
            -12,
            StagedChunkEnum::Features,
        );
        let super::Chunk::Proto(chunk) = chunk else {
            panic!("features stage should return a proto chunk");
        };

        let mut city_blocks = 0;
        let mut jigsaw_blocks = 0;
        for x in 496..512 {
            for z in -192..-176 {
                for y in -64..320 {
                    let block = chunk
                        .get_block_state(&pumpkin_util::math::vector3::Vector3::new(x, y, z))
                        .to_block_id();
                    if [
                        pumpkin_data::Block::DEEPSLATE_BRICKS.id,
                        pumpkin_data::Block::POLISHED_DEEPSLATE.id,
                        pumpkin_data::Block::REINFORCED_DEEPSLATE.id,
                        pumpkin_data::Block::SCULK.id,
                    ]
                    .contains(&block)
                    {
                        city_blocks += 1;
                    }
                    if block == pumpkin_data::Block::JIGSAW.id {
                        jigsaw_blocks += 1;
                    }
                }
            }
        }

        assert!(
            city_blocks > 0,
            "reference chunk contains no Ancient City blocks"
        );
        assert_eq!(jigsaw_blocks, 0, "jigsaw blocks were not replaced");
    }
}
