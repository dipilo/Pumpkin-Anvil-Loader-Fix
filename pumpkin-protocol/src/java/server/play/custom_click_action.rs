use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_CUSTOM_CLICK_ACTION;
use pumpkin_macros::java_packet;
use pumpkin_util::resource_location::ResourceLocation;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_CUSTOM_CLICK_ACTION)]
pub struct SCustomClickAction {
    pub action_id: ResourceLocation,
    pub payload: Option<Box<[u8]>>,
}

impl ServerPacket for SCustomClickAction {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            action_id: bytebuf.get_str()?.to_string(),
            payload: bytebuf.get_option(|b| b.read_remaining_to_boxed_slice(32767))?,
        })
    }
}
