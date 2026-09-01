use crate::identity::{self, ServerIdentity};
use anyhow::{anyhow, Context, Result};
use snow::{Builder, TransportState};
use std::io;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

const MAX_HANDSHAKE_MESSAGE: u32 = 65_535;
const NOISE_TAG_SIZE: usize = 16;
const MAX_PLAINTEXT_CHUNK: usize = 32 * 1024;
const MAX_CIPHERTEXT_FRAME: usize = MAX_PLAINTEXT_CHUNK + NOISE_TAG_SIZE;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub type RelayStream = NoiseStream<TcpStream>;

pub async fn client_connect(endpoint: &str, expected_public_key: &[u8]) -> Result<RelayStream> {
    let stream = timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(endpoint))
        .await
        .map_err(|_| anyhow!("timed out connecting to relay {endpoint}"))?
        .with_context(|| format!("connecting to relay {endpoint}"))?;
    stream
        .set_nodelay(true)
        .with_context(|| format!("enabling TCP_NODELAY for relay {endpoint}"))?;

    timeout(
        HANDSHAKE_TIMEOUT,
        client_handshake(stream, expected_public_key),
    )
    .await
    .map_err(|_| anyhow!("timed out during Noise handshake with relay {endpoint}"))?
}

pub async fn client_handshake<S>(
    mut stream: S,
    expected_public_key: &[u8],
) -> Result<NoiseStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if expected_public_key.len() != crate::identity::STATIC_KEY_SIZE {
        return Err(anyhow!(
            "pinned server public key must be exactly {} bytes",
            identity::STATIC_KEY_SIZE
        ));
    }

    let builder = Builder::new(crate::identity::noise_params());
    let client_key = builder
        .generate_keypair()
        .map_err(|error| anyhow!("generating client Noise session key: {error}"))?;
    let mut handshake = builder
        .local_private_key(&client_key.private)
        .build_initiator()
        .map_err(|error| anyhow!("creating Noise initiator: {error}"))?;
    let mut output = vec![0_u8; MAX_HANDSHAKE_MESSAGE as usize];
    let mut payload = vec![0_u8; MAX_HANDSHAKE_MESSAGE as usize];

    let length = handshake
        .write_message(&[], &mut output)
        .map_err(|error| anyhow!("writing Noise handshake message 1: {error}"))?;
    write_noise_message(&mut stream, &output[..length]).await?;

    let input = read_noise_message(&mut stream).await?;
    handshake
        .read_message(&input, &mut payload)
        .map_err(|error| anyhow!("reading Noise handshake message 2: {error}"))?;
    let remote_static = handshake
        .get_remote_static()
        .ok_or_else(|| anyhow!("relay did not provide a static Noise identity key"))?;
    if remote_static != expected_public_key {
        return Err(anyhow!(
            "relay server Noise public key does not match the pinned key"
        ));
    }

    let length = handshake
        .write_message(&[], &mut output)
        .map_err(|error| anyhow!("writing Noise handshake message 3: {error}"))?;
    write_noise_message(&mut stream, &output[..length]).await?;

    let transport = handshake
        .into_transport_mode()
        .map_err(|error| anyhow!("entering Noise transport mode: {error}"))?;
    Ok(NoiseStream::new(stream, transport))
}

pub async fn server_handshake<S>(mut stream: S, identity: &ServerIdentity) -> Result<NoiseStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let builder = Builder::new(crate::identity::noise_params());
    let mut handshake = builder
        .local_private_key(identity.private_key())
        .build_responder()
        .map_err(|error| anyhow!("creating Noise responder: {error}"))?;
    let mut output = vec![0_u8; MAX_HANDSHAKE_MESSAGE as usize];
    let mut payload = vec![0_u8; MAX_HANDSHAKE_MESSAGE as usize];

    let input = read_noise_message(&mut stream).await?;
    handshake
        .read_message(&input, &mut payload)
        .map_err(|error| anyhow!("reading Noise handshake message 1: {error}"))?;

    let length = handshake
        .write_message(&[], &mut output)
        .map_err(|error| anyhow!("writing Noise handshake message 2: {error}"))?;
    write_noise_message(&mut stream, &output[..length]).await?;

    let input = read_noise_message(&mut stream).await?;
    handshake
        .read_message(&input, &mut payload)
        .map_err(|error| anyhow!("reading Noise handshake message 3: {error}"))?;

    let transport = handshake
        .into_transport_mode()
        .map_err(|error| anyhow!("entering Noise transport mode: {error}"))?;
    Ok(NoiseStream::new(stream, transport))
}

