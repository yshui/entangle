#[allow(unused_imports)]
use ::anyhow::{anyhow, Context, Result};
use ::argh::FromArgs;
use ::config::Config;
use ::static_assertions::const_assert;
use ::std::mem::MaybeUninit;
use ::std::net::SocketAddr;

#[derive(FromArgs)]
/// Pair host with client
struct Pair {
    /// listen for connection from a client
    #[argh(switch, short = 'l')]
    listen: bool,

    /// pair with a remote host
    #[argh(option, short = 's')]
    server: Option<SocketAddr>,
}

const_assert!(
    ::sodiumoxide::crypto::kx::SESSIONKEYBYTES
        >= ::sodiumoxide::crypto::pwhash::argon2id13::SALTBYTES
);

fn generate_pin(
    (a, b): (
        &::sodiumoxide::crypto::kx::SessionKey,
        &::sodiumoxide::crypto::kx::SessionKey,
    ),
) -> Result<u32> {
    use ::sodiumoxide::crypto::pwhash::argon2id13 as pwhash;
    let salt = pwhash::Salt::from_slice(&b[0..pwhash::SALTBYTES]).unwrap();
    let mut key: MaybeUninit<[u8; pwhash::HASHEDPASSWORDBYTES]> = MaybeUninit::uninit();
    pwhash::derive_key(
        unsafe { &mut *key.as_mut_ptr() },
        a.as_ref(),
        &salt,
        pwhash::OPSLIMIT_INTERACTIVE,
        pwhash::MEMLIMIT_INTERACTIVE,
    )
    .map_err(|()| anyhow!("Failed to calculate key"))?;
    let key = unsafe { key.assume_init() };

    use ::byteorder::{LittleEndian, ReadBytesExt};
    let pin = key.as_ref().read_u32::<LittleEndian>()?;
    Ok(pin)
}

fn ask(prompt: &str) -> Result<bool> {
    use ::std::io::Write;
    use ::termion::input::TermRead;
    use ::termion::raw::IntoRawMode;
    let key = {
        let mut keys = ::std::io::stdin().keys();
        let mut raw = ::std::io::stdout().into_raw_mode()?;
        write!(raw, "{}", prompt)?;
        raw.flush()?;

        keys.next().unwrap()?
    };
    Ok(if key == ::termion::event::Key::Char('y') {
        println!("yes");
        true
    } else {
        println!("no");
        false
    })
}
use ::sodiumoxide::crypto::{box_, kx};

/// Send an authenticated message
async fn send_auth(
    sock: &::async_std::net::UdpSocket,
    buf: &[u8],
    tx: &kx::SessionKey,
) -> Result<()> {
    use ::sodiumoxide::crypto::onetimeauth as auth;
    let tx = auth::Key::from_slice(tx.as_ref()).unwrap();
    let tag = auth::authenticate(buf, &tx);
    let mut data = Vec::new();
    data.extend(buf);
    data.extend(tag.as_ref());

    sock.send(data.as_slice()).await?;
    Ok(())
}

/// Receive an authenticated message
async fn recv_auth(
    sock: &::async_std::net::UdpSocket,
    buf: &mut [u8],
    rx: &kx::SessionKey,
) -> Result<usize> {
    use ::sodiumoxide::crypto::onetimeauth as auth;
    assert!(buf.len() > auth::TAGBYTES);
    let size = sock.recv(buf).await?;
    assert!(size <= buf.len());
    let tag = auth::Tag::from_slice(&buf[(size - auth::TAGBYTES)..size]).unwrap();

    let rx = auth::Key::from_slice(rx.as_ref()).unwrap();
    if !auth::verify(&tag, &buf[0..(size - auth::TAGBYTES)], &rx) {
        Err(anyhow!("Failed to verify the client message"))
    } else {
        Ok(size - auth::TAGBYTES)
    }
}

