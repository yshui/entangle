use ::serde_derive::{Deserialize, Serialize};
use ::sodiumoxide::crypto::box_::{PublicKey, SecretKey, PUBLICKEYBYTES, SECRETKEYBYTES};
use ::std::mem::MaybeUninit;
mod base64;

#[derive(Serialize, Deserialize)]
pub struct Peer {
    pub addr: Option<::std::net::SocketAddr>,
    #[serde(with = "base64")]
    public: [u8; PUBLICKEYBYTES],
}

impl Peer {
    pub fn public(&self) -> PublicKey {
        PublicKey::from_slice(&self.public[..]).unwrap()
    }
    pub fn new(addr: Option<::std::net::SocketAddr>, pk: PublicKey) -> Self {
        let mut public = MaybeUninit::<[u8; PUBLICKEYBYTES]>::uninit();
        let public = unsafe {
            (*public.as_mut_ptr()).copy_from_slice(pk.as_ref());
            public.assume_init()
        };
        Self { addr, public }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(with = "base64")]
    public: [u8; PUBLICKEYBYTES],
    #[serde(with = "base64")]
    secret: [u8; SECRETKEYBYTES],
    pub peers: Vec<Peer>,
}

impl Config {
    pub fn public(&self) -> PublicKey {
        PublicKey::from_slice(&self.public[..]).unwrap()
    }
    pub fn secret(&self) -> SecretKey {
        SecretKey::from_slice(&self.secret[..]).unwrap()
    }
    pub fn generate() -> Self {
        let (pk, sk) = ::sodiumoxide::crypto::box_::gen_keypair();
        let (mut public, mut secret) = (
            MaybeUninit::<[u8; PUBLICKEYBYTES]>::uninit(),
            MaybeUninit::<[u8; SECRETKEYBYTES]>::uninit(),
        );

        let (public, secret) = unsafe {
            (*public.as_mut_ptr()).copy_from_slice(pk.as_ref());
            (*secret.as_mut_ptr()).copy_from_slice(sk.as_ref());
            (public.assume_init(), secret.assume_init())
        };
        Self {
            public,
            secret,
            peers: Vec::new(),
        }
    }
}
