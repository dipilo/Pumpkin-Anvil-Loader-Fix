use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_ATTACK;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

use crate::codec::var_int::VarInt;

#[java_packet(PLAY_ATTACK)]
pub struct SAttack {
    pub entity_id: VarInt,
}

impl ServerPacket for SAttack {
    fn read(
        mut bytebuf: impl Read,
        _protocol_version: &JavaMinecraftVersion,
    ) -> Result<Self, ReadingError> {
        Ok(Self {
            entity_id: bytebuf.get_var_int()?,
        })
    }
}
