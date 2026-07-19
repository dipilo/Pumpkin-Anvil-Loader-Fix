use pumpkin_data::chunk::Biome;
use pumpkin_data::dimension::Dimension;
use pumpkin_data::structures::{
    RandomSpreadStructurePlacement, Structure, StructureKeys, StructurePlacementType, StructureSet,
};
use pumpkin_util::math::floor_div;
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_world::biome::end::TheEndBiomeSupplier;
use pumpkin_world::biome::{BiomeSupplier, MultiNoiseBiomeSupplier};
use pumpkin_world::chunk::dynamic_biome::DYNAMIC_BIOMES;
use pumpkin_world::generation::biome_coords;
use pumpkin_world::generation::generator::{VanillaGenerator, WorldGenerator};
use pumpkin_world::generation::noise::router::multi_noise_sampler::{
    MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
};
use pumpkin_world::generation::noise::router::surface_height_sampler::{
    SurfaceHeightEstimateSampler, SurfaceHeightSamplerBuilderOptions,
};
use pumpkin_world::generation::positions::chunk_pos;
use pumpkin_world::generation::structure::lazily_generate_structure;
use pumpkin_world::generation::structure::placement::get_structure_chunk_in_region;
use pumpkin_world::generation::structure::structures::{
    StructureGeneratorContext, create_chunk_random,
};

use crate::command::CommandResult;
use crate::command::args::FindArg;
use crate::command::args::resource::biome::BiomeArgumentConsumer;
use crate::command::args::resource_location::ResourceLocationArgumentConsumer;
use crate::command::dispatcher::CommandError;
use crate::command::tree::CommandTree;
use crate::command::tree::builder::{argument, literal};
use crate::command::{CommandExecutor, CommandSender, ConsumedArgs};

const NAMES: [&str; 1] = ["locate"];
const DESCRIPTION: &str = "Locate the nearest generated feature or biome.";
const ARG_BIOME: &str = "biome";
const ARG_STRUCTURE: &str = "structure";

/// `/locate biome` scans horizontally out to 6400 blocks
const MAX_RADIUS_BLOCKS: i32 = 6400;
/// Sample every 64 blocks (16 biome cells)
const STEP_BLOCKS: i32 = 64;

/// `/locate structure` scans outward this many placement rings
const MAX_STRUCTURE_RING: i32 = 40;

struct LocateBiomeExecutor;

