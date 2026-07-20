use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_SWING;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

use crate::VarInt;

#[java_packet(PLAY_SWING)]
pub struct SSwingArm {
    pub hand: VarInt,
}

impl ServerPacket for SSwingArm {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            hand: bytebuf.get_var_int()?,
        })
    }
}