async fn read_noise_message<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let length = reader
        .read_u32()
        .await
        .context("reading Noise handshake frame length")?;
    if length == 0 || length > MAX_HANDSHAKE_MESSAGE {
        return Err(anyhow!("invalid Noise handshake frame length: {length}"));
    }

    let mut message = vec![0_u8; length as usize];
    reader
        .read_exact(&mut message)
        .await
        .context("reading Noise handshake frame")?;
    Ok(message)
}

async fn write_noise_message<W>(writer: &mut W, message: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    if message.is_empty() || message.len() > MAX_HANDSHAKE_MESSAGE as usize {
        return Err(anyhow!("invalid Noise handshake message length"));
    }
    writer
        .write_u32(message.len() as u32)
        .await
        .context("writing Noise handshake frame length")?;
    writer
        .write_all(message)
        .await
        .context("writing Noise handshake frame")?;
    writer
        .flush()
        .await
        .context("flushing Noise handshake frame")
}

pub struct NoiseStream<S> {
    inner: S,
    transport: TransportState,
    read_header: [u8; 4],
    read_header_filled: usize,
    read_body: Vec<u8>,
    read_body_filled: usize,
    plaintext: Vec<u8>,
    plaintext_offset: usize,
    pending_write: Option<PendingWrite>,
}

struct PendingWrite {
    wire: Vec<u8>,
    offset: usize,
    plaintext_len: usize,
}

impl<S> NoiseStream<S> {
    fn new(inner: S, transport: TransportState) -> Self {
        Self {
            inner,
            transport,
            read_header: [0_u8; 4],
            read_header_filled: 0,
            read_body: Vec::new(),
            read_body_filled: 0,
            plaintext: Vec::new(),
            plaintext_offset: 0,
            pending_write: None,
        }
    }
}

impl<S> NoiseStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_pending_write(
        this: &mut Self,
        cx: &mut TaskContext<'_>,
    ) -> Poll<io::Result<Option<usize>>> {
        loop {
            let Some(pending) = this.pending_write.as_ref() else {
                return Poll::Ready(Ok(None));
            };
            if pending.offset == pending.wire.len() {
                let plaintext_len = pending.plaintext_len;
                this.pending_write = None;
                return Poll::Ready(Ok(Some(plaintext_len)));
            }

            let offset = pending.offset;
            let poll = {
                let pending = this.pending_write.as_ref().expect("pending write exists");
                Pin::new(&mut this.inner).poll_write(cx, &pending.wire[offset..])
            };
            match poll {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "relay stream returned zero bytes while writing",
                    )))
                }
                Poll::Ready(Ok(written)) => {
                    let pending = this.pending_write.as_mut().expect("pending write exists");
                    if written > pending.wire.len() - pending.offset {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "relay stream reported an invalid write length",
                        )));
                    }
                    pending.offset += written;
                }
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            }
        }
    }

    fn noise_io_error(error: snow::Error) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Noise transport error: {error}"),
        )
    }
}

impl<S> AsyncRead for NoiseStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();

        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        loop {
            if this.plaintext_offset < this.plaintext.len() {
                let available = &this.plaintext[this.plaintext_offset..];
                let count = available.len().min(buffer.remaining());
                buffer.put_slice(&available[..count]);
                this.plaintext_offset += count;
                if this.plaintext_offset == this.plaintext.len() {
                    this.plaintext.clear();
                    this.plaintext_offset = 0;
                }
                return Poll::Ready(Ok(()));
            }

            this.plaintext.clear();
            this.plaintext_offset = 0;

            while this.read_header_filled < this.read_header.len() {
                let start = this.read_header_filled;
                let (poll, count) = {
                    let mut read_buffer = ReadBuf::new(&mut this.read_header[start..]);
                    let before = read_buffer.filled().len();
                    let poll = Pin::new(&mut this.inner).poll_read(cx, &mut read_buffer);
                    (poll, read_buffer.filled().len() - before)
                };
                match poll {
                    Poll::Pending => {
                        this.read_header_filled += count;
                        return Poll::Pending;
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Ready(Ok(())) => {
                        if count == 0 {
                            if this.read_header_filled == 0 {
                                return Poll::Ready(Ok(()));
                            }
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "relay stream ended inside a Noise frame header",
                            )));
                        }
                        this.read_header_filled += count;
                    }
                }
            }

            let length = u32::from_be_bytes(this.read_header) as usize;
            if length == 0 || length > MAX_CIPHERTEXT_FRAME {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid encrypted relay frame length: {length}"),
                )));
            }
            this.read_header_filled = 0;
            this.read_body.resize(length, 0);
            this.read_body_filled = 0;

            while this.read_body_filled < this.read_body.len() {
                let start = this.read_body_filled;
                let (poll, count) = {
                    let mut read_buffer = ReadBuf::new(&mut this.read_body[start..]);
                    let before = read_buffer.filled().len();
                    let poll = Pin::new(&mut this.inner).poll_read(cx, &mut read_buffer);
                    (poll, read_buffer.filled().len() - before)
                };
                match poll {
                    Poll::Pending => {
                        this.read_body_filled += count;
                        return Poll::Pending;
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Ready(Ok(())) => {
                        if count == 0 {
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "relay stream ended inside an encrypted frame",
                            )));
                        }
                        this.read_body_filled += count;
                    }
                }
            }

            let ciphertext = std::mem::take(&mut this.read_body);
            this.read_body_filled = 0;
            let mut plaintext = vec![0_u8; ciphertext.len()];
            let length = match this.transport.read_message(&ciphertext, &mut plaintext) {
                Ok(length) => length,
                Err(error) => return Poll::Ready(Err(Self::noise_io_error(error))),
            };
            plaintext.truncate(length);
            this.plaintext = plaintext;
        }
    }
}

