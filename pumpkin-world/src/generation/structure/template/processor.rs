use pumpkin_data::{Block, BlockId, BlockState, tag};
use pumpkin_util::{
    math::vector3::Vector3,
    random::{RandomGenerator, RandomImpl, hash_block_pos, legacy_rand::LegacyRand},
};
use serde::Deserialize;
use std::sync::{Arc, LazyLock};

use crate::ProtoChunk;
use crate::block::BlockStateCodec;
use crate::generation::rule::{
    RuleTest, block_match::BlockMatchRuleTest, block_state_match::BlockStateMatchRuleTest,
    random_block_match::RandomBlockMatchRuleTest,
    random_block_state_match::RandomBlockStateMatchRuleTest, tag_match::TagMatchRuleTest,
};

pub enum StructureProcessor {
    BlockRot { integrity: f32, blocks: BlockTag },
    Rules(Vec<ProcessorRule>),
    ProtectedBlocks(BlockTag),
}

/// A single rule of a `minecraft:rule` structure processor
pub struct ProcessorRule {
    input_predicate: RuleTest,
    location_predicate: RuleTest,
    output_state: &'static BlockState,
}

#[derive(Clone, Copy)]
pub enum BlockTag {
    AncientCityReplaceable,
    FeaturesCannotReplace,
}

impl BlockTag {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "#minecraft:ancient_city_replaceable" => Some(Self::AncientCityReplaceable),
            "#minecraft:features_cannot_replace" => Some(Self::FeaturesCannotReplace),
            _ => None,
        }
    }

    fn contains(self, block_id: BlockId) -> bool {
        block_id.has_tag(match self {
            Self::AncientCityReplaceable => tag::Block::MINECRAFT_ANCIENT_CITY_REPLACEABLE,
            Self::FeaturesCannotReplace => tag::Block::MINECRAFT_FEATURES_CANNOT_REPLACE,
        })
    }
}

impl StructureProcessor {
    #[must_use]
    pub fn process(
        &self,
        chunk: &ProtoChunk,
        pos: Vector3<i32>,
        state: &'static BlockState,
    ) -> Option<&'static BlockState> {
        let input_block = state.id.to_block_id();
        match self {
            Self::BlockRot { integrity, blocks } => {
                if !blocks.contains(input_block) {
                    return Some(state);
                }
                let mut random = LegacyRand::from_seed(hash_block_pos(pos.x, pos.y, pos.z) as u64);
                (random.next_f32() <= *integrity).then_some(state)
            }
            Self::Rules(rules) => {
                let mut random = RandomGenerator::Legacy(LegacyRand::from_seed(
                    hash_block_pos(pos.x, pos.y, pos.z) as u64,
                ));
                let input_state = state.id;
                let world_state = chunk.get_block_state(&pos);
                for rule in rules {
                    if rule.input_predicate.test(input_state, &mut random)
                        && rule.location_predicate.test(world_state, &mut random)
                    {
                        return Some(rule.output_state);
                    }
                }
                Some(state)
            }
            Self::ProtectedBlocks(blocks) => {
                let existing = chunk.get_block_state(&pos).to_block_id();
                (!blocks.contains(existing)).then_some(state)
            }
        }
    }
}

#[derive(Deserialize)]
struct RawProcessorList {
    processors: Vec<RawProcessor>,
}

#[derive(Deserialize)]
#[serde(tag = "processor_type")]
enum RawProcessor {
    #[serde(rename = "minecraft:block_rot")]
    BlockRot {
        integrity: f32,
        rottable_blocks: String,
    },
    #[serde(rename = "minecraft:rule")]
    Rule { rules: Vec<RawRule> },
    #[serde(rename = "minecraft:protected_blocks")]
    ProtectedBlocks { value: String },
}

#[derive(Deserialize)]
struct RawRule {
    input_predicate: RawRuleTest,
    location_predicate: RawRuleTest,
    output_state: BlockStateCodec,
}

/// A vanilla `RuleTest`, tagged by `predicate_type`
#[derive(Deserialize)]
#[serde(tag = "predicate_type")]
enum RawRuleTest {
    #[serde(rename = "minecraft:always_true")]
    AlwaysTrue,
    #[serde(rename = "minecraft:block_match")]
    BlockMatch { block: String },
    #[serde(rename = "minecraft:blockstate_match")]
    BlockStateMatch { block_state: BlockStateCodec },
    #[serde(rename = "minecraft:random_block_match")]
    RandomBlockMatch { block: String, probability: f32 },
    #[serde(rename = "minecraft:random_blockstate_match")]
    RandomBlockStateMatch {
        block_state: BlockStateCodec,
        probability: f32,
    },
    #[serde(rename = "minecraft:tag_match")]
    TagMatch { tag: String },
    /// Any unrecognized `predicate_type`
    #[serde(other)]
    Unknown,
}

