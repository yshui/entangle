use ::base64;
use serde::{de, Deserialize, Deserializer, Serializer};

pub fn serialize<S, Buf: AsRef<[u8]>>(bytes: &Buf, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&base64::encode_config(bytes, base64::URL_SAFE_NO_PAD))
}

pub fn deserialize<'de, D, Buf: AsMut<[u8]> + Default>(deserializer: D) -> Result<Buf, D::Error>
where
    D: Deserializer<'de>,
{
    use std::io::Write;
    let s = <&str>::deserialize(deserializer)?;
    let mut ret: Buf = Default::default();
    let v = base64::decode_config(s, base64::URL_SAFE_NO_PAD).map_err(de::Error::custom)?;
    if (&mut ret).as_mut().write(&v).map_err(de::Error::custom)? != v.len() {
        Err(de::Error::custom("Length mismatch"))
    } else {
        Ok(ret)
    }
}
