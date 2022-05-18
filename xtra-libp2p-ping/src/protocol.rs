//! The actual protocol functions for sending ping messages.
//!
//! Insbired by https://github.com/libp2p/rust-libp2p/blob/102509afe3a3b984e43a88dbe4de935fde36f319/protocols/ping/src/protocol.rs#L82-L113.

pub const SIZE: usize = 32;

use futures::AsyncReadExt;
use futures::AsyncWriteExt;
use rand::distributions;
use rand::thread_rng;
use rand::Rng;
use std::io;
use std::time::Duration;
use std::time::Instant;
use xtra_libp2p::Substream;

/// Sends a ping and waits for the pong.
pub async fn send(mut stream: Substream) -> io::Result<Duration> {
    let payload: [u8; SIZE] = thread_rng().sample(distributions::Standard);
    stream.write_all(&payload).await?;
    stream.flush().await?;

    let started = Instant::now();

    let mut recv_payload = [0u8; SIZE];
    stream.read_exact(&mut recv_payload).await?;

    if recv_payload != payload {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Ping payload mismatch",
        ));
    }

    Ok(started.elapsed())
}

/// Waits for a ping and sends a pong.
pub async fn recv(mut stream: Substream) -> io::Result<()> {
    let mut payload = [0u8; SIZE];
    stream.read_exact(&mut payload).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    Ok(())
}