impl RawRuleTest {
    /// Convert into runtime [`RuleTest`]
    fn build(self) -> Option<RuleTest> {
        Some(match self {
            Self::AlwaysTrue => RuleTest::AlwaysTrue,
            Self::BlockMatch { block } => RuleTest::BlockMatch(BlockMatchRuleTest {
                block: block_id_from_name(&block)?,
            }),
            Self::BlockStateMatch { block_state } => {
                RuleTest::BlockStateMatch(BlockStateMatchRuleTest {
                    block_state: block_state.get_state_id(),
                })
            }
            Self::RandomBlockMatch { block, probability } => {
                RuleTest::RandomBlockMatch(RandomBlockMatchRuleTest {
                    block: block_id_from_name(&block)?,
                    probability,
                })
            }
            Self::RandomBlockStateMatch {
                block_state,
                probability,
            } => RuleTest::RandomBlockStateMatch(RandomBlockStateMatchRuleTest {
                block_state: block_state.get_state_id(),
                probability,
            }),
            Self::TagMatch { tag } => RuleTest::TagMatch(TagMatchRuleTest {
                tag: resolve_block_tag(&tag)?,
            }),
            Self::Unknown => return None,
        })
    }
}

fn block_id_from_name(name: &str) -> Option<BlockId> {
    let stripped = name.strip_prefix("minecraft:").unwrap_or(name);
    Block::from_name(stripped).map(|block| block.id)
}

/// Resolve a block-tag name (`#minecraft:foo` / `minecraft:foo` / `foo`) to a runtime [`tag::Tag`]
fn resolve_block_tag(name: &str) -> Option<tag::Tag> {
    let stripped = name.strip_prefix('#').unwrap_or(name);
    let lookup = |key: &str| -> Option<tag::Tag> {
        let values = tag::get_tag_values(tag::RegistryKey::Block, key)?;
        let ids = tag::get_tag_ids(tag::RegistryKey::Block, key)?;
        Some((values, ids))
    };
    lookup(stripped).or_else(|| {
        (!stripped.contains(':'))
            .then(|| lookup(&format!("minecraft:{stripped}")))
            .flatten()
    })
}

#[must_use]
pub fn load_processor_list(name: &str) -> Arc<[StructureProcessor]> {
    static CACHE: LazyLock<dashmap::DashMap<String, Arc<[StructureProcessor]>>> =
        LazyLock::new(dashmap::DashMap::new);

    if let Some(processors) = CACHE.get(name) {
        return Arc::clone(&processors);
    }

    let Some(json) = super::cache::get_processor_list_json(name) else {
        tracing::warn!("Unknown structure processor list: {name}");
        return Arc::from([]);
    };
    let raw: RawProcessorList = match serde_json::from_str(json) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::error!("Failed to parse structure processor list {name}: {error}");
            return Arc::from([]);
        }
    };

    let processors = raw
        .processors
        .into_iter()
        .filter_map(|processor| match processor {
            RawProcessor::BlockRot {
                integrity,
                rottable_blocks,
            } => BlockTag::from_name(&rottable_blocks)
                .map(|blocks| StructureProcessor::BlockRot { integrity, blocks }),
            RawProcessor::ProtectedBlocks { value } => {
                BlockTag::from_name(&value).map(StructureProcessor::ProtectedBlocks)
            }
            RawProcessor::Rule { rules } => Some(StructureProcessor::Rules(
                rules
                    .into_iter()
                    .filter_map(|rule| {
                        Some(ProcessorRule {
                            input_predicate: rule.input_predicate.build()?,
                            location_predicate: rule.location_predicate.build()?,
                            output_state: rule.output_state.get_state(),
                        })
                    })
                    .collect(),
            )),
        })
        .collect::<Arc<[_]>>();
    CACHE.insert(name.to_owned(), Arc::clone(&processors));
    processors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ancient_city_processor_lists() {
        assert_eq!(
            load_processor_list("minecraft:ancient_city_generic_degradation").len(),
            3
        );
        assert_eq!(
            load_processor_list("minecraft:ancient_city_start_degradation").len(),
            2
        );
        assert_eq!(
            load_processor_list("minecraft:ancient_city_walls_degradation").len(),
            3
        );
    }

    /// Regression: `street_plains` uses `block_match` / `always_true` rule tests
    /// and `location_predicate`s, which previously failed to parse ("missing field `probability`")
    #[test]
    fn parses_street_plains_processor_list() {
        let processors = load_processor_list("minecraft:street_plains");
        assert_eq!(processors.len(), 1, "expected one rule processor");
        match &processors[0] {
            StructureProcessor::Rules(rules) => assert_eq!(rules.len(), 4),
            _ => panic!("expected a rule processor"),
        }
    }
}
