use anyhow::Result;
use snow::{Builder, TransportState};
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// todo: investigate what these params do and whether we can use get authentication for free using
// them
static PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

pub async fn initiator_handshake(connection: &mut TcpStream) -> Result<TransportState> {
    let builder: Builder<'_> = Builder::new(PARAMS.parse()?);
    let static_key = builder.generate_keypair()?.private;

    let mut noise = builder.local_private_key(&static_key).build_initiator()?;

    let mut buf = vec![0u8; 65535];

    let len = noise.write_message(&[], &mut buf)?;
    send(connection, &buf[..len]).await?;

    noise.read_message(&recv(connection).await?, &mut buf)?;

    let len = noise.write_message(&[], &mut buf)?;
    send(connection, &buf[..len]).await?;

    let noise = noise.into_transport_mode()?;

    tracing::debug!("Noise protocol initiator handshake is complete");

    Ok(noise)
}

pub async fn responder_handshake(connection: &mut TcpStream) -> Result<TransportState> {
    let builder: Builder<'_> = Builder::new(PARAMS.parse()?);
    let static_key = builder.generate_keypair()?.private;

    let mut noise = builder.local_private_key(&static_key).build_responder()?;

    let mut buf = vec![0u8; 65535];

    noise.read_message(&recv(connection).await?, &mut buf)?;

    let len = noise.write_message(&[0u8; 0], &mut buf)?;
    send(connection, &buf[..len]).await?;

    noise.read_message(&recv(connection).await?, &mut buf)?;

    let noise = noise.into_transport_mode()?;

    tracing::debug!("Noise protocol responder handshake is complete");

    Ok(noise)
}

/// Hyper-basic stream transport receiver. 16-bit BE size followed by payload.
async fn recv(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut msg_len_buf = [0u8; 2];
    stream.read_exact(&mut msg_len_buf).await?;
    let msg_len = ((msg_len_buf[0] as usize) << 8) + (msg_len_buf[1] as usize);
    let mut msg = vec![0u8; msg_len];
    stream.read_exact(&mut msg[..]).await?;
    Ok(msg)
}

/// Hyper-basic stream transport sender. 16-bit BE size followed by payload.
async fn send(stream: &mut TcpStream, buf: &[u8]) -> Result<()> {
    let msg_len_buf = [(buf.len() >> 8) as u8, (buf.len() & 0xff) as u8];
    stream.write_all(&msg_len_buf).await?;
    stream.write_all(buf).await?;
    Ok(())
}
