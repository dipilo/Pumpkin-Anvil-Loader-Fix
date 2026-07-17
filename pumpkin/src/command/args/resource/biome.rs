use pumpkin_data::chunk::Biome;
use pumpkin_protocol::java::client::play::{ArgumentType, CommandSuggestion, SuggestionProviders};
use pumpkin_world::chunk::dynamic_biome::DYNAMIC_BIOMES;

use crate::command::{
    CommandSender,
    args::{
        Arg, ArgumentConsumer, ConsumeResult, ConsumedArgs, DefaultNameArgConsumer, FindArg,
        GetClientSideArgParser, SuggestResult,
    },
    dispatcher::CommandError,
    tree::RawArgs,
};
use crate::server::Server;

/// Argument for a biome id
pub struct BiomeArgumentConsumer;

impl GetClientSideArgParser for BiomeArgumentConsumer {
    fn get_client_side_parser(&self) -> ArgumentType {
        ArgumentType::ResourceLocation
    }

    fn get_client_side_suggestion_type_override(&self) -> Option<SuggestionProviders> {
        // Force server-side suggestions so `suggest` below is consulted
        Some(SuggestionProviders::AskServer)
    }
}

impl ArgumentConsumer for BiomeArgumentConsumer {
    fn consume<'a, 'b>(
        &'a self,
        _sender: &'a CommandSender,
        _server: &'a Server,
        args: &'b mut RawArgs<'a>,
    ) -> ConsumeResult<'a> {
        let s_opt: Option<&'a str> = args.pop().map(|arg| arg.value);
        Box::pin(async move { s_opt.map(Arg::ResourceLocation) })
    }

    fn suggest<'a>(
        &'a self,
        _sender: &CommandSender,
        _server: &'a Server,
        _input: &'a str,
    ) -> SuggestResult<'a> {
        Box::pin(async move {
            // Vanilla biomes occupy the contiguous id run starting at 0
            let mut names: Vec<String> = (0u8..=u8::MAX)
                .map_while(Biome::from_id)
                .map(|b| format!("minecraft:{}", b.registry_id))
                .collect();
            // Datapack/dynamic biomes already carry their full namespaced name
            names.extend(DYNAMIC_BIOMES.read().unwrap().names());
            names.sort_unstable();
            names.dedup();
            let suggestions = names
                .into_iter()
                .map(|name| CommandSuggestion::new(name, None))
                .collect();
            Ok(Some(suggestions))
        })
    }
}

impl DefaultNameArgConsumer for BiomeArgumentConsumer {
    fn default_name(&self) -> &'static str {
        "biome"
    }
}

impl<'a> FindArg<'a> for BiomeArgumentConsumer {
    type Data = &'a str;

    fn find_arg(args: &'a ConsumedArgs, name: &str) -> Result<Self::Data, CommandError> {
        match args.get(name) {
            Some(Arg::ResourceLocation(data)) => Ok(data),
            _ => Err(CommandError::InvalidConsumption(Some(name.to_string()))),
        }
    }
}