impl CommandExecutor for LocateBiomeExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a crate::server::Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let target = BiomeArgumentConsumer::find_arg(args, ARG_BIOME)?;
            let Some(origin) = sender.position() else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "This command must be run from a position (e.g. as a player).",
                )));
            };
            let Some(world) = sender.world() else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "No world is available.",
                )));
            };
            // flat worlds have no biome climate to search
            let WorldGenerator::Noise(generator) = &*world.level.world_gen else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "This world type has no biome climate to locate.",
                )));
            };

            // Resolve the requested biome to its runtime id
            let stripped = target.strip_prefix("minecraft:").unwrap_or(target);
            let target_id = Biome::from_name(stripped)
                .map(|b| b.id)
                .or_else(|| DYNAMIC_BIOMES.read().unwrap().lookup(target));
            let Some(target_id) = target_id else {
                return Err(CommandError::CommandFailed(TextComponent::text(format!(
                    "Unknown biome: {target}"
                ))));
            };

            let mut sampler = MultiNoiseSampler::generate(
                &generator.base_router.multi_noise,
                &MultiNoiseSamplerBuilderOptions::new(0, 0, 1),
            );
            let datapack = generator.datapack_biome_supplier;
            let overworld = MultiNoiseBiomeSupplier::OVERWORLD;
            let nether = MultiNoiseBiomeSupplier::NETHER;
            let end = TheEndBiomeSupplier;
            let vanilla: &dyn BiomeSupplier = if generator.dimension == Dimension::THE_END {
                &end
            } else if generator.dimension == Dimension::THE_NETHER {
                &nether
            } else {
                &overworld
            };

            let origin_bx = biome_coords::from_block(origin.x.floor() as i32);
            let origin_bz = biome_coords::from_block(origin.z.floor() as i32);
            let by = biome_coords::from_block(origin.y.floor() as i32);
            let step = biome_coords::from_block(STEP_BLOCKS).max(1);
            let max_ring = biome_coords::from_block(MAX_RADIUS_BLOCKS) / step;

            // Square spiral outward; return the first (nearest-by-ring) match
            let mut found: Option<(i32, i32)> = None;
            'search: for ring in 0..=max_ring {
                for (gx, gz) in ring_offsets(ring) {
                    let bx = origin_bx + gx * step;
                    let bz = origin_bz + gz * step;
                    let id = match datapack {
                        Some(supplier) => {
                            supplier.biome_id(&sampler.sample(bx, by, bz).convert_to_list())
                        }
                        None => vanilla.biome(bx, by, bz, &mut sampler).id,
                    };
                    if id == target_id {
                        found = Some((bx, bz));
                        break 'search;
                    }
                }
            }

            match found {
                Some((bx, bz)) => {
                    let block_x = biome_coords::to_block(bx);
                    let block_z = biome_coords::to_block(bz);
                    let dx = f64::from(block_x) - origin.x;
                    let dz = f64::from(block_z) - origin.z;
                    let distance = dx.hypot(dz).round() as i64;
                    sender
                        .send_message(
                            TextComponent::text(format!(
                                "The nearest {target} is at [{block_x}, ~, {block_z}] ({distance} blocks away)"
                            ))
                            .color_named(NamedColor::Green),
                        )
                        .await;
                    Ok(distance as i32)
                }
                None => Err(CommandError::CommandFailed(TextComponent::text(format!(
                    "Could not find biome {target} within {MAX_RADIUS_BLOCKS} blocks"
                )))),
            }
        })
    }
}

/// Offsets `(gx, gz)` on the square ring at Chebyshev radius `ring`
fn ring_offsets(ring: i32) -> Vec<(i32, i32)> {
    if ring == 0 {
        return vec![(0, 0)];
    }
    let mut out = Vec::new();
    // Top and bottom rows (full width)
    for gx in -ring..=ring {
        out.push((gx, -ring));
        out.push((gx, ring));
    }
    // Left and right columns (excluding the corners already added)
    for gz in (-ring + 1)..ring {
        out.push((-ring, gz));
        out.push((ring, gz));
    }
    out
}

/// The registry id for a built-in structure
const fn structure_key_id(key: StructureKeys) -> &'static str {
    match key {
        StructureKeys::PillagerOutpost => "pillager_outpost",
        StructureKeys::Mineshaft => "mineshaft",
        StructureKeys::MineshaftMesa => "mineshaft_mesa",
        StructureKeys::Mansion => "mansion",
        StructureKeys::JunglePyramid => "jungle_pyramid",
        StructureKeys::DesertPyramid => "desert_pyramid",
        StructureKeys::Igloo => "igloo",
        StructureKeys::Shipwreck => "shipwreck",
        StructureKeys::ShipwreckBeached => "shipwreck_beached",
        StructureKeys::SwampHut => "swamp_hut",
        StructureKeys::Stronghold => "stronghold",
        StructureKeys::Monument => "monument",
        StructureKeys::OceanRuinCold => "ocean_ruin_cold",
        StructureKeys::OceanRuinWarm => "ocean_ruin_warm",
        StructureKeys::Fortress => "fortress",
        StructureKeys::NetherFossil => "nether_fossil",
        StructureKeys::EndCity => "end_city",
        StructureKeys::BuriedTreasure => "buried_treasure",
        StructureKeys::BastionRemnant => "bastion_remnant",
        StructureKeys::VillagePlains => "village_plains",
        StructureKeys::VillageDesert => "village_desert",
        StructureKeys::VillageSavanna => "village_savanna",
        StructureKeys::VillageSnowy => "village_snowy",
        StructureKeys::VillageTaiga => "village_taiga",
        StructureKeys::RuinedPortal => "ruined_portal",
        StructureKeys::RuinedPortalDesert => "ruined_portal_desert",
        StructureKeys::RuinedPortalJungle => "ruined_portal_jungle",
        StructureKeys::RuinedPortalSwamp => "ruined_portal_swamp",
        StructureKeys::RuinedPortalMountain => "ruined_portal_mountain",
        StructureKeys::RuinedPortalOcean => "ruined_portal_ocean",
        StructureKeys::RuinedPortalNether => "ruined_portal_nether",
        StructureKeys::AncientCity => "ancient_city",
        StructureKeys::TrailRuins => "trail_ruins",
        StructureKeys::TrialChambers => "trial_chambers",
    }
}

