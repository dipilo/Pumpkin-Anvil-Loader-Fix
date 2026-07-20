use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_RENAME_ITEM;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[derive(Debug)]
#[java_packet(PLAY_RENAME_ITEM)]
pub struct SRenameItem {
    pub item_name: String,
}

impl ServerPacket for SRenameItem {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            item_name: bytebuf.get_str_bounded(32767)?.into_string(),
        })
    }
}
