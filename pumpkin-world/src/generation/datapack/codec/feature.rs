//! Runtime JSON → runtime-value codec for `worldgen/{configured,placed}_feature`.
//!
//! This is the runtime counterpart of the compile-time codegen in
//! `pumpkin-codegen/src/{configured_feature,placed_feature}.rs`: it parses datapack
//! feature JSON into the same owned runtime types (`ConfiguredFeature`, `PlacedFeature`,
//! `BlockStateProvider`, `BlockPredicate`, `PlacementModifier`, …) that the codegen
//! constructs, so datapack-defined (incl. modded) features can decorate chunks.
//!
//! Design notes:
//! - Every helper is **total** with vanilla-matching defaults (mirroring the codegen),
//!   so a malformed field degrades gracefully rather than dropping the whole feature.
//!   Unknown configured-feature *types* fall back to [`ConfiguredFeature::NoOp`]; unknown
//!   placement-modifier types are skipped.
//! - Nested feature references (named or inline) are resolved through a
//!   [`FeatureRefResolver`], implemented by the datapack feature registry, which resolves
//!   vanilla ids to the compile-time impls and datapack ids to freshly parsed values.
//! - `minecraft:biome` placement modifiers are **dropped** for datapack features (see
//!   [`parse_placement_modifier`]): the decoration loop already runs each biome's own
//!   feature list, so the per-position biome re-check is a fidelity refinement tracked
//!   for a later pass (exact `IndexedFeatures` placement parity).

use serde_json::Value;

use pumpkin_data::{Block, BlockDirection, BlockState, tag};
use pumpkin_util::DoublePerlinNoiseParametersCodec;
use pumpkin_util::HeightMap;
use pumpkin_util::math::int_provider::{
    BiasedToBottomIntProvider, ClampedIntProvider, ClampedNormalIntProvider, ConstantIntProvider,
    IntProvider, NormalIntProvider, TrapezoidIntProvider, UniformIntProvider, WeightedEntry,
    WeightedListIntProvider,
};
use pumpkin_util::math::pool::Weighted;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::y_offset::{AboveBottom, Absolute, BelowTop, YOffset};

use crate::block::BlockStateCodec;
use crate::generation::block_predicate::{
    AllOfBlockPredicate, AnyOfBlockPredicate, BlockPredicate, HasSturdyFacePredicate,
    InsideWorldBoundsBlockPredicate, MatchingBlockTagPredicate, MatchingBlocksBlockPredicate,
    MatchingBlocksWrapper, MatchingFluidsBlockPredicate, NotBlockPredicate,
    OffsetBlocksBlockPredicate, ReplaceableBlockPredicate, SolidBlockPredicate,
    WouldSurviveBlockPredicate,
};
use crate::generation::block_state_provider::{
    BlockStateProvider, BlockStateRule, DualNoiseBlockStateProvider, NoiseBlockStateProvider,
    NoiseBlockStateProviderBase, NoiseThresholdBlockStateProvider, PillarBlockStateProvider,
    RandomizedIntBlockStateProvider, RuleBasedBlockStateProvider, SimpleStateProvider,
    WeightedBlockStateProvider,
};
use crate::generation::feature::configured_features::ConfiguredFeature;
use crate::generation::feature::features::{
    bamboo::BambooFeature,
    block_column::{BlockColumnFeature, Layer},
    disk::DiskFeature,
    fallen_tree::FallenTreeFeature,
    forest_rock::ForestRockFeature,
    iceberg::IcebergFeature,
    lake::LakeFeature,
    nether_forest_vegetation::NetherForestVegetationFeature,
    netherrack_replace_blobs::ReplaceBlobsFeature,
    ore::{OreFeature, OreTarget},
    random_boolean_selector::RandomBooleanFeature,
    random_patch::RandomPatchFeature,
    random_selector::{RandomFeature, RandomFeatureEntry},
    root_system::RootSystemFeature,
    scattered_ore::ScatteredOreFeature,
    sea_pickle::SeaPickleFeature,
    seagrass::SeagrassFeature,
    simple_block::SimpleBlockFeature,
    simple_random_selector::SimpleRandomFeature,
    spring_feature::{BlockWrapper, SpringFeatureFeature},
    twisting_vines::TwistingVinesFeature,
    underwater_magma::UnderwaterMagmaFeature,
    vegetation_patch::VegetationPatchFeature,
    waterlogged_vegetation_patch::WaterloggedVegetationPatchFeature,
};
use crate::generation::feature::features::tree::TreeFeature;
use crate::generation::feature::features::tree::decorator::{
    TreeDecorator, alter_ground::AlterGroundTreeDecorator,
    attached_to_leaves::AttachedToLeavesTreeDecorator, attached_to_logs::AttachedToLogsTreeDecorator,
    beehive::BeehiveTreeDecorator, cocoa::CocoaTreeDecorator, creaking_heart::CreakingHeartTreeDecorator,
    leave_vine::LeavesVineTreeDecorator, pale_moss::PaleMossTreeDecorator,
    place_on_ground::PlaceOnGroundTreeDecorator, trunk_vine::TrunkVineTreeDecorator,
};
use crate::generation::feature::features::tree::foliage::{
    FoliagePlacer, FoliageType, acacia::AcaciaFoliagePlacer, blob::BlobFoliagePlacer,
    bush::BushFoliagePlacer, cherry::CherryFoliagePlacer, dark_oak::DarkOakFoliagePlacer,
    fancy::LargeOakFoliagePlacer, jungle::JungleFoliagePlacer, mega_pine::MegaPineFoliagePlacer,
    pine::PineFoliagePlacer, random_spread::RandomSpreadFoliagePlacer, spruce::SpruceFoliagePlacer,
};
use crate::generation::feature::features::tree::trunk::{
    TrunkPlacer, TrunkType, bending::BendingTrunkPlacer, cherry::CherryTrunkPlacer,
    dark_oak::DarkOakTrunkPlacer, fancy::FancyTrunkPlacer, forking::ForkingTrunkPlacer,
    giant::GiantTrunkPlacer, mega_jungle::MegaJungleTrunkPlacer, straight::StraightTrunkPlacer,
    upwards_branching::UpwardsBranchingTrunkPlacer,
};
use crate::generation::feature::features::tree::root::{
    RootPlacer,
    mangrove::{AboveRootPlacement, MangroveRootPlacement, MangroveRootPlacer},
};
use crate::generation::feature::placed_features::{
    BlockFilterPlacementModifier, CountOnEveryLayerPlacementModifier,
    CountPlacementModifier, EnvironmentScanPlacementModifier, Feature, HeightRangePlacementModifier,
    HeightmapPlacementModifier, NoiseBasedCountPlacementModifier, NoiseThresholdCountPlacementModifier,
    PlacedFeature, PlacedFeatureWrapper, PlacementModifier, RandomOffsetPlacementModifier,
    RarityFilterPlacementModifier, SurfaceThresholdFilterPlacementModifier,
    SurfaceWaterDepthFilterPlacementModifier,
};
use crate::generation::feature::size::{
    FeatureSize, FeatureSizeType, ThreeLayersFeatureSize, TwoLayersFeatureSize,
};
use crate::generation::height_provider::{
    HeightProvider, TrapezoidHeightProvider, UniformHeightProvider, VeryBiasedToBottomHeightProvider,
};
use crate::generation::rule::{
    RuleTest, block_match::BlockMatchRuleTest, block_state_match::BlockStateMatchRuleTest,
    random_block_match::RandomBlockMatchRuleTest,
    random_block_state_match::RandomBlockStateMatchRuleTest, tag_match::TagMatchRuleTest,
};

/// Resolver for nested feature references encountered while parsing.
///
/// Implemented by the datapack feature registry: it resolves a reference id to the
/// datapack's own definition (parsing it) or to a compile-time vanilla feature, and
/// guards against reference cycles.
pub trait FeatureRefResolver {
    /// Resolve a configured-feature reference (a `worldgen/configured_feature` id).
    fn resolve_configured_ref(&self, id: &str) -> Feature;
    /// Resolve a placed-feature reference (a `worldgen/placed_feature` id) as a wrapper
    /// (named vanilla or an inlined datapack definition).
    fn resolve_placed_wrapper(&self, id: &str) -> PlacedFeatureWrapper;
}

// ---------------------------------------------------------------------------
// Leaf helpers
// ---------------------------------------------------------------------------