/// Resolves a single structure registry id to its key
fn structure_key_from_id(id: &str) -> Option<StructureKeys> {
    Some(match id {
        "pillager_outpost" => StructureKeys::PillagerOutpost,
        "mineshaft" => StructureKeys::Mineshaft,
        "mineshaft_mesa" => StructureKeys::MineshaftMesa,
        "mansion" => StructureKeys::Mansion,
        "jungle_pyramid" => StructureKeys::JunglePyramid,
        "desert_pyramid" => StructureKeys::DesertPyramid,
        "igloo" => StructureKeys::Igloo,
        "shipwreck" => StructureKeys::Shipwreck,
        "shipwreck_beached" => StructureKeys::ShipwreckBeached,
        "swamp_hut" => StructureKeys::SwampHut,
        "stronghold" => StructureKeys::Stronghold,
        "monument" => StructureKeys::Monument,
        "ocean_ruin_cold" => StructureKeys::OceanRuinCold,
        "ocean_ruin_warm" => StructureKeys::OceanRuinWarm,
        "fortress" => StructureKeys::Fortress,
        "nether_fossil" => StructureKeys::NetherFossil,
        "end_city" => StructureKeys::EndCity,
        "buried_treasure" => StructureKeys::BuriedTreasure,
        "bastion_remnant" => StructureKeys::BastionRemnant,
        "village_plains" => StructureKeys::VillagePlains,
        "village_desert" => StructureKeys::VillageDesert,
        "village_savanna" => StructureKeys::VillageSavanna,
        "village_snowy" => StructureKeys::VillageSnowy,
        "village_taiga" => StructureKeys::VillageTaiga,
        "ruined_portal" => StructureKeys::RuinedPortal,
        "ruined_portal_desert" => StructureKeys::RuinedPortalDesert,
        "ruined_portal_jungle" => StructureKeys::RuinedPortalJungle,
        "ruined_portal_swamp" => StructureKeys::RuinedPortalSwamp,
        "ruined_portal_mountain" => StructureKeys::RuinedPortalMountain,
        "ruined_portal_ocean" => StructureKeys::RuinedPortalOcean,
        "ruined_portal_nether" => StructureKeys::RuinedPortalNether,
        "ancient_city" => StructureKeys::AncientCity,
        "trail_ruins" => StructureKeys::TrailRuins,
        "trial_chambers" => StructureKeys::TrialChambers,
        _ => return None,
    })
}

