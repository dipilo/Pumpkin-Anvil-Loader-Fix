use pumpkin_data::packet::serverbound::{PLAY_PICK_ITEM_FROM_BLOCK, PLAY_PICK_ITEM_FROM_ENTITY};
use pumpkin_macros::java_packet;
use pumpkin_util::math::position::BlockPos;

use crate::codec::var_int::VarInt;
use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_PICK_ITEM_FROM_BLOCK)]
pub struct SPickItemFromBlock {
    pub pos: BlockPos,
    pub include_data: bool,
}

impl ServerPacket for SPickItemFromBlock {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            pos: BlockPos::from_i64(bytebuf.get_i64_be()?),
            include_data: bytebuf.get_bool()?,
        })
    }
}

#[java_packet(PLAY_PICK_ITEM_FROM_ENTITY)]
pub struct SPickItemFromEntity {
    pub id: VarInt,
    pub include_data: bool,
}

impl ServerPacket for SPickItemFromEntity {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            id: bytebuf.get_var_int()?,
            include_data: bytebuf.get_bool()?,
        })
    }
}