/// Parse a `{ "Name", "Properties" }` block state, defaulting to the block's default
/// state (or air) on any error. `BlockStateCodec::get_state` **panics** when the JSON
/// supplies properties invalid for the block (`Block::from_properties` → "Invalid
/// props"); datapack states are untrusted, so we catch that and fall back rather than
/// aborting the whole registry build. (This runs at world load only.)
fn parse_block_state(v: &Value) -> &'static BlockState {
    let Ok(codec) = serde_json::from_value::<BlockStateCodec>(v.clone()) else {
        return Block::AIR.default_state;
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec.get_state()))
        .unwrap_or_else(|_| codec.get_block().default_state)
}

/// Parse a block-state codec (keeps `{Name,Properties}`), defaulting to air.
fn parse_block_state_codec(v: &Value) -> BlockStateCodec {
    serde_json::from_value::<BlockStateCodec>(v.clone()).unwrap_or(BlockStateCodec {
        name: &Block::AIR,
        properties: None,
    })
}

/// Resolve a block-tag name (`#minecraft:foo` / `minecraft:foo` / `foo`) to a runtime
/// [`tag::Tag`]. Unknown tags resolve to the empty tag (matches nothing).
pub(super) fn resolve_block_tag(name: &str) -> tag::Tag {
    let stripped = name.strip_prefix('#').unwrap_or(name);
    let lookup = |key: &str| -> Option<tag::Tag> {
        let values = tag::get_tag_values(tag::RegistryKey::Block, key)?;
        let ids = tag::get_tag_ids(tag::RegistryKey::Block, key)?;
        Some((values, ids))
    };
    lookup(stripped)
        .or_else(|| {
            (!stripped.contains(':'))
                .then(|| lookup(&format!("minecraft:{stripped}")))
                .flatten()
        })
        .unwrap_or((&[], &[]))
}

/// Resolve a `BlockDirection` from a lowercase string (default `Down`).
fn parse_block_direction(s: &str) -> BlockDirection {
    match s.to_ascii_lowercase().as_str() {
        "up" => BlockDirection::Up,
        "north" => BlockDirection::North,
        "south" => BlockDirection::South,
        "west" => BlockDirection::West,
        "east" => BlockDirection::East,
        _ => BlockDirection::Down,
    }
}

/// Resolve a `HeightMap` from its `SCREAMING_SNAKE` name (default `MotionBlocking`).
fn parse_height_map(s: &str) -> HeightMap {
    match s {
        "WORLD_SURFACE_WG" => HeightMap::WorldSurfaceWg,
        "WORLD_SURFACE" => HeightMap::WorldSurface,
        "OCEAN_FLOOR_WG" => HeightMap::OceanFloorWg,
        "OCEAN_FLOOR" => HeightMap::OceanFloor,
        "MOTION_BLOCKING_NO_LEAVES" => HeightMap::MotionBlockingNoLeaves,
        _ => HeightMap::MotionBlocking,
    }
}

/// Parse a `DoublePerlinNoiseParametersCodec`, computing the derived `amplitude` exactly
/// as the codegen (`value_to_dpnp`) does.
fn parse_dpnp(v: &Value) -> DoublePerlinNoiseParametersCodec {
    let mut codec: DoublePerlinNoiseParametersCodec =
        serde_json::from_value(v.clone()).unwrap_or(DoublePerlinNoiseParametersCodec {
            first_octave: -7,
            amplitudes: Vec::new(),
            amplitude: 0.0,
        });
    let mut min_octave = i32::MAX;
    let mut max_octave = i32::MIN;
    for (index, amp) in codec.amplitudes.iter().enumerate() {
        if *amp != 0.0 {
            min_octave = min_octave.min(index as i32);
            max_octave = max_octave.max(index as i32);
        }
    }
    codec.amplitude = if max_octave < min_octave {
        0.0
    } else {
        let octaves = max_octave - min_octave;
        let create_amp = 0.1f64 * (1.0f64 + 1.0f64 / f64::from(octaves + 1));
        0.166_666_666_666_666_66f64 / create_amp
    };
    codec
}

/// Read a field as `f32` with a default.
pub(super) fn f32_of(v: &Value, key: &str, default: f32) -> f32 {
    v.get(key).and_then(Value::as_f64).map_or(default, |x| x as f32)
}

/// Read a field as `f64` with a default.
fn f64_of(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(default)
}

/// Read a field as `i32` with a default.
pub(super) fn i32_of(v: &Value, key: &str, default: i32) -> i32 {
    v.get(key).and_then(Value::as_i64).map_or(default, |x| x as i32)
}

/// Read a field as `bool` with a default.
fn bool_of(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}

// ---------------------------------------------------------------------------
// IntProvider / HeightProvider / YOffset (mirrors the codegen manual parsers)
// ---------------------------------------------------------------------------

/// The `type` field of a JSON object as a namespaced resource location. Datapacks may
/// write type ids bare (`"count"`, `"environment_scan"`) or namespaced
/// (`"minecraft:count"`) — Terralith mixes both, even within one placed feature — so
/// normalize a bare id into the `minecraft:` namespace. Matches how the density-function
/// codec already handles bare ids; without it a bare-id modifier/provider/predicate is
/// silently dropped (e.g. `lakes`' `environment_scan`, causing water placed in mid-air).
pub(super) fn norm_id(t: &str) -> std::borrow::Cow<'_, str> {
    if t.contains(':') {
        std::borrow::Cow::Borrowed(t)
    } else {
        std::borrow::Cow::Owned(format!("minecraft:{t}"))
    }
}

fn type_id(v: &Value) -> std::borrow::Cow<'_, str> {
    norm_id(v["type"].as_str().unwrap_or(""))
}