/// Resolves the `#`-prefixed structure tags that group the multi-variant vanilla structures
fn structure_keys_from_tag(tag: &str) -> Option<&'static [StructureKeys]> {
    use StructureKeys::{
        Mineshaft, MineshaftMesa, OceanRuinCold, OceanRuinWarm, RuinedPortal, RuinedPortalDesert,
        RuinedPortalJungle, RuinedPortalMountain, RuinedPortalNether, RuinedPortalOcean,
        RuinedPortalSwamp, Shipwreck, ShipwreckBeached, VillageDesert, VillagePlains,
        VillageSavanna, VillageSnowy, VillageTaiga,
    };
    Some(match tag {
        "village" => &[
            VillagePlains,
            VillageDesert,
            VillageSavanna,
            VillageSnowy,
            VillageTaiga,
        ],
        "ocean_ruin" => &[OceanRuinCold, OceanRuinWarm],
        "shipwreck" => &[Shipwreck, ShipwreckBeached],
        "mineshaft" => &[Mineshaft, MineshaftMesa],
        "ruined_portal" => &[
            RuinedPortal,
            RuinedPortalDesert,
            RuinedPortalJungle,
            RuinedPortalSwamp,
            RuinedPortalMountain,
            RuinedPortalOcean,
            RuinedPortalNether,
        ],
        _ => return None,
    })
}

/// Parses `/locate structure <id>` argument into set of structure keys to search for
fn parse_structure_arg(arg: &str) -> Option<Vec<StructureKeys>> {
    arg.strip_prefix('#').map_or_else(
        || {
            let id = arg.strip_prefix("minecraft:").unwrap_or(arg);
            structure_key_from_id(id).map(|key| vec![key])
        },
        |tag| {
            let tag = tag.strip_prefix("minecraft:").unwrap_or(tag);
            structure_keys_from_tag(tag).map(<[StructureKeys]>::to_vec)
        },
    )
}

/// A group of requested structures that share a single placement
struct PlacementGroup {
    placement: &'static RandomSpreadStructurePlacement,
    salt: u32,
    keys: Vec<StructureKeys>,
}

/// Builds the searchable placement groups for the requested structures
/// Reports whether any requested structure used a concentric-ring placement (strongholds)
fn build_placement_groups(keys: &[StructureKeys]) -> (Vec<PlacementGroup>, bool) {
    let mut groups: Vec<PlacementGroup> = Vec::new();
    let mut concentric_skipped = false;
    for &key in keys {
        let Some(set) = find_set_for_key(key) else {
            continue;
        };
        match &set.placement.placement_type {
            StructurePlacementType::RandomSpread(placement) => {
                if let Some(group) = groups
                    .iter_mut()
                    .find(|g| std::ptr::eq(g.placement, placement))
                {
                    group.keys.push(key);
                } else {
                    groups.push(PlacementGroup {
                        placement,
                        salt: set.placement.salt,
                        keys: vec![key],
                    });
                }
            }
            StructurePlacementType::ConcentricRings(_) => concentric_skipped = true,
        }
    }
    (groups, concentric_skipped)
}

