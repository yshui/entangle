pub mod generator;
use ::anyhow::{anyhow, Context, Result};
use ::async_std::net::{self, SocketAddr, ToSocketAddrs};
use ::log::*;
use ::sodiumoxide::crypto::{
    aead,
    box_::{self, PublicKey, SecretKey},
    kx::{self, SessionKey},
};
use ::std::collections::{hash_map::Entry, HashMap, HashSet};
use ::std::pin::Pin;
use generator::{Generator, GeneratorState, Turnable};

#[async_trait::async_trait]
pub trait Socket {
    async fn recv(&mut self) -> Result<(SocketAddr, Vec<u8>)>;
    async fn connect(
        &mut self,
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<()>;
    async fn send(&mut self, buf: &[u8]) -> Result<usize>;
    async fn send_to(
        &mut self,
        buf: &[u8],
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<usize>;
}

#[async_trait::async_trait]
impl Socket for net::UdpSocket {
    async fn recv(&mut self) -> Result<(SocketAddr, Vec<u8>)> {
        use ::nix::sys::socket::{recvmsg, MsgFlags};
        use ::std::os::unix::io::AsRawFd;
        let _ = self.peek(&mut []).await?;
        let size = {
            let fd = self.as_raw_fd();
            recvmsg(fd, &[], None, MsgFlags::MSG_PEEK | MsgFlags::MSG_TRUNC)?.bytes
        };
        let mut buf: Vec<u8> = Vec::with_capacity(size);
        buf.resize_with(size, Default::default);

        let (size, addr) = self.recv_from(buf.as_mut_slice()).await?;
        assert_eq!(size, buf.len());
        Ok((addr, buf))
    }
    async fn connect(
        &mut self,
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<()> {
        Ok(net::UdpSocket::connect(self, addr).await?)
    }
    async fn send_to(
        &mut self,
        buf: &[u8],
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<usize> {
        Ok(net::UdpSocket::send_to(self, buf, addr).await?)
    }

    async fn send(&mut self, buf: &[u8]) -> Result<usize> {
        Ok(net::UdpSocket::send(self, buf).await?)
    }
}

type HandshakeGenerator = dyn Turnable<Vec<u8>, Option<Vec<u8>>, Result<(SessionKey, SessionKey)>>
    + Send
    + Sync
    + 'static;
enum AuthState {
    Initiated(Pin<Box<HandshakeGenerator>>),
    Completed((aead::Key, aead::Key)),
}

pub struct CDGramServer<T> {
    /// Our public key
    _public: PublicKey,
    /// Our secret key
    secret: SecretKey,
    authorized_keys: HashSet<PublicKey>,
    socket: T,
    auth_states: HashMap<SocketAddr, AuthState>,
}

impl<T: 'static> CDGramServer<T> {
    pub fn new(
        public: PublicKey,
        secret: SecretKey,
        authorized_keys: impl IntoIterator<Item = PublicKey>,
        socket: T,
    ) -> Self {
        Self {
            _public: public,
            secret,
            socket,
            authorized_keys: authorized_keys.into_iter().collect(),
            auth_states: Default::default(),
        }
    }
}

// TODO(yshui) Handle disconnection and reset
async fn handshake(
    our_sk: SecretKey,
    mut s: GeneratorState<Vec<u8>, Option<Vec<u8>>>,
) -> Result<(SessionKey, SessionKey)> {
    // First packet, client pubkey + ephemeral key exchange pubkey + client challenge
    let pkt = s.yield_(None).await;
    if pkt.len() != kx::PUBLICKEYBYTES + box_::PUBLICKEYBYTES + 32 {
        return Err(anyhow!("Malformed initial handshake packet"));
    }
    let client_pk = PublicKey::from_slice(&pkt[0..box_::PUBLICKEYBYTES]).unwrap();
    let client_kx_pk = kx::PublicKey::from_slice(
        &pkt[box_::PUBLICKEYBYTES..(box_::PUBLICKEYBYTES + kx::PUBLICKEYBYTES)],
    )
    .unwrap();
    let nonce = box_::gen_nonce();
    let response = box_::seal(
        &pkt[(kx::PUBLICKEYBYTES + box_::PUBLICKEYBYTES)..],
        &nonce,
        &client_pk,
        &our_sk,
    );
    // First reply. server challenge + ephemeral key change pubkey + response to client challenge
    let (kx_pk, kx_sk) = kx::gen_keypair();
    let challenge = ::sodiumoxide::randombytes::randombytes(32);
    let mut send = challenge.clone();
    send.extend(kx_pk.as_ref());
    send.extend(nonce.as_ref());
    send.extend(response.as_slice());

    // Second packet, response to the challenge. A box containing the challenge, created with
    // client secret key + our public key
    let pkt = s.yield_(Some(send)).await;
    if pkt.len() != 32 + box_::MACBYTES + box_::NONCEBYTES {
        return Err(anyhow!("Malformed "));
    }
    let nonce = box_::Nonce::from_slice(&pkt[0..box_::NONCEBYTES]).unwrap();
    debug!("Received client response nonce {:?}", nonce.as_ref());
    let response = box_::open(&pkt[box_::NONCEBYTES..], &nonce, &client_pk, &our_sk)
        .map_err(|()| anyhow!("Client failed challenge"))?;
    if response != challenge {
        return Err(anyhow!("Client response doesn't match the challenge"));
    }

    kx::server_session_keys(&kx_pk, &kx_sk, &client_kx_pk)
        .map_err(|()| anyhow!("Failed to generate session keys"))
}
impl<T: Socket> CDGramServer<T> {
    pub async fn recv(&mut self) -> Result<(SocketAddr, Vec<u8>)> {
        loop {
            let (addr, buf) = self.socket.recv().await?;

            use ::either::Either;
            // Find session key
            let our_sk = self.secret.clone();
            let auth_state = self.auth_states.entry(addr);

            if let Entry::Vacant(_) = auth_state {
                info!("New connection from {}", addr);
                if buf.len() < box_::PUBLICKEYBYTES {
                    info!("{} Malformed handshake", addr);
                    continue;
                }
                let pubkey = box_::PublicKey::from_slice(&buf[0..box_::PUBLICKEYBYTES]).unwrap();
                if !self.authorized_keys.contains(&pubkey) {
                    // Unauthorized key, just drop the handshake packet
                    info!("{} sent us unauthorized pubkey", addr);
                    continue;
                }
            }
            let auth_state = auth_state.or_insert_with(|| {
                let mut g = Box::pin(Generator::new(|g| handshake(our_sk, g)));
                Pin::new(&mut g).start();
                AuthState::Initiated(g)
            });
            match auth_state {
                AuthState::Initiated(g) => match Pin::new(g).turn(buf) {
                    Either::Left(reply) => {
                        if let Some(reply) = reply {
                            debug!("Sending handshake{:?} to {}", reply, addr);
                            self.socket.send_to(reply.as_slice(), addr).await?;
                        }
                    }
                    Either::Right(Ok((rx, tx))) => {
                        *auth_state = AuthState::Completed((
                            aead::Key::from_slice(rx.as_ref()).unwrap(),
                            aead::Key::from_slice(tx.as_ref()).unwrap(),
                        ))
                    }
                    Either::Right(Err(e)) => {
                        error!("Handshake error with {}: {}", addr, e);
                        self.auth_states.remove(&addr);
                    }
                },
                AuthState::Completed((rx, _)) => {
                    let nonce = aead::Nonce::from_slice(&buf[0..aead::NONCEBYTES]).unwrap();
                    let ret = aead::open(&buf[aead::NONCEBYTES..], None, &nonce, &rx)
                        .map_err(|()| anyhow!("Failed to decrypt client package"))?;
                    return Ok((addr, ret));
                }
            };
        }
    }

    pub async fn send(&mut self, addr: impl ToSocketAddrs, buf: &[u8]) -> Result<usize> {
        let addr = addr
            .to_socket_addrs()
            .await?
            .next()
            .with_context(|| "Failed to resolve address".to_owned())?;
        let auth_state = self
            .auth_states
            .get(&addr)
            .with_context(|| format!("Trying to send to unknown client {}", addr))?;
        debug!("Sending packet to {}", addr);
        match auth_state {
            AuthState::Completed((_, tx)) => {
                let nonce = aead::gen_nonce();
                let c = aead::seal(buf, None, &nonce, &tx);
                let mut send = nonce.as_ref().to_vec();
                send.extend(c.as_slice());
                Ok(self.socket.send_to(send.as_slice(), addr).await?)
            }
            AuthState::Initiated(_) => {
                return Err(anyhow!(
                    "Trying to send to a client {} in the middle of handshake",
                    addr
                ))
            }
        }
    }
}

pub struct CDGramClient<T> {
    /// Our public key
    public: PublicKey,
    /// Our secret key
    secret: SecretKey,
    /// Server's public key
    server_public: PublicKey,
    session_keys: Option<(aead::Key, aead::Key)>,
    socket: T,
}

impl<T: 'static> CDGramClient<T> {
    pub fn new(public: PublicKey, secret: SecretKey, server_public: PublicKey, socket: T) -> Self {
        Self {
            public,
            secret,
            server_public,
            socket,
            session_keys: None,
        }
    }
}
impl<T: Socket> CDGramClient<T> {
    pub async fn connect(&mut self, addr: SocketAddr) -> Result<()> {
        let (pk, sk) = kx::gen_keypair();
        let mut send = self.public.as_ref().to_vec();
        let challenge = ::sodiumoxide::randombytes::randombytes(32);
        send.extend(pk.as_ref());
        send.extend(challenge.as_slice());

        self.socket.connect(addr).await?;
        self.socket.send(send.as_slice()).await?;
        debug!("Client sent handshake to {}", addr);

        let (_, reply) = self.socket.recv().await?;
        debug!("Client got handshake reply");
        if reply.len() != 32 + kx::PUBLICKEYBYTES + box_::NONCEBYTES + 32 + box_::MACBYTES {
            return Err(anyhow!("Malformed server reply"));
        }
        let server_kx_pk =
            kx::PublicKey::from_slice(&reply[32..(32 + kx::PUBLICKEYBYTES)]).unwrap();

        let nonce = box_::Nonce::from_slice(
            &reply[(32 + kx::PUBLICKEYBYTES)..(32 + kx::PUBLICKEYBYTES + box_::NONCEBYTES)],
        )
        .unwrap();
        let server_response = box_::open(
            &reply[(32 + kx::PUBLICKEYBYTES + box_::NONCEBYTES)..],
            &nonce,
            &self.server_public,
            &self.secret,
        )
        .map_err(|()| anyhow!("Server failed challenge"))?;
        if server_response != challenge {
            return Err(anyhow!("Server reponse doesn't match the challenge"));
        }

        let nonce = box_::gen_nonce();
        let response = box_::seal(&reply[0..32], &nonce, &self.server_public, &self.secret);
        let mut send = nonce.as_ref().to_vec();
        send.extend(response.as_slice());
        self.socket.send(send.as_slice()).await?;
        debug!("Client sent handshake finish, nonce {:?}", nonce.as_ref());

        let (rx, tx) = kx::client_session_keys(&pk, &sk, &server_kx_pk)
            .map_err(|()| anyhow!("Failed to generate session keys"))?;
        self.session_keys = Some((
            aead::Key::from_slice(rx.as_ref()).unwrap(),
            aead::Key::from_slice(tx.as_ref()).unwrap(),
        ));

        Ok(())
    }

    pub async fn send(&mut self, buf: &[u8]) -> Result<usize> {
        if let Some((_, tx)) = self.session_keys.as_ref() {
            let nonce = aead::gen_nonce();
            let c = aead::seal(buf, None, &nonce, &tx);
            let mut send = nonce.as_ref().to_vec();
            send.extend(c.as_slice());
            Ok(self.socket.send(send.as_slice()).await?)
        } else {
            Err(anyhow!("Client not connected yet"))
        }
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        if let Some((rx, _)) = self.session_keys.as_ref() {
            let (_, pkt) = self.socket.recv().await?;
            let nonce = aead::Nonce::from_slice(&pkt[0..aead::NONCEBYTES]).unwrap();
            aead::open(&pkt[aead::NONCEBYTES..], None, &nonce, &rx)
                .map_err(|()| anyhow!("Failed to decrypt server message"))
        } else {
            Err(anyhow!("Client not connected yet"))
        }
    }
}

#[cfg(any(test, feature = "mock"))]
pub mod tests;
