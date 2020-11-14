use super::Socket;
use ::anyhow::{anyhow, Result};
use ::async_std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs};
use ::async_std::sync::{Receiver, Sender};

pub struct MockSocket {
    tx: Sender<(SocketAddr, SocketAddr, Vec<u8>)>,
    rx: Receiver<(SocketAddr, SocketAddr, Vec<u8>)>,
    local: SocketAddr,
    remote: Option<SocketAddr>,
}

impl MockSocket {
    pub fn new(addr1: SocketAddr, addr2: SocketAddr) -> (MockSocket, MockSocket) {
        let (tx1, rx1) = ::async_std::sync::channel(1024);
        let (tx2, rx2) = ::async_std::sync::channel(1024);
        (
            MockSocket {
                tx: tx1,
                rx: rx2,
                local: addr1,
                remote: None,
            },
            MockSocket {
                tx: tx2,
                rx: rx1,
                local: addr2,
                remote: None,
            },
        )
    }
}

#[async_trait::async_trait]
impl Socket for MockSocket {
    async fn send(&mut self, buf: &[u8]) -> Result<usize> {
        if let Some(remote_addr) = self.remote {
            self.tx
                .send((self.local.clone(), remote_addr.clone(), buf.to_owned()))
                .await;
            Ok(buf.len())
        } else {
            Err(anyhow!("Socket not connected"))
        }
    }
    async fn send_to(
        &mut self,
        buf: &[u8],
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<usize> {
        if let Some(remote) = addr.to_socket_addrs().await?.next() {
            self.tx
                .send((self.local.clone(), remote.clone(), buf.to_owned()))
                .await;
            Ok(buf.len())
        } else {
            Err(anyhow!("Failed to resolve remote"))
        }
    }
    async fn connect(
        &mut self,
        addr: impl ToSocketAddrs<Iter = impl Iterator<Item = SocketAddr> + Send + 'static>
            + Send
            + Sync
            + 'static,
    ) -> Result<()> {
        self.remote = addr.to_socket_addrs().await?.next();
        Ok(())
    }
    async fn recv(&mut self) -> Result<(SocketAddr, Vec<u8>)> {
        loop {
            let (sender, receiver, payload) = self.rx.recv().await?;
            if let Some(remote_addr) = self.remote.as_ref() {
                if &sender != remote_addr {
                    continue;
                }
            }

            if receiver != self.local {
                continue;
            }
            break Ok((sender, payload));
        }
    }
}

pub fn random_addr() -> SocketAddr {
    let random = ::sodiumoxide::randombytes::randombytes(6);
    SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(random[0], random[1], random[2], random[3]),
        ((random[4] as u16) << 8) + random[5] as u16,
    ))
}

#[cfg(test)]
#[test]
fn test_connect() {
    use super::{CDGramClient, CDGramServer};
    ::env_logger::init();
    let (server_pk, server_sk) = ::sodiumoxide::crypto::box_::gen_keypair();
    let (client_pk, client_sk) = ::sodiumoxide::crypto::box_::gen_keypair();
    let (server_addr, client_addr) = (random_addr(), random_addr());
    let (server_sock, client_sock) = MockSocket::new(server_addr.clone(), client_addr.clone());
    let mut server = CDGramServer::new(
        server_pk,
        server_sk,
        ::std::iter::once(client_pk.clone()),
        server_sock,
    );
    let mut client = CDGramClient::new(client_pk, client_sk, server_pk, client_sock);

    ::async_std::task::block_on(async move {
        let recv_handle = ::async_std::task::spawn(async move {
            let (addr, pkt) = server.recv().await.unwrap();
            server.send(addr, &[5, 4, 3, 2, 1]).await.unwrap();
            (addr, pkt)
        });
        client.connect(server_addr).await.unwrap();
        client.send(&[1, 2, 3, 4, 5]).await.unwrap();
        let (addr, pkt) = recv_handle.await;
        assert_eq!(addr, client_addr);
        assert_eq!(&pkt[..], &[1, 2, 3, 4, 5]);

        let pkt = client.recv().await.unwrap();
        assert_eq!(&pkt[..], &[5, 4, 3, 2, 1]);
    })
}
