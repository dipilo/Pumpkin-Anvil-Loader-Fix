use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_world::chunk::dynamic_biome::DYNAMIC_BIOMES;
use pumpkin_world::generation::datapack::active_worldgen_counts;
use pumpkin_world::level::resolve_datapack_selection;

use crate::command::CommandResult;
use crate::command::tree::CommandTree;
use crate::command::tree::builder::literal;
use crate::command::{CommandExecutor, CommandSender, ConsumedArgs};

const NAMES: [&str; 1] = ["datapack"];
const DESCRIPTION: &str = "Manage the world's data packs.";

/// Resolve the world root folder for the first loaded world,
/// or send a message and return `None`
async fn world_root(
    sender: &CommandSender,
    server: &crate::server::Server,
) -> Option<std::path::PathBuf> {
    let worlds = server.worlds.load();
    let Some(world) = worlds.first() else {
        sender
            .send_message(TextComponent::text("No world is loaded."))
            .await;
        return None;
    };
    Some(world.level.level_folder.root_folder.clone())
}

fn pack_links(ids: &[String], color: NamedColor) -> TextComponent {
    let mut root = TextComponent::text("");
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            root = root.add_text(", ");
        }
        root = root.add_child(TextComponent::text(format!("[{id}]")).color_named(color));
    }
    root
}

/// `/datapack list [available|enabled]`
enum ListMode {
    All,
    Available,
    Enabled,
}

struct ListExecutor(ListMode);

impl ListExecutor {
    async fn list_enabled(&self, sender: &CommandSender, enabled: &[String]) {
        if enabled.is_empty() {
            sender
                .send_message(TextComponent::text("There are no data packs enabled"))
                .await;
        } else {
            sender
                .send_message(
                    TextComponent::text(format!(
                        "There are {} data pack(s) enabled: ",
                        enabled.len()
                    ))
                    .add_child(pack_links(enabled, NamedColor::Green)),
                )
                .await;
        }
    }

    async fn list_available(&self, sender: &CommandSender, available: &[String]) {
        if available.is_empty() {
            sender
                .send_message(TextComponent::text("There are no more data packs available"))
                .await;
        } else {
            sender
                .send_message(
                    TextComponent::text(format!(
                        "There are {} data pack(s) available: ",
                        available.len()
                    ))
                    .add_child(pack_links(available, NamedColor::Red)),
                )
                .await;
        }
    }
}

impl CommandExecutor for ListExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        server: &'a crate::server::Server,
        _args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let Some(root) = world_root(sender, server).await else {
                return Ok(0);
            };
            let selection = resolve_datapack_selection(&root);

            // Vanilla `listPacks` = listEnabledPacks + listAvailablePacks, in that order
            if matches!(self.0, ListMode::All | ListMode::Enabled) {
                self.list_enabled(sender, &selection.enabled).await;
            }
            if matches!(self.0, ListMode::All | ListMode::Available) {
                self.list_available(sender, &selection.available).await;
            }

            Ok(1)
        })
    }
}

/// `/datapack status`
/// a fork-specific extension (vanilla has no such subcommand)
/// the world-generation status dump
struct StatusExecutor;

impl CommandExecutor for StatusExecutor {
    fn execute<'a>(
        &'a self,
        sender: &'a CommandSender,
        server: &'a crate::server::Server,
        _args: &'a ConsumedArgs<'a>,
    ) -> CommandResult<'a> {
        Box::pin(async move {
            let worlds = server.worlds.load();
            let Some(world) = worlds.first() else {
                sender
                    .send_message(TextComponent::text("No world is loaded."))
                    .await;
                return Ok(0);
            };
            let level = &world.level;

            sender
                .send_message(TextComponent::text("Datapacks").color_named(NamedColor::Gold))
                .await;

            // Effective selection (level.dat + vanilla auto-add)
            let selection = resolve_datapack_selection(&level.level_folder.root_folder);
            if selection.enabled.is_empty() {
                sender
                    .send_message(
                        TextComponent::text("  (none enabled)").color_named(NamedColor::Gray),
                    )
                    .await;
            } else {
                for id in &selection.enabled {
                    sender
                        .send_message(
                            TextComponent::text(format!("  - {id}")).color_named(NamedColor::Green),
                        )
                        .await;
                }
            }

            // Indexed worldgen registry summary
            match active_worldgen_counts() {
                Some(counts) => {
                    sender
                        .send_message(TextComponent::text(format!(
                            "  worldgen: {} noise_settings, {} density_functions, {} noise, {} dimension(s)",
                            counts.noise_settings,
                            counts.density_functions,
                            counts.noise,
                            counts.dimensions
                        )))
                        .await;
                }
                None => {
                    sender
                        .send_message(
                            TextComponent::text("  worldgen: none (vanilla)")
                                .color_named(NamedColor::Gray),
                        )
                        .await;
                }
            }

            let modded_biomes = DYNAMIC_BIOMES.read().unwrap().len();
            sender
                .send_message(TextComponent::text(format!(
                    "  modded biomes registered: {modded_biomes}"
                )))
                .await;

            // Live generation status for this world's dimension
            let generator = &level.world_gen;
            let terrain = if generator.uses_datapack_terrain() {
                "datapack"
            } else {
                "vanilla"
            };
            let biomes = generator.datapack_biome_supplier().map_or_else(
                || "vanilla".to_string(),
                |supplier| format!("datapack ({} entries)", supplier.len()),
            );
            sender
                .send_message(TextComponent::text(format!(
                    "  {}: terrain {terrain}, biomes {biomes}",
                    generator.dimension().minecraft_name
                )))
                .await;

            Ok(1)
        })
    }
}

pub fn init_command_tree() -> CommandTree {
    CommandTree::new(NAMES, DESCRIPTION)
        .then(
            literal("list")
                .execute(ListExecutor(ListMode::All))
                .then(literal("available").execute(ListExecutor(ListMode::Available)))
                .then(literal("enabled").execute(ListExecutor(ListMode::Enabled))),
        )
        .then(literal("status").execute(StatusExecutor))
}
