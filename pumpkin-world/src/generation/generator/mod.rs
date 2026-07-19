use pumpkin_data::BlockState;
use pumpkin_data::chunk_gen_settings::GenerationSettings;
use pumpkin_data::dimension::Dimension;
use pumpkin_data::noise_router::{
    END_BASE_NOISE_ROUTER, NETHER_BASE_NOISE_ROUTER, OVERWORLD_BASE_NOISE_ROUTER,
};

use super::noise::router::proto_noise_router::ProtoNoiseRouters;
use crate::generation::proto_chunk::TerrainCache;
use crate::generation::{GlobalRandomConfig, Seed};

pub mod structure_finder;

pub trait GeneratorInit {
    fn new(seed: Seed, dimension: Dimension) -> Self;
}

use pumpkin_data::structures::{StructurePlacementCalculator, StructureSet};
use rustc_hash::FxHashMap;

pub mod flat;

#[derive(Clone, Debug)]
pub struct FlatLayer {
    pub block: String,
    pub height: i32,
}

pub enum WorldGenerator {
    Noise(Box<VanillaGenerator>),
    Flat(flat::FlatGenerator),
}

impl WorldGenerator {
    #[must_use]
    pub const fn dimension(&self) -> &Dimension {
        match self {
            Self::Noise(noise_gen) => &noise_gen.dimension,
            Self::Flat(flat_gen) => &flat_gen.dimension,
        }
    }

    #[must_use]
    pub const fn seed(&self) -> u64 {
        match self {
            Self::Noise(noise_gen) => noise_gen.random_config.seed,
            Self::Flat(flat_gen) => flat_gen.seed,
        }
    }

    #[must_use]
    pub const fn global_structure_cache(
        &self,
    ) -> Option<&crate::generation::structure::placement::GlobalStructureCache> {
        match self {
            Self::Noise(noise_gen) => Some(&noise_gen.global_structure_cache),
            Self::Flat(_) => None,
        }
    }

    /// Whether this dimension's terrain (noise router + settings) is datapack-driven
    #[must_use]
    pub const fn uses_datapack_terrain(&self) -> bool {
        match self {
            Self::Noise(noise_gen) => noise_gen.uses_datapack_terrain,
            Self::Flat(_) => false,
        }
    }

    /// The datapack multi-noise biome supplier
    #[must_use]
    pub const fn datapack_biome_supplier(
        &self,
    ) -> Option<&'static crate::generation::datapack::biome_placement::DatapackBiomeSupplier> {
        match self {
            Self::Noise(noise_gen) => noise_gen.datapack_biome_supplier,
            Self::Flat(_) => None,
        }
    }
}

pub struct VanillaGenerator {
    pub random_config: GlobalRandomConfig,
    pub base_router: ProtoNoiseRouters,
    pub dimension: Dimension,
    pub settings: &'static GenerationSettings,
    /// Datapack multi-noise biome placement
    pub datapack_biome_supplier:
        Option<&'static crate::generation::datapack::biome_placement::DatapackBiomeSupplier>,
    pub uses_datapack_terrain: bool,
    pub biome_mixer_seed: i64,

    pub terrain_cache: TerrainCache,

    pub default_block: &'static BlockState,

    pub global_structure_cache: crate::generation::structure::placement::GlobalStructureCache,
    pub structure_calculator: StructurePlacementCalculator,
    pub structure_allowed_biomes: FxHashMap<usize, Vec<u16>>,
}

impl GeneratorInit for VanillaGenerator {
    fn new(seed: Seed, dimension: Dimension) -> Self {
        // TODO: The generation settings contains (part of?) the noise routers too; do we keep the separate or
        // use only the generation settings?
        let vanilla_base = if dimension == Dimension::OVERWORLD {
            OVERWORLD_BASE_NOISE_ROUTER
        } else if dimension == Dimension::THE_NETHER {
            NETHER_BASE_NOISE_ROUTER
        } else if dimension == Dimension::THE_END {
            END_BASE_NOISE_ROUTER
        } else {
            tracing::error!("Unsupported dimension for noise router: {:?}", dimension);
            OVERWORLD_BASE_NOISE_ROUTER
        };

        // Prefer a datapack-driven router + settings when the active world's
        // datapacks fully define this dimension's noise setting
        let datapack = crate::generation::datapack::resolve_active_dimension(&dimension);
        let settings = datapack
            .as_ref()
            .map_or_else(|| GenerationSettings::from_dimension(&dimension), |d| d.settings);
            
        let random_config = GlobalRandomConfig::new(seed.0, settings.legacy_random_source);
        let terrain_cache = TerrainCache::from_random(&random_config);

        let default_block = settings.default_block;
        let base_router = datapack.as_ref().map_or_else(
            || ProtoNoiseRouters::generate(&vanilla_base, &random_config),
            |d| ProtoNoiseRouters::generate(&d.routers, &random_config),
        );
        let datapack_biome_supplier = datapack.as_ref().and_then(|d| d.biome_placement);
        let uses_datapack_terrain = datapack.is_some();
        let biome_mixer_seed = crate::biome::hash_seed(seed.0);

        let mut structure_allowed_biomes = FxHashMap::default();
        for (i, set) in StructureSet::ALL.iter().enumerate() {
            structure_allowed_biomes.insert(
                i,
                crate::generation::proto_chunk::ProtoChunk::get_allowed_biomes(set),
            );
        }

        Self {
            random_config,
            base_router,
            dimension,
            settings,
            datapack_biome_supplier,
            uses_datapack_terrain,
            biome_mixer_seed,
            terrain_cache,
            default_block,
            global_structure_cache:
                crate::generation::structure::placement::GlobalStructureCache::new(),
            structure_calculator: StructurePlacementCalculator::new(seed.0 as i64),
            structure_allowed_biomes,
        }
    }
}