/// Parse an `IntProvider` (bare number = constant, or a typed object), matching the
/// codegen's `value_to_int_provider` exactly (notably `trapezoid` uses `min`/`max`).
fn parse_int_provider(v: &Value) -> IntProvider {
    match v {
        Value::Number(n) => IntProvider::Constant(n.as_i64().unwrap_or(0) as i32),
        Value::Object(_) => match type_id(v).as_ref() {
            "minecraft:constant" => IntProvider::Object(NormalIntProvider::Constant(
                ConstantIntProvider {
                    value: i32_of(v, "value", 0),
                },
            )),
            "minecraft:uniform" => IntProvider::Object(NormalIntProvider::Uniform(
                UniformIntProvider {
                    min_inclusive: i32_of(v, "min_inclusive", 0),
                    max_inclusive: i32_of(v, "max_inclusive", 0),
                },
            )),
            "minecraft:biased_to_bottom" => IntProvider::Object(NormalIntProvider::BiasedToBottom(
                BiasedToBottomIntProvider {
                    min_inclusive: i32_of(v, "min_inclusive", 0),
                    max_inclusive: i32_of(v, "max_inclusive", 0),
                },
            )),
            "minecraft:clamped" => IntProvider::Object(NormalIntProvider::Clamped(
                ClampedIntProvider {
                    source: Box::new(parse_int_provider(&v["source"])),
                    min_inclusive: i32_of(v, "min_inclusive", 0),
                    max_inclusive: i32_of(v, "max_inclusive", 0),
                },
            )),
            "minecraft:trapezoid" => IntProvider::Object(NormalIntProvider::Trapezoid(
                TrapezoidIntProvider {
                    min_inclusive: i32_of(v, "min", 0),
                    max_inclusive: i32_of(v, "max", 0),
                    plateau: i32_of(v, "plateau", 0),
                },
            )),
            "minecraft:clamped_normal" => IntProvider::Object(NormalIntProvider::ClampedNormal(
                ClampedNormalIntProvider {
                    mean: f32_of(v, "mean", 0.0),
                    deviation: f32_of(v, "deviation", 1.0),
                    min_inclusive: i32_of(v, "min_inclusive", 0),
                    max_inclusive: i32_of(v, "max_inclusive", 0),
                },
            )),
            "minecraft:weighted_list" => {
                let distribution = v["distribution"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|e| WeightedEntry {
                                data: parse_int_provider(&e["data"]),
                                weight: i32_of(e, "weight", 1),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                IntProvider::Object(NormalIntProvider::WeightedList(WeightedListIntProvider {
                    distribution,
                }))
            }
            _ => IntProvider::Constant(0),
        },
        _ => IntProvider::Constant(0),
    }
}

/// Parse a `YOffset` (`absolute` / `above_bottom` / `below_top`).
#[allow(clippy::option_if_let_else)]
pub(super) fn parse_y_offset(v: &Value) -> YOffset {
    if let Some(abs) = v.get("absolute").and_then(Value::as_i64) {
        YOffset::Absolute(Absolute {
            absolute: abs as i16,
        })
    } else if let Some(ab) = v.get("above_bottom").and_then(Value::as_i64) {
        YOffset::AboveBottom(AboveBottom {
            above_bottom: ab as i8,
        })
    } else if let Some(bt) = v.get("below_top").and_then(Value::as_i64) {
        YOffset::BelowTop(BelowTop {
            below_top: bt as i8,
        })
    } else {
        YOffset::Absolute(Absolute { absolute: 0 })
    }
}

/// Parse a `HeightProvider` (uniform / trapezoid / `very_biased_to_bottom` / constant).
fn parse_height_provider(v: &Value) -> HeightProvider {
    match type_id(v).as_ref() {
        "minecraft:trapezoid" => HeightProvider::Trapezoid(TrapezoidHeightProvider {
            min_inclusive: parse_y_offset(&v["min_inclusive"]),
            max_inclusive: parse_y_offset(&v["max_inclusive"]),
            plateau: v["plateau"].as_i64().map(|x| x as i32),
        }),
        "minecraft:very_biased_to_bottom" => {
            HeightProvider::VeryBiasedToBottom(VeryBiasedToBottomHeightProvider {
                min_inclusive: parse_y_offset(&v["min_inclusive"]),
                max_inclusive: parse_y_offset(&v["max_inclusive"]),
                inner: v["inner"]
                    .as_u64()
                    .and_then(|n| std::num::NonZeroU32::new(n as u32)),
            })
        }
        "minecraft:constant" => HeightProvider::Uniform(UniformHeightProvider {
            min_inclusive: parse_y_offset(&v["value"]),
            max_inclusive: parse_y_offset(&v["value"]),
        }),
        // uniform + fallback
        _ => HeightProvider::Uniform(UniformHeightProvider {
            min_inclusive: parse_y_offset(&v["min_inclusive"]),
            max_inclusive: parse_y_offset(&v["max_inclusive"]),
        }),
    }
}

// ---------------------------------------------------------------------------
// RuleTest
// ---------------------------------------------------------------------------

fn parse_rule_test(v: &Value) -> RuleTest {
    match norm_id(v["predicate_type"].as_str().unwrap_or("")).as_ref() {
        "minecraft:block_match" => {
            let block = v["block"].as_str().unwrap_or("minecraft:stone");
            RuleTest::BlockMatch(BlockMatchRuleTest {
                block: Block::from_name(block).unwrap_or(&Block::STONE).id,
            })
        }
        "minecraft:blockstate_match" => RuleTest::BlockStateMatch(BlockStateMatchRuleTest {
            block_state: parse_block_state(&v["block_state"]).id,
        }),
        "minecraft:tag_match" => RuleTest::TagMatch(TagMatchRuleTest {
            tag: resolve_block_tag(v["tag"].as_str().unwrap_or("")),
        }),
        "minecraft:random_block_match" => {
            let block = v["block"].as_str().unwrap_or("minecraft:stone");
            RuleTest::RandomBlockMatch(RandomBlockMatchRuleTest {
                block: Block::from_name(block).unwrap_or(&Block::STONE).id,
                probability: f32_of(v, "probability", 0.5),
            })
        }
        "minecraft:random_blockstate_match" => {
            RuleTest::RandomBlockStateMatch(RandomBlockStateMatchRuleTest {
                block_state: parse_block_state(&v["block_state"]).id,
                probability: f32_of(v, "probability", 0.5),
            })
        }
        // "minecraft:always_true" | "" | unknown
        _ => RuleTest::AlwaysTrue,
    }
}

// ---------------------------------------------------------------------------
// BlockPredicate
// ---------------------------------------------------------------------------

fn parse_offset_predicate(v: &Value) -> OffsetBlocksBlockPredicate {
    if let Some(arr) = v.as_array()
        && arr.len() == 3
    {
        return OffsetBlocksBlockPredicate {
            offset: Some(Vector3::new(
                arr[0].as_i64().unwrap_or(0) as i32,
                arr[1].as_i64().unwrap_or(0) as i32,
                arr[2].as_i64().unwrap_or(0) as i32,
            )),
        };
    }
    OffsetBlocksBlockPredicate { offset: None }
}

fn parse_matching_blocks_wrapper(v: &Value) -> MatchingBlocksWrapper {
    match v {
        Value::String(s) => MatchingBlocksWrapper::Single(s.clone()),
        Value::Array(arr) => MatchingBlocksWrapper::Multiple(
            arr.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
        ),
        _ => MatchingBlocksWrapper::Single(String::new()),
    }
}

fn parse_block_predicate(v: &Value) -> BlockPredicate {
    // Bare strings: "#tag" is a tag predicate, anything else is AlwaysTrue.
    if let Some(s) = v.as_str() {
        return if s.starts_with('#') {
            BlockPredicate::MatchingBlockTag(MatchingBlockTagPredicate {
                offset: OffsetBlocksBlockPredicate { offset: None },
                tag: resolve_block_tag(s),
            })
        } else {
            BlockPredicate::AlwaysTrue
        };
    }

    match type_id(v).as_ref() {
        "minecraft:matching_blocks" => BlockPredicate::MatchingBlocks(MatchingBlocksBlockPredicate {
            offset: parse_offset_predicate(&v["offset"]),
            blocks: parse_matching_blocks_wrapper(&v["blocks"]),
        }),
        "minecraft:matching_block_tag" => BlockPredicate::MatchingBlockTag(MatchingBlockTagPredicate {
            offset: parse_offset_predicate(&v["offset"]),
            tag: resolve_block_tag(v["tag"].as_str().unwrap_or("")),
        }),
        "minecraft:matching_fluids" => BlockPredicate::MatchingFluids(MatchingFluidsBlockPredicate {
            offset: parse_offset_predicate(&v["offset"]),
            fluids: parse_matching_blocks_wrapper(&v["fluids"]),
        }),
        "minecraft:has_sturdy_face" => BlockPredicate::HasSturdyFace(HasSturdyFacePredicate {
            offset: parse_offset_predicate(&v["offset"]),
            direction: parse_block_direction(v["direction"].as_str().unwrap_or("down")),
        }),
        "minecraft:solid" => BlockPredicate::Solid(SolidBlockPredicate {
            offset: parse_offset_predicate(&v["offset"]),
        }),
        "minecraft:replaceable" => BlockPredicate::Replaceable(ReplaceableBlockPredicate {
            offset: parse_offset_predicate(&v["offset"]),
        }),
        "minecraft:would_survive" => BlockPredicate::WouldSurvive(WouldSurviveBlockPredicate {
            offset: parse_offset_predicate(&v["offset"]),
            state: parse_block_state_codec(&v["state"]),
        }),
        "minecraft:inside_world_bounds" => {
            let offset = v["offset"].as_array().map_or_else(
                || Vector3::new(0, 0, 0),
                |arr| {
                    Vector3::new(
                        arr.first().and_then(Value::as_i64).unwrap_or(0) as i32,
                        arr.get(1).and_then(Value::as_i64).unwrap_or(0) as i32,
                        arr.get(2).and_then(Value::as_i64).unwrap_or(0) as i32,
                    )
                },
            );
            BlockPredicate::InsideWorldBounds(InsideWorldBoundsBlockPredicate { offset })
        }
        "minecraft:any_of" => BlockPredicate::AnyOf(AnyOfBlockPredicate {
            predicates: parse_predicate_list(&v["predicates"]),
        }),
        "minecraft:all_of" => BlockPredicate::AllOf(AllOfBlockPredicate {
            predicates: parse_predicate_list(&v["predicates"]),
        }),
        "minecraft:not" => BlockPredicate::Not(NotBlockPredicate {
            predicate: Box::new(parse_block_predicate(&v["predicate"])),
        }),
        // "minecraft:true" | "" | unknown
        _ => BlockPredicate::AlwaysTrue,
    }
}

fn parse_predicate_list(v: &Value) -> Vec<BlockPredicate> {
    v.as_array()
        .map(|a| a.iter().map(parse_block_predicate).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// BlockStateProvider
// ---------------------------------------------------------------------------

fn parse_noise_base(v: &Value) -> NoiseBlockStateProviderBase {
    NoiseBlockStateProviderBase {
        seed: v["seed"].as_i64().unwrap_or(0),
        noise: parse_dpnp(&v["noise"]),
        scale: f32_of(v, "scale", 1.0),
    }
}

fn parse_block_state_list(v: &Value) -> Vec<&'static BlockState> {
    v.as_array()
        .map(|a| a.iter().map(parse_block_state).collect())
        .unwrap_or_default()
}

fn parse_block_state_provider(v: &Value) -> BlockStateProvider {
    match type_id(v).as_ref() {
        "minecraft:simple_state_provider" => BlockStateProvider::Simple(SimpleStateProvider {
            state: parse_block_state(&v["state"]),
        }),
        "minecraft:weighted_state_provider" => {
            let entries = v["entries"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| Weighted {
                            data: parse_block_state(&e["data"]),
                            weight: i32_of(e, "weight", 1),
                        })
                        .collect()
                })
                .unwrap_or_default();
            BlockStateProvider::Weighted(WeightedBlockStateProvider { entries })
        }
        "minecraft:rotated_block_provider" => BlockStateProvider::Pillar(PillarBlockStateProvider {
            state: parse_block_state(&v["state"]),
        }),
        "minecraft:noise_provider" => BlockStateProvider::NoiseProvider(NoiseBlockStateProvider {
            base: parse_noise_base(v),
            states: parse_block_state_list(&v["states"]),
        }),
        "minecraft:dual_noise_provider" => {
            let base = NoiseBlockStateProvider {
                base: parse_noise_base(v),
                states: parse_block_state_list(&v["states"]),
            };
            BlockStateProvider::DualNoise(DualNoiseBlockStateProvider {
                base,
                variety: [
                    v["variety"][0].as_u64().unwrap_or(2) as u32,
                    v["variety"][1].as_u64().unwrap_or(4) as u32,
                ],
                slow_noise: parse_dpnp(&v["slow_noise"]),
                slow_scale: f64_of(v, "slow_scale", 1.0),
            })
        }
        "minecraft:noise_threshold_provider" => {
            BlockStateProvider::NoiseThreshold(NoiseThresholdBlockStateProvider {
                base: parse_noise_base(v),
                threshold: f32_of(v, "threshold", 0.0),
                high_chance: f32_of(v, "high_chance", 0.0),
                default_state: parse_block_state(&v["default_state"]),
                low_states: parse_block_state_list(&v["low_states"]),
                high_states: parse_block_state_list(&v["high_states"]),
            })
        }
        "minecraft:randomized_int_state_provider" => {
            BlockStateProvider::RandomizedInt(RandomizedIntBlockStateProvider {
                source: Box::new(parse_block_state_provider(&v["source"])),
                property: v["property"].as_str().unwrap_or("").to_string(),
                values: parse_int_provider(&v["values"]),
            })
        }
        "minecraft:rule_based_state_provider" => {
            let fallback = (!v["fallback"].is_null())
                .then(|| Box::new(parse_block_state_provider(&v["fallback"])));
            let rules = v["rules"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|rule| BlockStateRule {
                            if_true: parse_block_predicate(&rule["if_true"]),
                            then: parse_block_state_provider(&rule["then"]),
                        })
                        .collect()
                })
                .unwrap_or_default();
            BlockStateProvider::Rule(RuleBasedBlockStateProvider { fallback, rules })
        }
        _ => BlockStateProvider::Simple(SimpleStateProvider {
            state: Block::AIR.default_state,
        }),
    }
}