impl<S> AsyncWrite for NoiseStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.as_mut().get_mut();
        match Self::poll_pending_write(this, cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(Some(count))) => return Poll::Ready(Ok(count)),
            Poll::Ready(Ok(None)) => {}
        }

        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let plaintext_len = buffer.len().min(MAX_PLAINTEXT_CHUNK);
        let mut ciphertext = vec![0_u8; plaintext_len + NOISE_TAG_SIZE];
        let ciphertext_len = match this
            .transport
            .write_message(&buffer[..plaintext_len], &mut ciphertext)
        {
            Ok(length) => length,
            Err(error) => return Poll::Ready(Err(Self::noise_io_error(error))),
        };
        ciphertext.truncate(ciphertext_len);

        let mut wire = Vec::with_capacity(4 + ciphertext_len);
        wire.extend_from_slice(&(ciphertext_len as u32).to_be_bytes());
        wire.extend_from_slice(&ciphertext);
        this.pending_write = Some(PendingWrite {
            wire,
            offset: 0,
            plaintext_len,
        });

        match Self::poll_pending_write(this, cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Some(count))) => Poll::Ready(Ok(count)),
            Poll::Ready(Ok(None)) => unreachable!("new pending write must be present"),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        match Self::poll_pending_write(this, cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(Some(_))) => self.poll_flush(cx),
            Poll::Ready(Ok(None)) => Pin::new(&mut this.inner).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        match Self::poll_pending_write(this, cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(Some(_))) => self.poll_shutdown(cx),
            Poll::Ready(Ok(None)) => Pin::new(&mut this.inner).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::STATIC_KEY_SIZE;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn handshake_pins_server_key_and_encrypts_transport() {
        let keypair = Builder::new(identity::noise_params())
            .generate_keypair()
            .unwrap();
        let expected_public = keypair.public.clone();
        let server_identity = ServerIdentity::from_private(keypair.private).unwrap();
        let (client_side, server_side) = duplex(128 * 1024);

        let (client_result, server_result) = tokio::join!(
            client_handshake(client_side, &expected_public),
            server_handshake(server_side, &server_identity)
        );
        let mut client = client_result.unwrap();
        let mut server = server_result.unwrap();

        let message = vec![0x5a_u8; MAX_PLAINTEXT_CHUNK + 17];
        client.write_all(&message).await.unwrap();
        let mut received = vec![0_u8; message.len()];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(received, message);

        server.write_all(b"ok").await.unwrap();
        let mut response = [0_u8; 2];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"ok");
        assert_eq!(expected_public.len(), STATIC_KEY_SIZE);
    }

    #[tokio::test]
    async fn handshake_rejects_wrong_pinned_key() {
        let keypair = Builder::new(identity::noise_params())
            .generate_keypair()
            .unwrap();
        let server_identity = ServerIdentity::from_private(keypair.private).unwrap();
        let wrong_public = [0_u8; STATIC_KEY_SIZE];
        let (client_side, server_side) = duplex(16 * 1024);

        let (client_result, server_result) = tokio::join!(
            client_handshake(client_side, &wrong_public),
            server_handshake(server_side, &server_identity)
        );
        assert!(client_result.is_err());
        assert!(server_result.is_err());
    }
}