/// Searches outward for the nearest structure among `groups`
fn find_nearest_structure(
    generator: &VanillaGenerator,
    groups: &[PlacementGroup],
    origin_x: f64,
    origin_z: f64,
) -> Option<(StructureKeys, i32, i32, i64)> {
    let seed = generator.random_config.seed as i64;
    let settings = generator.settings;
    let noise_router = &generator.base_router;

    // Vanilla biome suppliers by dimension
    let overworld = MultiNoiseBiomeSupplier::OVERWORLD;
    let nether = MultiNoiseBiomeSupplier::NETHER;
    let end = TheEndBiomeSupplier;
    let biome_supplier: &dyn BiomeSupplier = if generator.dimension == Dimension::THE_END {
        &end
    } else if generator.dimension == Dimension::THE_NETHER {
        &nether
    } else {
        &overworld
    };
    let mut multi_noise_sampler = MultiNoiseSampler::generate(
        &noise_router.multi_noise,
        &MultiNoiseSamplerBuilderOptions::new(0, 0, 0),
    );

    let origin_cx = (origin_x.floor() as i32) >> 4;
    let origin_cz = (origin_z.floor() as i32) >> 4;

    for ring in 0..=MAX_STRUCTURE_RING {
        let mut best: Option<(f64, StructureKeys, i32, i32)> = None;
        for group in groups {
            let origin_rx = floor_div(origin_cx, group.placement.spacing);
            let origin_rz = floor_div(origin_cz, group.placement.spacing);
            for rx_off in -ring..=ring {
                let x_edge = rx_off == -ring || rx_off == ring;
                for rz_off in -ring..=ring {
                    let z_edge = rz_off == -ring || rz_off == ring;
                    if !(x_edge || z_edge) {
                        continue;
                    }
                    let (cand_x, cand_z) = get_structure_chunk_in_region(
                        group.placement,
                        seed,
                        origin_rx + rx_off,
                        origin_rz + rz_off,
                        group.salt,
                    );
                    for &key in &group.keys {
                        // Computed directly so a read-only locate never feeds values back into chunk generation
                        let mut height_sampler =
                            build_height_sampler(noise_router, settings, cand_x, cand_z);
                        let context = StructureGeneratorContext {
                            seed,
                            chunk_x: cand_x,
                            chunk_z: cand_z,
                            random: create_chunk_random(seed, cand_x, cand_z),
                            sea_level: settings.sea_level,
                            min_y: settings.shape.min_y as i32,
                            height_sampler: Some(&mut height_sampler),
                            structure_key: Some(key),
                        };
                        if lazily_generate_structure(
                            &key,
                            Structure::get(&key),
                            context,
                            biome_supplier,
                            &mut multi_noise_sampler,
                        )
                        .is_some()
                        {
                            // Reports the placement chunk's corner as the locate pos
                            let block_x = chunk_pos::start_block_x(cand_x);
                            let block_z = chunk_pos::start_block_z(cand_z);
                            let distance =
                                (f64::from(block_x) - origin_x).hypot(f64::from(block_z) - origin_z);
                            if best.as_ref().is_none_or(|(bd, ..)| distance < *bd) {
                                best = Some((distance, key, block_x, block_z));
                            }
                        }
                    }
                }
            }
        }
        if let Some((distance, key, block_x, block_z)) = best {
            return Some((key, block_x, block_z, distance.round() as i64));
        }
    }
    None
}

struct LocateStructureExecutor;

impl CommandExecutor for LocateStructureExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        _server: &'a crate::server::Server,
        args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let target = ResourceLocationArgumentConsumer::find_arg(args, ARG_STRUCTURE)?;
            let Some(keys) = parse_structure_arg(target) else {
                return Err(CommandError::CommandFailed(TextComponent::text(format!(
                    "Unknown structure: {target}"
                ))));
            };
            let Some(origin) = sender.position() else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "This command must be run from a position (e.g. as a player).",
                )));
            };
            let Some(world) = sender.world() else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "No world is available.",
                )));
            };
            let WorldGenerator::Noise(generator) = &*world.level.world_gen else {
                return Err(CommandError::CommandFailed(TextComponent::text(
                    "This world type does not generate structures.",
                )));
            };

            let (groups, concentric_skipped) = build_placement_groups(&keys);
            if groups.is_empty() {
                let msg = if concentric_skipped {
                    "Locating concentric-ring structures (e.g. strongholds) is not yet supported."
                } else {
                    "That structure has no searchable placement."
                };
                return Err(CommandError::CommandFailed(TextComponent::text(msg)));
            }

            match find_nearest_structure(generator, &groups, origin.x, origin.z) {
                Some((key, block_x, block_z, distance)) => {
                    let name = format!("minecraft:{}", structure_key_id(key));
                    sender
                        .send_message(
                            TextComponent::text(format!(
                                "The nearest {name} is at [{block_x}, ~, {block_z}] ({distance} blocks away)"
                            ))
                            .color_named(NamedColor::Green),
                        )
                        .await;
                    Ok(distance as i32)
                }
                None => Err(CommandError::CommandFailed(TextComponent::text(format!(
                    "Could not find structure {target} nearby"
                )))),
            }
        })
    }
}