async fn accept_client(mut cfg: Config) -> Result<Config> {
    let sock = ::async_std::net::UdpSocket::bind("0.0.0.0:0").await?;
    // Temporary keys for pairing
    let (pk, sk) = kx::gen_keypair();
    let addr = sock.local_addr()?;
    println!("Waiting for client contact at {}", addr);

    let mut buf: MaybeUninit<[u8; kx::PUBLICKEYBYTES]> = MaybeUninit::uninit();
    let (size, remote_addr) = sock.recv_from(unsafe { &mut *buf.as_mut_ptr() }).await?;
    if size != ::std::mem::size_of_val(&buf) {
        return Err(anyhow!("Malformed handshake packet"));
    }

    let client_pk = unsafe { buf.assume_init() };

    sock.connect(remote_addr).await?;
    sock.send(pk.as_ref()).await?;

    // Generate temporary session keys
    let (rx, tx) = ::sodiumoxide::crypto::kx::server_session_keys(
        &pk,
        &sk,
        &kx::PublicKey::from_slice(&client_pk[..]).unwrap(),
    )
    .map_err(|()| anyhow!("Failed to generate the shared secret"))?;
    let pin = generate_pin((&rx, &tx))?;
    println!(
        "Please verify the client displays the same number as below\n\t{}",
        pin % 1_0000_0000
    );
    ask("Pair?(y/n)")?;

    // Receive client public key
    let mut buf = MaybeUninit::<[u8; 128]>::uninit();
    let client_pk_len = recv_auth(&sock, unsafe { &mut *buf.as_mut_ptr() }, &rx).await?;
    let client_pk = unsafe { &buf.assume_init()[0..client_pk_len] };
    let client_pk = box_::PublicKey::from_slice(client_pk).unwrap();
    cfg.peers.push(::config::Peer::new(None, client_pk));

    // Send server public key
    send_auth(&sock, cfg.public().as_ref(), &tx).await?;
    Ok(cfg)
}

async fn pair_server(mut cfg: Config, mut server: SocketAddr) -> Result<Config> {
    let sock = ::async_std::net::UdpSocket::bind("0.0.0.0:0").await?;
    // Temporary keys for pairing
    let (pk, sk) = kx::gen_keypair();
    sock.connect(server).await?;
    sock.send(pk.as_ref()).await?;

    let mut buf: MaybeUninit<[u8; kx::PUBLICKEYBYTES]> = MaybeUninit::uninit();
    let size = sock.recv(unsafe { &mut *buf.as_mut_ptr() }).await?;
    if size != ::std::mem::size_of_val(&buf) {
        return Err(anyhow!("Malformed handshake packet"));
    }
    let server_pk = unsafe { buf.assume_init() };

    // Generate temporary session keys
    let (rx, tx) = ::sodiumoxide::crypto::kx::client_session_keys(
        &pk,
        &sk,
        &kx::PublicKey::from_slice(&server_pk[..]).unwrap(),
    )
    .map_err(|()| anyhow!("Failed to generate the shared secret"))?;
    let pin = generate_pin((&tx, &rx))?;
    println!(
        "Please verify the server displays the same number as below\n\t{}",
        pin % 1_0000_0000
    );
    ask("Pair?(y/n)")?;

    // Send client public key
    send_auth(&sock, cfg.public().as_ref(), &tx).await?;

    // Receive server public key
    let mut buf = MaybeUninit::<[u8; 128]>::uninit();
    let server_pk_len = recv_auth(&sock, unsafe { &mut *buf.as_mut_ptr() }, &rx).await?;
    let server_pk = unsafe { &buf.assume_init()[0..server_pk_len] };
    let server_pk = box_::PublicKey::from_slice(server_pk).unwrap();

    server.set_port(3241);
    cfg.peers.push(::config::Peer::new(Some(server), server_pk));

    Ok(cfg)
}

fn main() -> Result<()> {
    let opt: Pair = ::argh::from_env();
    let config = if ::std::path::Path::new("/etc/entangle.conf").exists() {
        let cfg = ::std::fs::read_to_string("/etc/entangle.conf")?;
        ::toml::de::from_str(&cfg)?
    } else {
        ::config::Config::generate()
    };

    let cfg = if opt.listen {
        ::async_std::task::block_on(accept_client(config))
    } else {
        ::async_std::task::block_on(pair_server(config, opt.server.unwrap()))
    }?;

    use ::std::io::Write;
    let mut cfgf = ::std::fs::File::create("/etc/entangle.conf")?;
    write!(cfgf, "{}", toml::ser::to_string(&cfg)?)?;

    use ::std::os::unix::fs::PermissionsExt;
    let permissions = ::std::fs::Permissions::from_mode(0o600);
    cfgf.set_permissions(permissions)?;
    Ok(())
}
