use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_SET_COMMAND_BLOCK;
use pumpkin_macros::java_packet;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

use crate::codec::var_int::VarInt;

#[java_packet(PLAY_SET_COMMAND_BLOCK)]
pub struct SSetCommandBlock {
    pub pos: BlockPos,
    pub command: String,
    pub mode: VarInt,

    /// Operation mode flags
    /// - 0x01: Track output
    /// - 0x02: Is conditional
    /// - 0x04: Automatic
    pub flags: i8,
}

impl ServerPacket for SSetCommandBlock {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            pos: BlockPos::from_i64(bytebuf.get_i64_be()?),
            command: bytebuf.get_str_bounded(32767)?.into_string(),
            mode: bytebuf.get_var_int()?,
            flags: bytebuf.get_i8()?,
        })
    }
}

pub enum CommandBlockMode {
    Chain,
    Repeating,
    /// Redstone only
    Impulse,
}

impl TryFrom<VarInt> for CommandBlockMode {
    type Error = ();

    fn try_from(value: VarInt) -> Result<Self, Self::Error> {
        match value.0 {
            0 => Ok(Self::Chain),
            1 => Ok(Self::Repeating),
            2 => Ok(Self::Impulse),
            _ => Err(()),
        }
    }
}
