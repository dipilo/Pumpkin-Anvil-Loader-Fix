use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_SET_JIGSAW_BLOCK;
use pumpkin_macros::java_packet;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

use crate::codec::var_int::VarInt;

#[java_packet(PLAY_SET_JIGSAW_BLOCK)]
pub struct SSetJigsawBlock {
    pub pos: BlockPos,
    pub name: String,
    pub target: String,
    pub pool: String,
    pub final_state: String,
    pub joint: String,
    pub selection_priority: VarInt,
    pub placement_priority: VarInt,
}

impl ServerPacket for SSetJigsawBlock {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            pos: BlockPos::from_i64(bytebuf.get_i64_be()?),
            name: bytebuf.get_str_bounded(32767)?.into_string(),
            target: bytebuf.get_str_bounded(32767)?.into_string(),
            pool: bytebuf.get_str_bounded(32767)?.into_string(),
            final_state: bytebuf.get_str_bounded(32767)?.into_string(),
            joint: bytebuf.get_str_bounded(32767)?.into_string(),
            selection_priority: bytebuf.get_var_int()?,
            placement_priority: bytebuf.get_var_int()?,
        })
    }
}
