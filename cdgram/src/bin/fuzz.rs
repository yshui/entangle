fn pop_packet(data: &[u8]) -> (&[u8], &[u8]) {
    let len = data[0] as usize;
    if data.len() - 1 < len {
        return data[1..].split_at(0);
    }
    return data[1..].split_at(len);
}

fn main() {
    use ::sodiumoxide::crypto::box_::{PublicKey, SecretKey};
    let mut data = Vec::new();
    let mut inputf = ::std::fs::File::open(::std::env::args().nth(1).unwrap()).unwrap();
    use ::std::io::Read;
    let server_pk = PublicKey::from_slice(
        &::base64::decode_config(
            "abpZcuoJca0-vrfoxh3gNFZZi_Q-hdD5j7nl7ZE4MDI",
            ::base64::URL_SAFE_NO_PAD,
        )
        .unwrap(),
    )
    .unwrap();
    let server_sk = SecretKey::from_slice(
        &::base64::decode_config(
            "_iFiwTUJ8Ey0Y3cZuzFwtrfMKtEPbBNitnQfT2ofrC0",
            ::base64::URL_SAFE_NO_PAD,
        )
        .unwrap(),
    )
    .unwrap();
    let client_pk = PublicKey::from_slice(
        &::base64::decode_config(
            "wEQDBea-8xQestYHszI86lN29t9kIiu0vD_CjYEEkhI",
            ::base64::URL_SAFE_NO_PAD,
        )
        .unwrap(),
    )
    .unwrap();
    //let client_pk =
    //&::base64::decode_config("wEQDBea-8xQestYHszI86lN29t9kIiu0vD_CjYEEkhI", ::base64::URL_SAFE_NO_PAD).unwrap();
    //let mut corpus = ::std::fs::File::create("corpus").unwrap();
    //use ::std::io::Write;
    //corpus.write(&[39]).unwrap();
    //corpus.write(&client_pk).unwrap();
    //corpus.write(&[1,2,3,4,5,6,7]).unwrap();
    //panic!();

    inputf.read_to_end(&mut data).unwrap();
    let mut data = data.as_slice();
    use ::cdgram::{
        tests::{random_addr, MockSocket},
        CDGramClient, CDGramServer, Socket,
    };
    let _ = ::env_logger::try_init();
    let (server_addr, client_addr) = (random_addr(), random_addr());
    let (server_sock, mut client_sock) = MockSocket::new(server_addr.clone(), client_addr.clone());
    let mut server = CDGramServer::new(
        server_pk,
        server_sk,
        ::std::iter::once(client_pk.clone()),
        server_sock,
    );
    let recv_handle = ::async_std::task::spawn(async move { server.recv().await.unwrap() });

    ::async_std::task::block_on(async move {
        client_sock.connect(server_addr.clone()).await.unwrap();
        while data.len() > 0 {
            let (pkt, next) = pop_packet(data);
            client_sock.send(pkt).await.unwrap();
            data = next;
        }

        assert!(recv_handle.cancel().await.is_none());
    });
}
