use ::serde_derive::{Deserialize, Serialize};
use ::std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientMessage {
    /// Sychronize the list of input devices and their states.
    /// Client sends a list of input devices it knows about, and then
    /// updates its list based on the InputDeviceUpdate reply from the server
    Sync(HashMap<u32, InputDevice>),
    KeepAlive,
    Ping,
}

#[derive(Serialize, Deserialize)]
pub enum ServerMessage {
    /// Sychronize the list of input devices and their states
    Sync(HashMap<u32, InputDeviceUpdate>),
    /// Input event
    Event((u32, InputEvent)),
    Pong,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum InputDeviceUpdate {
    /// This input device has updated states
    Update(InputDevice),
    /// This input device has been dropped from the server
    Drop,
}

use ::fixedbitset::FixedBitSet;
#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Clone)]
pub struct InputDevice {
    /// Available keys
    #[serde(with = "fixedbitset")]
    pub key_bits: FixedBitSet,
    /// Available relative axes
    #[serde(with = "fixedbitset")]
    pub rel_bits: FixedBitSet,
    /// Supported event types (right now keys and rel)
    #[serde(with = "fixedbitset")]
    pub cap: FixedBitSet,
    /// Device name
    pub name: String,
    /// Currently pressed keys
    #[serde(with = "fixedbitset")]
    pub key_vals: FixedBitSet,
    /// VID
    pub vendor: u16,
    /// PID
    pub product: u16,
    /// Hardware revision?
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

mod fixedbitset {
    use ::fixedbitset::FixedBitSet;
    use ::serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &FixedBitSet, ser: S) -> Result<S::Ok, S::Error> {
        use ::byteorder::{ByteOrder, LittleEndian};
        let i = v.as_slice();
        let mut buf = Vec::with_capacity(i.len() * 4);
        buf.resize(i.len() * 4, 0);
        LittleEndian::write_u32_into(i, &mut buf);
        ser.serialize_bytes(&buf)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<FixedBitSet, D::Error> {
        use ::serde::de::Error;
        use ::serde_bytes::Deserialize;
        let cow: ::std::borrow::Cow<[u8]> = Deserialize::deserialize(de)?;

        use ::byteorder::{ByteOrder, LittleEndian};
        if cow.len() % 4 != 0 {
            return Err(<D as Deserializer>::Error::custom("byte array not aligned"));
        }

        let mut ret = FixedBitSet::with_capacity(cow.len() * 8);
        LittleEndian::read_u32_into(&cow, ret.as_mut_slice());
        Ok(ret)
    }
}
