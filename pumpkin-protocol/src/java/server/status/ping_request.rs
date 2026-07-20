use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::STATUS_PING_REQUEST;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

/// Sent by the client to measure the round-trip time (latency) to the server.
///
/// This is the second part of the Server List Ping (SLP) process
/// The server should respond with `CPingResponse`.
#[java_packet(STATUS_PING_REQUEST)]
pub struct SStatusPingRequest {
    pub payload: i64,
}

impl ServerPacket for SStatusPingRequest {
    fn read(
        mut bytebuf: impl Read,
        _protocol_version: &JavaMinecraftVersion,
    ) -> Result<Self, ReadingError> {
        Ok(Self {
            payload: bytebuf.get_i64_be()?,
        })
    }
}