/// Finds structure set that contains `key`
fn find_set_for_key(key: StructureKeys) -> Option<&'static StructureSet> {
    StructureSet::ALL
        .iter()
        .find(|set| set.structures.iter().any(|entry| entry.structure == key))
}

/// Builds a surface-height estimator positioned at the given candidate chunk
fn build_height_sampler<'a>(
    noise_router: &'a pumpkin_world::generation::noise::router::proto_noise_router::ProtoNoiseRouters,
    settings: &'static pumpkin_data::chunk_gen_settings::GenerationSettings,
    chunk_x: i32,
    chunk_z: i32,
) -> SurfaceHeightEstimateSampler<'a> {
    SurfaceHeightEstimateSampler::generate(
        &noise_router.surface_estimator,
        &SurfaceHeightSamplerBuilderOptions::new(
            biome_coords::from_block(chunk_pos::start_block_x(chunk_x)),
            biome_coords::from_block(chunk_pos::start_block_z(chunk_z)),
            4,
            settings.shape.min_y as i32,
            settings.shape.height as i32,
            (settings.shape.height / settings.shape.vertical_cell_block_count() as u16) as usize,
        ),
    )
}

pub fn init_command_tree() -> CommandTree {
    CommandTree::new(NAMES, DESCRIPTION)
        .then(
            literal("biome")
                .then(argument(ARG_BIOME, BiomeArgumentConsumer).execute(LocateBiomeExecutor)),
        )
        .then(
            literal("structure").then(
                argument(ARG_STRUCTURE, ResourceLocationArgumentConsumer)
                    .execute(LocateStructureExecutor),
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        find_set_for_key, parse_structure_arg, structure_key_from_id, structure_key_id,
    };
    use pumpkin_data::structures::StructureSet;

    /// Every structure that appears in a set must round-trip through the id table and resolve back to its owning set
    #[test]
    fn structure_key_tables_round_trip() {
        for set in StructureSet::ALL {
            for entry in set.structures {
                let key = entry.structure;
                let id = structure_key_id(key);
                assert_eq!(
                    structure_key_from_id(id),
                    Some(key),
                    "id `{id}` did not round-trip"
                );
                assert!(
                    find_set_for_key(key).is_some(),
                    "no set found for `{id}`"
                );
            }
        }
    }

    #[test]
    fn parse_accepts_ids_tags_and_namespaces() {
        // Namespaced and bare ids resolve to the same single structure
        assert_eq!(parse_structure_arg("village_plains").unwrap().len(), 1);
        assert_eq!(
            parse_structure_arg("minecraft:village_plains").unwrap().len(),
            1
        );
        // A structure tag expands to every variant
        assert_eq!(parse_structure_arg("#minecraft:village").unwrap().len(), 5);
        assert_eq!(parse_structure_arg("#village").unwrap().len(), 5);
        // Unknown ids are rejected
        assert!(parse_structure_arg("terralith:fortified_village").is_none());
        assert!(parse_structure_arg("not_a_structure").is_none());
    }

    /// Build a real vanilla overworld generator and confirm the full search pipeline 
    /// locates a village near the origin
    /// Ignored because it builds a generator and expands jigsaws; run with `--ignored`
    #[test]
    #[ignore = "builds a full vanilla generator; run explicitly with --ignored"]
    fn finds_a_vanilla_village_end_to_end() {
        use super::{build_placement_groups, find_nearest_structure};
        use pumpkin_data::dimension::Dimension;
        use pumpkin_util::world_seed::Seed;
        use pumpkin_world::generation::generator::{GeneratorInit, VanillaGenerator};

        let generator = VanillaGenerator::new(Seed(1), Dimension::OVERWORLD);
        let keys = parse_structure_arg("#minecraft:village").unwrap();
        let (groups, _) = build_placement_groups(&keys);
        let found = find_nearest_structure(&generator, &groups, 0.0, 0.0);
        assert!(found.is_some(), "expected to locate a village near origin");
    }
}
