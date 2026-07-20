use pumpkin_data::packet::clientbound::PLAY_EXPLODE;
use pumpkin_macros::java_packet;
use pumpkin_util::math::vector3::Vector3;

use crate::ClientPacket;
use crate::ser::NetworkWriteExt;
use crate::{IdOr, SoundEvent, codec::var_int::VarInt};
use pumpkin_util::version::JavaMinecraftVersion;

/// Notifies the client that an explosion has occurred.
///
/// This is a high-level packet that handles the visual, auditory, and physical
/// effects of an explosion in a single call. It triggers the explosion particles,
/// plays the sound at the source, and applies knockback to the player.
#[java_packet(PLAY_EXPLODE)]
pub struct CExplosion {
    /// The center coordinates of the explosion.
    pub center: Vector3<f64>,
    /// The strength/radius of the explosion.
    /// Higher values increase the visual size of the particle effect.
    pub radius: f32,
    /// The number of blocks affected/destroyed.
    /// Note: The actual block list is typically handled via a separate packet or
    /// following byte array in older versions, but this field specifies the count.
    pub block_count: i32,
    /// The impulse/knockback applied to the player receiving this packet.
    /// If None, no velocity change is applied.
    pub knockback: Option<Vector3<f64>>,
    /// The ID of the particle to use for the explosion (e.g., `minecraft:explosion`).
    pub particle: VarInt,
    /// The sound to play (e.g., `minecraft:entity.generic.explode`).
    pub sound: IdOr<SoundEvent>,
    /// The size of the block particles pool, used for debris visuals.
    pub block_particles_pool_size: VarInt,
}

impl CExplosion {
    #[must_use]
    pub const fn new(
        center: Vector3<f64>,
        radius: f32,
        block_count: i32,
        knockback: Option<Vector3<f64>>,
        particle: VarInt,
        sound: IdOr<SoundEvent>,
    ) -> Self {
        Self {
            center,
            radius,
            block_count,
            knockback,
            particle,
            sound,
            block_particles_pool_size: VarInt(0),
        }
    }
}

impl ClientPacket for CExplosion {
    fn write_packet_data(
        &self,
        mut write: impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), crate::ser::WritingError> {
        write.write_f64_be(self.center.x)?;
        write.write_f64_be(self.center.y)?;
        write.write_f64_be(self.center.z)?;
        write.write_f32_be(self.radius)?;
        write.write_i32_be(self.block_count)?;
        write.write_option(&self.knockback, |w, k| {
            w.write_f64_be(k.x)?;
            w.write_f64_be(k.y)?;
            w.write_f64_be(k.z)?;
            Ok(())
        })?;
        write.write_var_int(&self.particle)?;
        match &self.sound {
            IdOr::Id(id) => write.write_var_int(&VarInt((*id + 1) as i32))?,
            IdOr::Value(event) => {
                write.write_var_int(&VarInt(0))?;
                write.write_string(&event.sound_name)?;
                write.write_option(&event.range, |w, r| w.write_f32_be(*r))?;
            }
        }
        write.write_var_int(&self.block_particles_pool_size)?;
        Ok(())
    }
}
