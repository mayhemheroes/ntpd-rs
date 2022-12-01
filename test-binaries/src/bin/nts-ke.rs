use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::Arc,
};

use aes_siv::{
    aead::{Aead, KeyInit, Payload},
    Aes256SivAead,
    Key, // Or `Aes128SivAead`
    Nonce,
};

use ntp_proto::{ExtensionField, NtsRecord};
use ntp_udp::UdpSocket;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls;

fn key_exchange_client() -> Result<tokio_rustls::TlsConnector, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().expect("could not load platform certs") {
        roots.add(&rustls::Certificate(cert.0)).unwrap();
    }

    let mut config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols.push(b"ntske/1".to_vec());

    let rc_config = Arc::new(config);

    Ok(tokio_rustls::TlsConnector::from(rc_config))
}

// unstable in std; check on https://github.com/rust-lang/rust/issues/88581 some time in the future
pub const fn next_multiple_of(lhs: usize, rhs: usize) -> usize {
    match lhs % rhs {
        0 => lhs,
        r => lhs + (rhs - r),
    }
}

// unstable in std; check on https://github.com/rust-lang/rust/issues/88581 some time in the future
pub const fn div_ceil(lhs: usize, rhs: usize) -> usize {
    let d = lhs / rhs;
    let r = lhs % rhs;
    if r > 0 && rhs > 0 {
        d + 1
    } else {
        d
    }
}

fn key_exchange_packet(
    identifier: &[u8],
    cookie: &[u8],
    cipher: Aes256SivAead,
    nonce: &Nonce,
) -> Vec<u8> {
    let mut packet = vec![
        0b00100011, 0, 10, 0, //hdr
        0, 0, 0, 0, // root delay
        0, 0, 0, 0, // root dispersion
        0, 0, 0, 0, // refid
        0, 0, 0, 0, 0, 0, 0, 0, // ref timestamp
        0, 0, 0, 0, 0, 0, 0, 0, // org timestamp
        0, 0, 0, 0, 0, 0, 0, 0, // recv timestamp
        1, 2, 3, 4, 5, 6, 7, 8, // xmt timestamp
    ];

    let unique_identifier = ExtensionField::UniqueIdentifier(identifier.into());
    unique_identifier.serialize(&mut packet).unwrap();

    let cookie = ExtensionField::NtsCookie(cookie.into());
    cookie.serialize(&mut packet).unwrap();

    let payload = Payload {
        msg: b"",
        aad: &packet,
    };

    let ciphertext = cipher.encrypt(nonce, payload).unwrap();

    let signature = ExtensionField::NtsEncryptedField {
        nonce: nonce.as_slice().into(),
        ciphertext: ciphertext.into(),
    };
    signature.serialize(&mut packet).unwrap();

    packet
}

fn key_exchange_records() -> [NtsRecord; 3] {
    [
        NtsRecord::NextProtocol {
            protocol_ids: vec![0],
        },
        NtsRecord::AeadAlgorithm {
            critical: false,
            algorithm_ids: vec![15],
        },
        NtsRecord::EndOfMessage,
    ]
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let domain = "time.cloudflare.com";
    let config = key_exchange_client().unwrap();

    let addr = (domain, 4460)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut stream = config
        .connect(domain.try_into().unwrap(), stream)
        .await
        .unwrap();

    let records = key_exchange_records();

    let mut buffer = Vec::with_capacity(1024);
    for record in records {
        buffer.clear();
        record.write(&mut buffer)?;
        stream.write_all(&buffer).await?;
    }

    let mut remote = domain.to_string();
    let mut port = 123;
    let mut cookie = None;

    let mut buffer = [0; 1024];
    let mut decoder = ntp_proto::NtsRecord::decoder();

    'outer: loop {
        let n = stream.read(&mut buffer).await.unwrap();
        decoder.extend(buffer[..n].iter().copied());

        while let Some(record) = decoder.next().unwrap() {
            match record {
                NtsRecord::EndOfMessage => break 'outer,
                NtsRecord::NewCookie { cookie_data } => cookie = Some(cookie_data),
                NtsRecord::Server { name, .. } => remote = name.to_string(),
                NtsRecord::Port { port: p, .. } => port = p,
                _ => { /* ignore */ }
            }
        }
    }

    let cookie = match cookie {
        Some(cookie) => cookie,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "did not receive cookie",
            ))
        }
    };

    println!("cookie: {:?}", &cookie);

    let mut c2s = [0; 64];
    let mut s2c = [0; 64];
    let label = b"EXPORTER-network-time-security";

    stream
        .get_ref()
        .1
        .export_keying_material(&mut c2s, label, Some(&[0, 0, 0, 15, 0]))
        .unwrap();
    stream
        .get_ref()
        .1
        .export_keying_material(&mut s2c, label, Some(&[0, 0, 0, 15, 1]))
        .unwrap();

    let addr = (remote, port).to_socket_addrs().unwrap().next().unwrap();

    let mut socket = match addr {
        SocketAddr::V4(_) => UdpSocket::client((Ipv4Addr::UNSPECIFIED, 0).into(), addr).await?,
        SocketAddr::V6(_) => UdpSocket::client((Ipv6Addr::UNSPECIFIED, 0).into(), addr).await?,
    };

    let identifier: Vec<u8> = (0..).take(32).collect();
    let cipher = Aes256SivAead::new(Key::<Aes256SivAead>::from_slice(c2s.as_slice()));
    let nonce = Nonce::from_slice(b"any unique nonce");
    let packet = key_exchange_packet(&identifier, &cookie, cipher, nonce);

    socket.send(&packet).await?;
    let mut buf = [0; 1024];
    let (n, _remote, _timestamp) = socket.recv(&mut buf).await?;
    println!("response ({n} bytes): {:?}", &buf[0..n]);

    Ok(())
}