// ---------------------------------------------------------------------------
// PlacementModifier
// ---------------------------------------------------------------------------

/// Parse a single placement modifier. Returns `None` for unknown/unsupported types
/// (skipped).
fn parse_placement_modifier(v: &Value) -> Option<PlacementModifier> {
    let type_binding = type_id(v);
    Some(match type_binding.as_ref() {
        // The biome filter is KEPT (unlike earlier Phase-4 builds that dropped it): the
        // decoration loop scopes features per biome, but a feature's positions can still
        // span the whole chunk column (e.g. deep-dark sculk over a 0..256 height range).
        // `BiomePlacementModifier` now does a datapack-aware per-position check by the
        // feature's `&'static` identity, so keeping it confines features to their biome.
        "minecraft:biome" => PlacementModifier::Biome(
            crate::generation::feature::placed_features::BiomePlacementModifier,
        ),
        "minecraft:in_square" => PlacementModifier::InSquare(
            crate::generation::feature::placed_features::SquarePlacementModifier,
        ),
        "minecraft:fixed_placement" => {
            let positions = v["positions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            let c = p.as_array()?;
                            Some(pumpkin_util::math::position::BlockPos::new(
                                c.first()?.as_i64()? as i32,
                                c.get(1)?.as_i64()? as i32,
                                c.get(2)?.as_i64()? as i32,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            PlacementModifier::FixedPlacement(positions)
        }
        "minecraft:heightmap" => PlacementModifier::Heightmap(HeightmapPlacementModifier {
            heightmap: parse_height_map(v["heightmap"].as_str().unwrap_or("MOTION_BLOCKING")),
        }),
        "minecraft:height_range" => PlacementModifier::HeightRange(HeightRangePlacementModifier {
            height: parse_height_provider(&v["height"]),
        }),
        "minecraft:count" => PlacementModifier::Count(CountPlacementModifier {
            count: parse_int_provider(&v["count"]),
        }),
        "minecraft:count_on_every_layer" => {
            PlacementModifier::CountOnEveryLayer(CountOnEveryLayerPlacementModifier {
                count: parse_int_provider(&v["count"]),
            })
        }
        "minecraft:rarity_filter" => PlacementModifier::RarityFilter(RarityFilterPlacementModifier {
            chance: v["chance"].as_u64().unwrap_or(1) as u32,
        }),
        "minecraft:block_predicate_filter" => {
            PlacementModifier::BlockPredicateFilter(BlockFilterPlacementModifier {
                predicate: parse_block_predicate(&v["predicate"]),
            })
        }
        "minecraft:surface_relative_threshold_filter" => {
            PlacementModifier::SurfaceRelativeThresholdFilter(SurfaceThresholdFilterPlacementModifier {
                heightmap: parse_height_map(v["heightmap"].as_str().unwrap_or("MOTION_BLOCKING")),
                min_inclusive: v["min_inclusive"].as_i64().map(|x| x as i32),
                max_inclusive: v["max_inclusive"].as_i64().map(|x| x as i32),
            })
        }
        "minecraft:surface_water_depth_filter" => {
            PlacementModifier::SurfaceWaterDepthFilter(SurfaceWaterDepthFilterPlacementModifier {
                max_water_depth: i32_of(v, "max_water_depth", 0),
            })
        }
        "minecraft:noise_based_count" => {
            PlacementModifier::NoiseBasedCount(NoiseBasedCountPlacementModifier {
                to_count_ratio: i32_of(v, "noise_to_count_ratio", 0),
                factor: f64_of(v, "noise_factor", 1.0),
                offset: f64_of(v, "noise_offset", 0.0),
            })
        }
        "minecraft:noise_threshold_count" => {
            PlacementModifier::NoiseThresholdCount(NoiseThresholdCountPlacementModifier {
                noise_level: f64_of(v, "noise_level", 0.0),
                below_noise: i32_of(v, "below_noise", 0),
                above_noise: i32_of(v, "above_noise", 0),
            })
        }
        "minecraft:environment_scan" => {
            PlacementModifier::EnvironmentScan(EnvironmentScanPlacementModifier {
                direction_of_search: parse_block_direction(
                    v["direction_of_search"].as_str().unwrap_or("down"),
                ),
                target_condition: parse_block_predicate(&v["target_condition"]),
                allowed_search_condition: v["allowed_search_condition"]
                    .is_object()
                    .then(|| parse_block_predicate(&v["allowed_search_condition"])),
                max_steps: i32_of(v, "max_steps", 1),
            })
        }
        "minecraft:random_offset" => PlacementModifier::RandomOffset(RandomOffsetPlacementModifier {
            xz_spread: parse_int_provider(&v["xz_spread"]),
            y_spread: parse_int_provider(&v["y_spread"]),
        }),
        _ => return None,
    })
}

fn parse_placement(v: &Value) -> Vec<PlacementModifier> {
    v.as_array()
        .map(|a| a.iter().filter_map(parse_placement_modifier).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Feature / PlacedFeature entry points
// ---------------------------------------------------------------------------

/// Parse the `"feature"` field of a placed feature (named ref or inline configured).
fn parse_feature(v: &Value, r: &dyn FeatureRefResolver) -> Feature {
    match v {
        Value::String(s) => r.resolve_configured_ref(s),
        Value::Object(_) => Feature::Inlined(Box::new(parse_configured_feature(v, r))),
        _ => Feature::Inlined(Box::new(ConfiguredFeature::NoOp)),
    }
}

/// Parse an inline placed feature (`{ "feature", "placement" }`).
pub fn parse_placed_feature(v: &Value, r: &dyn FeatureRefResolver) -> PlacedFeature {
    PlacedFeature {
        feature: parse_feature(&v["feature"], r),
        placement: parse_placement(&v["placement"]),
    }
}

/// Parse a placed-feature *wrapper* reference (named vanilla or inline).
fn parse_placed_feature_wrapper(v: &Value, r: &dyn FeatureRefResolver) -> PlacedFeatureWrapper {
    match v {
        Value::String(s) => r.resolve_placed_wrapper(s),
        Value::Object(_) => PlacedFeatureWrapper::Direct(parse_placed_feature(v, r)),
        _ => PlacedFeatureWrapper::Direct(PlacedFeature {
            feature: Feature::Inlined(Box::new(ConfiguredFeature::NoOp)),
            placement: Vec::new(),
        }),
    }
}

// ---------------------------------------------------------------------------
// ConfiguredFeature
// ---------------------------------------------------------------------------

/// Parse a configured feature (`{ "type", "config" }`). Unknown/unsupported types
/// degrade to [`ConfiguredFeature::NoOp`].
#[allow(clippy::too_many_lines)]
pub fn parse_configured_feature(v: &Value, r: &dyn FeatureRefResolver) -> ConfiguredFeature {
    let type_binding = type_id(v);
    let type_str = type_binding.as_ref();
    let config = &v["config"];
    match type_str {
        "minecraft:no_op" | "minecraft:sequence" | "minecraft:weighted_random_selector" => {
            ConfiguredFeature::NoOp
        }
        "minecraft:bamboo" => ConfiguredFeature::Bamboo(BambooFeature {
            probability: f32_of(config, "probability", 0.0),
        }),
        "minecraft:seagrass" => ConfiguredFeature::Seagrass(SeagrassFeature {
            probability: f32_of(config, "probability", 0.0),
        }),
        "minecraft:sea_pickle" => ConfiguredFeature::SeaPickle(SeaPickleFeature {
            count: parse_int_provider(&config["count"]),
        }),
        "minecraft:nether_forest_vegetation" => {
            ConfiguredFeature::NetherForestVegetation(NetherForestVegetationFeature {
                state_provider: parse_block_state_provider(&config["state_provider"]),
                spread_width: i32_of(config, "spread_width", 8),
                spread_height: i32_of(config, "spread_height", 4),
            })
        }
        "minecraft:netherrack_replace_blobs" => {
            ConfiguredFeature::NetherrackReplaceBlobs(ReplaceBlobsFeature {
                target: parse_block_state(&config["target"]),
                state: parse_block_state(&config["state"]),
                radius: parse_int_provider(&config["radius"]),
            })
        }
        "minecraft:simple_block" => ConfiguredFeature::SimpleBlock(SimpleBlockFeature {
            to_place: parse_block_state_provider(&config["to_place"]),
            schedule_tick: config["schedule_tick"].as_bool(),
        }),
        "minecraft:ore" | "minecraft:scattered_ore" => {
            let size = i32_of(config, "size", 0);
            let discard = f32_of(config, "discard_chance_on_air_exposure", 0.0);
            let targets = config["targets"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|t| OreTarget {
                            target: parse_rule_test(&t["target"]),
                            state: parse_block_state(&t["state"]),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if type_str == "minecraft:scattered_ore" {
                ConfiguredFeature::ScatteredOre(ScatteredOreFeature {
                    size,
                    discard_chance_on_air_exposure: discard,
                    targets,
                })
            } else {
                ConfiguredFeature::Ore(OreFeature {
                    size,
                    discard_chance_on_air_exposure: discard,
                    targets,
                })
            }
        }
        "minecraft:spring_feature" => ConfiguredFeature::SpringFeature(SpringFeatureFeature {
            state: parse_block_state(&config["state"]),
            requires_block_below: bool_of(config, "requires_block_below", true),
            rock_count: i32_of(config, "rock_count", 4),
            hole_count: i32_of(config, "hole_count", 1),
            valid_blocks: parse_block_wrapper(&config["valid_blocks"]),
        }),
        "minecraft:block_column" => {
            let layers = config["layers"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|l| Layer {
                            height: parse_int_provider(&l["height"]),
                            provider: parse_block_state_provider(&l["provider"]),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::BlockColumn(BlockColumnFeature {
                layers,
                direction: parse_block_direction(config["direction"].as_str().unwrap_or("up")),
                allowed_placement: parse_block_predicate(&config["allowed_placement"]),
                prioritize_tip: bool_of(config, "prioritize_tip", false),
            })
        }
        "minecraft:fallen_tree" => ConfiguredFeature::FallenTree(FallenTreeFeature {
            trunk_provider: parse_block_state_provider(&config["trunk_provider"]),
        }),
        "minecraft:random_patch" | "minecraft:flower" | "minecraft:no_bonemeal_flower" => {
            let patch = RandomPatchFeature {
                tries: config["tries"].as_u64().unwrap_or(128) as u8,
                xz_spread: config["xz_spread"].as_u64().unwrap_or(7) as u8,
                y_spread: config["y_spread"].as_u64().unwrap_or(3) as u8,
                feature: Box::new(parse_placed_feature(&config["feature"], r)),
            };
            match type_str {
                "minecraft:flower" => ConfiguredFeature::Flower(patch),
                "minecraft:no_bonemeal_flower" => ConfiguredFeature::NoBonemealFlower(patch),
                _ => ConfiguredFeature::RandomPatch(patch),
            }
        }
        "minecraft:random_selector" => {
            let features = config["features"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| RandomFeatureEntry {
                            feature: parse_placed_feature_wrapper(&e["feature"], r),
                            chance: f32_of(e, "chance", 0.1),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ConfiguredFeature::RandomSelector(RandomFeature {
                features,
                default: Box::new(parse_placed_feature_wrapper(&config["default"], r)),
            })
        }
        "minecraft:simple_random_selector" => {
            let features = config["features"]
                .as_array()
                .map(|arr| arr.iter().map(|e| parse_placed_feature(e, r)).collect())
                .unwrap_or_default();
            ConfiguredFeature::SimpleRandomSelector(SimpleRandomFeature { features })
        }
        "minecraft:random_boolean_selector" => {
            ConfiguredFeature::RandomBooleanSelector(RandomBooleanFeature {
                feature_true: Box::new(parse_placed_feature_wrapper(&config["feature_true"], r)),
                feature_false: Box::new(parse_placed_feature_wrapper(&config["feature_false"], r)),
            })
        }
        "minecraft:tree" => ConfiguredFeature::Tree(Box::new(parse_tree_feature(config, r))),
        "minecraft:vegetation_patch" => {
            ConfiguredFeature::VegetationPatch(parse_vegetation_patch(config, r))
        }
        "minecraft:waterlogged_vegetation_patch" => {
            ConfiguredFeature::WaterloggedVegetationPatch(WaterloggedVegetationPatchFeature {
                base: parse_vegetation_patch(config, r),
            })
        }
        "minecraft:root_system" => ConfiguredFeature::RootSystem(RootSystemFeature {
            feature: Box::new(parse_placed_feature(&config["feature"], r)),
            required_vertical_space_for_tree: i32_of(config, "required_vertical_space_for_tree", 0),
            root_radius: i32_of(config, "root_radius", 0),
            root_replaceable: parse_block_predicate(&config["root_replaceable"]),
            root_state_provider: parse_block_state_provider(&config["root_state_provider"]),
            root_placement_attempts: i32_of(config, "root_placement_attempts", 0),
            root_column_max_height: i32_of(config, "root_column_max_height", 0),
            hanging_root_radius: i32_of(config, "hanging_root_radius", 0),
            hanging_roots_vertical_span: config["hanging_roots_vertical_span"]
                .as_i64()
                .or_else(|| config["hanging_root_vertical_span"].as_i64())
                .unwrap_or(0) as i32,
            hanging_root_state_provider: parse_block_state_provider(
                &config["hanging_root_state_provider"],
            ),
            hanging_root_placement_attempts: i32_of(config, "hanging_root_placement_attempts", 0),
            allowed_vertical_water_for_tree: i32_of(config, "allowed_vertical_water_for_tree", 0),
            allowed_tree_position: parse_block_predicate(&config["allowed_tree_position"]),
        }),
        "minecraft:twisting_vines" => ConfiguredFeature::TwistingVines(TwistingVinesFeature {
            spread_width: i32_of(config, "spread_width", 0),
            spread_height: i32_of(config, "spread_height", 0),
            max_height: i32_of(config, "max_height", 0),
        }),
        "minecraft:underwater_magma" => ConfiguredFeature::UnderwaterMagma(UnderwaterMagmaFeature {
            floor_search_range: i32_of(config, "floor_search_range", 0),
            placement_radius: i32_of(config, "placement_radius_around_floor", 0),
            placement_probability: f32_of(config, "placement_probability_per_valid_position", 0.0),
        }),
        "minecraft:disk" => ConfiguredFeature::Disk(DiskFeature {
            state_provider: parse_block_state_provider(&config["state_provider"]),
            target: parse_block_predicate(&config["target"]),
            radius: parse_int_provider(&config["radius"]),
            half_height: i32_of(config, "half_height", 1),
        }),
        "minecraft:block_blob" => ConfiguredFeature::ForestRock(ForestRockFeature {
            state: parse_block_state(&config["state"]),
        }),
        "minecraft:iceberg" => ConfiguredFeature::Iceberg(IcebergFeature {
            main_block: parse_block_state_codec(&config["state"]),
        }),
        "minecraft:lake" => ConfiguredFeature::Lake(LakeFeature {
            fluid: parse_block_state_provider(&config["fluid"]),
            barrier: parse_block_state_provider(&config["barrier"]),
        }),
        // --- Unit-config feature types (config carries nothing we model) ---
        "minecraft:glowstone_blob" => ConfiguredFeature::GlowstoneBlob(
            crate::generation::feature::features::glowstone_blob::GlowstoneBlobFeature {},
        ),
        "minecraft:basalt_pillar" => ConfiguredFeature::BasaltPillar(
            crate::generation::feature::features::basalt_pillar::BasaltPillarFeature {},
        ),
        "minecraft:freeze_top_layer" => ConfiguredFeature::FreezeTopLayer(
            crate::generation::feature::features::freeze_top_layer::FreezeTopLayerFeature {},
        ),
        "minecraft:ice_spike" | "minecraft:spike" => ConfiguredFeature::IceSpike(
            crate::generation::feature::features::ice_spike::IceSpikeFeature {},
        ),
        "minecraft:chorus_plant" => ConfiguredFeature::ChorusPlant(
            crate::generation::feature::features::chorus_plant::ChorusPlantFeature {},
        ),
        "minecraft:end_island" => ConfiguredFeature::EndIsland(
            crate::generation::feature::features::end_island::EndIslandFeature {},
        ),
        "minecraft:kelp" => ConfiguredFeature::Kelp(
            crate::generation::feature::features::kelp::KelpFeature {},
        ),
        "minecraft:huge_brown_mushroom" => ConfiguredFeature::HugeBrownMushroom(
            crate::generation::feature::features::huge_brown_mushroom::HugeBrownMushroomFeature {},
        ),
        "minecraft:huge_red_mushroom" => ConfiguredFeature::HugeRedMushroom(
            crate::generation::feature::features::huge_red_mushroom::HugeRedMushroomFeature {},
        ),
        "minecraft:vines" => ConfiguredFeature::Vines(
            crate::generation::feature::features::vines::VinesFeature,
        ),
        "minecraft:multiface_growth" => ConfiguredFeature::MultifaceGrowth(
            crate::generation::feature::features::multiface_growth::MultifaceGrowthFeature {},
        ),
        "minecraft:blue_ice" => ConfiguredFeature::BlueIce(
            crate::generation::feature::features::blue_ice::BlueIceFeature {},
        ),
        "minecraft:end_gateway" => ConfiguredFeature::EndGateway(
            crate::generation::feature::features::end_gateway::EndGatewayFeature {},
        ),
        "minecraft:monster_room" => ConfiguredFeature::MonsterRoom(
            crate::generation::feature::features::monster_room::DungeonFeature {},
        ),
        "minecraft:desert_well" => ConfiguredFeature::DesertWell(
            crate::generation::feature::features::desert_well::DesertWellFeature,
        ),
        "minecraft:block_pile" => ConfiguredFeature::BlockPile(
            crate::generation::feature::features::block_pile::BlockPileFeature {},
        ),
        "minecraft:replace_single_block" => ConfiguredFeature::ReplaceSingleBlock(
            crate::generation::feature::features::replace_single_block::ReplaceSingleBlockFeature {},
        ),
        "minecraft:void_start_platform" => ConfiguredFeature::VoidStartPlatform(
            crate::generation::feature::features::void_start_platform::VoidStartPlatformFeature {},
        ),
        "minecraft:delta_feature" => ConfiguredFeature::DeltaFeature(
            crate::generation::feature::features::delta_feature::DeltaFeatureFeature {},
        ),
        "minecraft:fill_layer" => ConfiguredFeature::FillLayer(
            crate::generation::feature::features::fill_layer::FillLayerFeature {},
        ),
        "minecraft:bonus_chest" => ConfiguredFeature::BonusChest(
            crate::generation::feature::features::bonus_chest::BonusChestFeature {},
        ),
        "minecraft:end_platform" => ConfiguredFeature::EndPlatform(
            crate::generation::feature::features::end_platform::EndPlatformFeature,
        ),
        "minecraft:coral_tree" => ConfiguredFeature::CoralTree(
            crate::generation::feature::features::coral::coral_tree::CoralTreeFeature,
        ),
        "minecraft:coral_mushroom" => ConfiguredFeature::CoralMushroom(
            crate::generation::feature::features::coral::coral_mushroom::CoralMushroomFeature,
        ),
        "minecraft:coral_claw" => ConfiguredFeature::CoralClaw(
            crate::generation::feature::features::coral::coral_claw::CoralClawFeature,
        ),
        // Everything else (geode, sculk_patch, fossil, end_spike, huge_fungus,
        // dripstone*, basalt_columns, weeping_vines, …) is not yet modelled by the
        // runtime codec; degrade to a no-op rather than dropping the chunk.
        _ => ConfiguredFeature::NoOp,
    }
}

fn parse_vegetation_patch(config: &Value, r: &dyn FeatureRefResolver) -> VegetationPatchFeature {
    use pumpkin_util::math::vertical_surface_type::VerticalSurfaceType;
    VegetationPatchFeature {
        replaceable: parse_block_predicate(&config["replaceable"]),
        ground_state: parse_block_state_provider(&config["ground_state"]),
        vegetation_feature: Box::new(parse_placed_feature(&config["vegetation_feature"], r)),
        surface: match config["surface"].as_str().unwrap_or("floor") {
            "ceiling" => VerticalSurfaceType::Ceiling,
            _ => VerticalSurfaceType::Floor,
        },
        depth: parse_int_provider(&config["depth"]),
        extra_bottom_block_chance: f32_of(config, "extra_bottom_block_chance", 0.0),
        vertical_range: i32_of(config, "vertical_range", 0),
        vegetation_chance: f32_of(config, "vegetation_chance", 0.0),
        xz_radius: parse_int_provider(&config["xz_radius"]),
        extra_edge_column_chance: f32_of(config, "extra_edge_column_chance", 0.0),
    }
}

/// Parse a block list/tag into a `BlockWrapper` (single name or multi names).
fn parse_block_wrapper(v: &Value) -> BlockWrapper {
    match v {
        Value::String(s) => BlockWrapper::Single(s.clone()),
        Value::Array(arr) => BlockWrapper::Multi(
            arr.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect(),
        ),
        _ => BlockWrapper::Single(String::new()),
    }
}

// ---------------------------------------------------------------------------
// Tree feature
// ---------------------------------------------------------------------------

fn parse_tree_feature(config: &Value, r: &dyn FeatureRefResolver) -> TreeFeature {
    TreeFeature {
        trunk_provider: parse_block_state_provider(&config["trunk_provider"]),
        trunk_placer: parse_trunk_placer(&config["trunk_placer"]),
        foliage_provider: parse_block_state_provider(&config["foliage_provider"]),
        foliage_placer: parse_foliage_placer(&config["foliage_placer"]),
        minimum_size: parse_feature_size(&config["minimum_size"]),
        ignore_vines: bool_of(config, "ignore_vines", true),
        below_trunk_provider: parse_block_state_provider(&config["below_trunk_provider"]),
        decorators: config["decorators"]
            .as_array()
            .map(|arr| arr.iter().filter_map(parse_tree_decorator).collect())
            .unwrap_or_default(),
        root_placer: config
            .get("root_placer")
            .filter(|rp| !rp.is_null())
            .and_then(|rp| parse_root_placer(rp, r)),
    }
}

fn parse_trunk_placer(v: &Value) -> TrunkPlacer {
    let base_height = v["base_height"].as_u64().unwrap_or(5) as u8;
    let height_rand_a = v["height_rand_a"].as_u64().unwrap_or(0) as u8;
    let height_rand_b = v["height_rand_b"].as_u64().unwrap_or(0) as u8;
    let r#type = match type_id(v).as_ref() {
        "minecraft:forking_trunk_placer" => TrunkType::Forking(ForkingTrunkPlacer),
        "minecraft:giant_trunk_placer" => TrunkType::Giant(GiantTrunkPlacer),
        "minecraft:mega_jungle_trunk_placer" => TrunkType::MegaJungle(MegaJungleTrunkPlacer),
        "minecraft:dark_oak_trunk_placer" => TrunkType::DarkOak(DarkOakTrunkPlacer),
        "minecraft:fancy_trunk_placer" => TrunkType::Fancy(FancyTrunkPlacer),
        "minecraft:bending_trunk_placer" => TrunkType::Bending(BendingTrunkPlacer {
            min_height_for_leaves: v["min_height_for_leaves"].as_u64().unwrap_or(1) as u32,
            bend_length: parse_int_provider(&v["bend_length"]),
        }),
        "minecraft:upwards_branching_trunk_placer" => {
            TrunkType::UpwardsBranching(UpwardsBranchingTrunkPlacer {
                extra_branch_steps: parse_int_provider(&v["extra_branch_steps"]),
                place_branch_per_log_probability: f32_of(
                    v,
                    "place_branch_per_log_probability",
                    0.0,
                ),
                extra_branch_length: parse_int_provider(&v["extra_branch_length"]),
                can_grow_through: parse_block_id_list(&v["can_grow_through"]),
            })
        }
        "minecraft:cherry_trunk_placer" => TrunkType::Cherry(CherryTrunkPlacer {
            count: parse_int_provider(&v["branch_count"]),
            horizontal_length: parse_int_provider(&v["branch_horizontal_length"]),
            start_offset_from_top: UniformIntProvider {
                min_inclusive: i32_of(&v["branch_start_offset_from_top"], "min_inclusive", 0),
                max_inclusive: i32_of(&v["branch_start_offset_from_top"], "max_inclusive", 0),
            },
            end_offset_from_top: parse_int_provider(&v["branch_end_offset_from_top"]),
        }),
        // straight + fallback
        _ => TrunkType::Straight(StraightTrunkPlacer),
    };
    TrunkPlacer {
        base_height,
        height_rand_a,
        height_rand_b,
        r#type,
    }
}

fn parse_foliage_placer(v: &Value) -> FoliagePlacer {
    let radius = parse_int_provider(&v["radius"]);
    let offset = parse_int_provider(&v["offset"]);
    let r#type = match type_id(v).as_ref() {
        "minecraft:spruce_foliage_placer" => FoliageType::Spruce(SpruceFoliagePlacer {
            trunk_height: parse_int_provider(&v["trunk_height"]),
        }),
        "minecraft:pine_foliage_placer" => FoliageType::Pine(PineFoliagePlacer {
            height: parse_int_provider(&v["height"]),
        }),
        "minecraft:acacia_foliage_placer" => FoliageType::Acacia(AcaciaFoliagePlacer),
        "minecraft:bush_foliage_placer" => FoliageType::Bush(BushFoliagePlacer {
            height: i32_of(v, "height", 2),
        }),
        "minecraft:fancy_foliage_placer" => FoliageType::Fancy(LargeOakFoliagePlacer {
            height: i32_of(v, "height", 4),
        }),
        "minecraft:jungle_foliage_placer" => FoliageType::Jungle(JungleFoliagePlacer {
            height: i32_of(v, "height", 2),
        }),
        "minecraft:mega_pine_foliage_placer" => FoliageType::MegaPine(MegaPineFoliagePlacer {
            crown_height: parse_int_provider(&v["crown_height"]),
        }),
        "minecraft:dark_oak_foliage_placer" => FoliageType::DarkOak(DarkOakFoliagePlacer),
        "minecraft:random_spread_foliage_placer" => {
            FoliageType::RandomSpread(RandomSpreadFoliagePlacer {
                foliage_height: parse_int_provider(&v["foliage_height"]),
                leaf_placement_attempts: i32_of(v, "leaf_placement_attempts", 128),
            })
        }
        "minecraft:cherry_foliage_placer" => FoliageType::Cherry(CherryFoliagePlacer {
            height: parse_int_provider(&v["height"]),
            wide_bottom_layer_hole_chance: f32_of(v, "wide_bottom_layer_hole_chance", 0.0),
            corner_hole_chance: f32_of(v, "corner_hole_chance", 0.0),
            hanging_leaves_chance: f32_of(v, "hanging_leaves_chance", 0.0),
            hanging_leaves_extension_chance: f32_of(v, "hanging_leaves_extension_chance", 0.0),
        }),
        // blob + fallback
        _ => FoliageType::Blob(BlobFoliagePlacer {
            height: i32_of(v, "height", 3),
        }),
    };
    FoliagePlacer {
        radius,
        offset,
        r#type,
    }
}

fn parse_feature_size(v: &Value) -> FeatureSize {
    let min_clipped_height = v["min_clipped_height"].as_u64().map(|x| x as u8);
    let r#type = match type_id(v).as_ref() {
        "minecraft:three_layers_feature_size" => {
            FeatureSizeType::ThreeLayersFeatureSize(ThreeLayersFeatureSize {
                limit: v["limit"].as_u64().unwrap_or(1) as u8,
                upper_limit: v["upper_limit"].as_u64().unwrap_or(1) as u8,
                lower_size: v["lower_size"].as_u64().unwrap_or(0) as u8,
                middle_size: v["middle_size"].as_u64().unwrap_or(1) as u8,
                upper_size: v["upper_size"].as_u64().unwrap_or(1) as u8,
            })
        }
        // two_layers + fallback
        _ => FeatureSizeType::TwoLayersFeatureSize(TwoLayersFeatureSize {
            limit: v["limit"].as_u64().unwrap_or(1) as u8,
            lower_size: v["lower_size"].as_u64().unwrap_or(0) as u8,
            upper_size: v["upper_size"].as_u64().unwrap_or(1) as u8,
        }),
    };
    FeatureSize {
        min_clipped_height,
        r#type,
    }
}

fn parse_tree_decorator(v: &Value) -> Option<TreeDecorator> {
    Some(match type_id(v).as_ref() {
        "minecraft:trunk_vine" => TreeDecorator::TrunkVine(TrunkVineTreeDecorator),
        "minecraft:leave_vine" => TreeDecorator::LeaveVine(LeavesVineTreeDecorator {
            probability: f32_of(v, "probability", 0.0),
        }),
        "minecraft:cocoa" => TreeDecorator::Cocoa(CocoaTreeDecorator {}),
        "minecraft:beehive" => TreeDecorator::Beehive(BeehiveTreeDecorator {
            probability: f32_of(v, "probability", 0.0),
        }),
        "minecraft:alter_ground" => TreeDecorator::AlterGround(AlterGroundTreeDecorator {}),
        "minecraft:attached_to_logs" => TreeDecorator::AttachedToLogs(AttachedToLogsTreeDecorator {
            probability: f32_of(v, "probability", 0.0),
            block_provider: parse_block_state_provider(&v["block_provider"]),
            directions: v["directions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| d.as_str().map(parse_block_direction))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "minecraft:attached_to_leaves" => {
            TreeDecorator::AttachedToLeaves(AttachedToLeavesTreeDecorator {})
        }
        "minecraft:place_on_ground" => TreeDecorator::PlaceOnGround(PlaceOnGroundTreeDecorator {
            tries: i32_of(v, "tries", 1),
            radius: i32_of(v, "radius", 1),
            height: i32_of(v, "height", 1),
            block_state_provider: parse_block_state_provider(&v["block_state_provider"]),
        }),
        "minecraft:creaking_heart" => TreeDecorator::CreakingHeart(CreakingHeartTreeDecorator {}),
        "minecraft:pale_moss" => TreeDecorator::PaleMoss(PaleMossTreeDecorator {}),
        _ => return None,
    })
}

fn parse_root_placer(v: &Value, _r: &dyn FeatureRefResolver) -> Option<RootPlacer> {
    match type_id(v).as_ref() {
        "minecraft:mangrove_root_placer" => {
            let mrp = &v["mangrove_root_placement"];
            Some(RootPlacer::Mangrove(MangroveRootPlacer {
                trunk_offset_y: parse_int_provider(&v["trunk_offset_y"]),
                root_provider: parse_block_state_provider(&v["root_provider"]),
                above_root_placement: v
                    .get("above_root_placement")
                    .filter(|a| !a.is_null())
                    .map(|a| AboveRootPlacement {
                        above_root_provider: parse_block_state_provider(&a["above_root_provider"]),
                        above_root_placement_chance: f32_of(a, "above_root_placement_chance", 0.0),
                    }),
                mangrove_root_placement: MangroveRootPlacement {
                    can_grow_through: parse_block_id_list(&mrp["can_grow_through"]),
                    muddy_roots_in: parse_block_id_list(&mrp["muddy_roots_in"]),
                    muddy_roots_provider: parse_block_state_provider(&mrp["muddy_roots_provider"]),
                    max_root_width: i32_of(mrp, "max_root_width", 8),
                    max_root_length: i32_of(mrp, "max_root_length", 15),
                    random_skew_chance: f32_of(mrp, "random_skew_chance", 0.0),
                },
            }))
        }
        _ => None,
    }
}

/// Parse a block list or `#tag` into a leaked `&'static [u16]` of block ids.
fn parse_block_id_list(v: &Value) -> &'static [u16] {
    if let Some(tag) = v.as_str() {
        return resolve_block_tag(tag).1;
    }
    let ids: Vec<u16> = v
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.as_str())
                .filter_map(|s| Block::from_name(s).map(|blk| blk.id.as_u16()))
                .collect()
        })
        .unwrap_or_default();
    Box::leak(ids.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolver that treats every reference as a no-op (feature-parse unit tests do
    /// not exercise cross-feature references).
    struct NoopResolver;
    impl FeatureRefResolver for NoopResolver {
        fn resolve_configured_ref(&self, _id: &str) -> Feature {
            Feature::Inlined(Box::new(ConfiguredFeature::NoOp))
        }
        fn resolve_placed_wrapper(&self, _id: &str) -> PlacedFeatureWrapper {
            PlacedFeatureWrapper::Direct(PlacedFeature {
                feature: Feature::Inlined(Box::new(ConfiguredFeature::NoOp)),
                placement: Vec::new(),
            })
        }
    }

    #[test]
    fn parses_ore_feature() {
        let v = serde_json::json!({
            "type": "minecraft:ore",
            "config": {
                "size": 9,
                "discard_chance_on_air_exposure": 0.5,
                "targets": [{
                    "target": { "predicate_type": "minecraft:tag_match", "tag": "minecraft:stone_ore_replaceables" },
                    "state": { "Name": "minecraft:iron_ore" }
                }]
            }
        });
        let ConfiguredFeature::Ore(ore) = parse_configured_feature(&v, &NoopResolver) else {
            panic!("expected an Ore feature");
        };
        assert_eq!(ore.size, 9);
        assert!((ore.discard_chance_on_air_exposure - 0.5).abs() < f32::EPSILON);
        assert_eq!(ore.targets.len(), 1);
        assert!(matches!(ore.targets[0].target, RuleTest::TagMatch(_)));
        assert_eq!(ore.targets[0].state.id, Block::IRON_ORE.default_state.id);
    }

    #[test]
    fn parses_weighted_state_provider_simple_block() {
        let v = serde_json::json!({
            "type": "minecraft:simple_block",
            "config": { "to_place": {
                "type": "minecraft:weighted_state_provider",
                "entries": [
                    { "weight": 3, "data": { "Name": "minecraft:dandelion" } },
                    { "weight": 1, "data": { "Name": "minecraft:poppy" } }
                ]
            }}
        });
        let ConfiguredFeature::SimpleBlock(f) = parse_configured_feature(&v, &NoopResolver) else {
            panic!("expected a SimpleBlock feature");
        };
        let BlockStateProvider::Weighted(w) = f.to_place else {
            panic!("expected a weighted provider");
        };
        assert_eq!(w.entries.len(), 2);
        assert_eq!(w.entries[0].weight, 3);
    }

    #[test]
    fn keeps_biome_placement_modifier() {
        let v = serde_json::json!({
            "feature": "minecraft:trees_birch",
            "placement": [
                { "type": "minecraft:count", "count": 10 },
                { "type": "minecraft:in_square" },
                { "type": "minecraft:heightmap", "heightmap": "OCEAN_FLOOR" },
                { "type": "minecraft:biome" }
            ]
        });
        let pf = parse_placed_feature(&v, &NoopResolver);
        // The `minecraft:biome` modifier is KEPT: it does a datapack-aware per-position
        // biome check that confines the feature to its biome (fixes cross-biome bleeding
        // and deep-dark sculk leaking onto the surface).
        assert_eq!(pf.placement.len(), 4);
        assert!(matches!(pf.placement[0], PlacementModifier::Count(_)));
        assert!(matches!(pf.placement[1], PlacementModifier::InSquare(_)));
        assert!(matches!(pf.placement[2], PlacementModifier::Heightmap(_)));
        assert!(matches!(pf.placement[3], PlacementModifier::Biome(_)));
    }

    #[test]
    fn parses_bare_type_id_placement_modifiers() {
        // Terralith writes some placement modifiers with BARE type ids (no `minecraft:`
        // prefix) — e.g. `terralith:yellowstone/lakes` uses `{"type":"count"}` and
        // `{"type":"environment_scan"}`. The codec must normalize these; dropping the
        // `environment_scan` made `simple_block` water spill onto the surface heightmap
        // (floating water). This mirrors that placed feature's placement list.
        let v = serde_json::json!({
            "feature": "minecraft:oak",
            "placement": [
                { "type": "minecraft:count", "count": 20 },
                { "type": "minecraft:in_square" },
                { "type": "minecraft:heightmap", "heightmap": "WORLD_SURFACE_WG" },
                { "type": "minecraft:biome" },
                { "type": "count", "count": { "type": "minecraft:uniform", "min_inclusive": 5, "max_inclusive": 5 } },
                { "type": "minecraft:random_offset", "xz_spread": { "type": "minecraft:uniform", "min_inclusive": -4, "max_inclusive": 4 }, "y_spread": 0 },
                { "type": "environment_scan", "direction_of_search": "down", "max_steps": 6,
                  "target_condition": { "type": "minecraft:matching_blocks", "blocks": ["minecraft:yellow_terracotta"], "offset": [0, 0, 0] } }
            ]
        });
        let pf = parse_placed_feature(&v, &NoopResolver);
        // All 7 modifiers must survive — the bare-id `count` (#5) and `environment_scan`
        // (#7) used to be silently dropped.
        assert_eq!(pf.placement.len(), 7, "bare-id modifiers were dropped");
        assert!(matches!(pf.placement[4], PlacementModifier::Count(_)));
        assert!(matches!(
            pf.placement[6],
            PlacementModifier::EnvironmentScan(_)
        ));
    }

    #[test]
    fn int_provider_trapezoid_reads_min_max() {
        // Vanilla int-provider trapezoid uses `min`/`max` (not `*_inclusive`).
        let v = serde_json::json!({ "type": "minecraft:trapezoid", "min": -4, "max": 4, "plateau": 2 });
        let ip = parse_int_provider(&v);
        assert_eq!(ip.get_min(), -4);
        assert_eq!(ip.get_max(), 4);
    }

    #[test]
    fn unknown_feature_type_degrades_to_noop() {
        let v = serde_json::json!({ "type": "somemod:custom_feature", "config": {} });
        assert!(matches!(
            parse_configured_feature(&v, &NoopResolver),
            ConfiguredFeature::NoOp
        ));
    }

    #[test]
    fn parses_a_tree_feature() {
        let v = serde_json::json!({
            "type": "minecraft:tree",
            "config": {
                "trunk_provider": { "type": "minecraft:simple_state_provider", "state": { "Name": "minecraft:oak_log" } },
                "trunk_placer": { "type": "minecraft:straight_trunk_placer", "base_height": 4, "height_rand_a": 2, "height_rand_b": 0 },
                "foliage_provider": { "type": "minecraft:simple_state_provider", "state": { "Name": "minecraft:oak_leaves" } },
                "foliage_placer": { "type": "minecraft:blob_foliage_placer", "radius": 2, "offset": 0, "height": 3 },
                "minimum_size": { "type": "minecraft:two_layers_feature_size", "limit": 1, "lower_size": 0, "upper_size": 1 },
                "dirt_provider": { "type": "minecraft:simple_state_provider", "state": { "Name": "minecraft:dirt" } },
                "decorators": []
            }
        });
        let ConfiguredFeature::Tree(tree) = parse_configured_feature(&v, &NoopResolver) else {
            panic!("expected a Tree feature");
        };
        assert_eq!(tree.trunk_placer.base_height, 4);
        assert!(matches!(tree.trunk_placer.r#type, TrunkType::Straight(_)));
        assert!(matches!(tree.foliage_placer.r#type, FoliageType::Blob(_)));
    }
}
