use pumpkin_data::chunk::Biome;
use pumpkin_data::dimension::Dimension;
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_world::biome::end::TheEndBiomeSupplier;
use pumpkin_world::biome::{BiomeSupplier, MultiNoiseBiomeSupplier};
use pumpkin_world::chunk::dynamic_biome::DYNAMIC_BIOMES;
use pumpkin_world::generation::biome_coords;
use pumpkin_world::generation::generator::WorldGenerator;
use pumpkin_world::generation::noise::router::multi_noise_sampler::{
    MultiNoiseSampler, MultiNoiseSamplerBuilderOptions,
};

use crate::command::CommandResult;
use crate::command::args::FindArg;
use crate::command::args::resource::biome::BiomeArgumentConsumer;
use crate::command::dispatcher::CommandError;
use crate::command::tree::CommandTree;
use crate::command::tree::builder::{argument, literal};
use crate::command::{CommandExecutor, CommandSender, ConsumedArgs};

const NAMES: [&str; 1] = ["locate"];
const DESCRIPTION: &str = "Locate the nearest generated feature or biome.";
const ARG_BIOME: &str = "biome";

/// `/locate biome` scans horizontally out to 6400 blocks
const MAX_RADIUS_BLOCKS: i32 = 6400;
/// Sample every 64 blocks (16 biome cells)
const STEP_BLOCKS: i32 = 64;

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

pub fn init_command_tree() -> CommandTree {
    CommandTree::new(NAMES, DESCRIPTION).then(
        literal("biome")
            .then(argument(ARG_BIOME, BiomeArgumentConsumer).execute(LocateBiomeExecutor)),
    )
}
