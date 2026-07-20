use pumpkin_data::packet::serverbound::PLAY_SELECT_TRADE;
use pumpkin_macros::java_packet;

use crate::VarInt;

use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_SELECT_TRADE)]
pub struct SSelectTrade {
    pub selected_slot: VarInt,
}

impl ServerPacket for SSelectTrade {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            selected_slot: bytebuf.get_var_int()?,
        })
    }
}
