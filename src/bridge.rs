use anyhow::Result;
use tokio::io::{self, AsyncRead, AsyncWrite};

pub async fn bidirectional<A, B>(a: A, b: B) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a, mut b) = (a, b);
    io::copy_bidirectional(&mut a, &mut b).await?;
    Ok(())
}

pub async fn stdin_stdout<S>(stream: S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stdin_stdout_with_io(stream, io::stdin(), io::stdout()).await
}

async fn stdin_stdout_with_io<S, I, O>(stream: S, mut input: I, mut output: O) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = io::split(stream);

    let upload = async {
        let result = io::copy(&mut input, &mut writer).await;
        // Signal EOF to the remote SSH process, but keep the read half alive
        // so commands that continue producing output (for example VS Code's
        // Remote-SSH bootstrap script) can finish normally.
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
        result
    };
    let download = async {
        let result = io::copy(&mut reader, &mut output).await;
        let _ = tokio::io::AsyncWriteExt::flush(&mut output).await;
        result
    };

    tokio::pin!(upload);
    tokio::pin!(download);
    tokio::select! {
        result = &mut upload => {
            result?;
            download.await?;
        }
        result = &mut download => { result?; }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bidirectional, stdin_stdout_with_io};
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn bidirectional_keeps_reading_after_one_side_eof() {
        let (left, mut left_peer) = duplex(1024);
        let (right, mut right_peer) = duplex(1024);
        let bridge = tokio::spawn(bidirectional(left, right));

        left_peer.write_all(b"request").await.unwrap();
        left_peer.shutdown().await.unwrap();

        let mut request = [0_u8; 7];
        timeout(Duration::from_secs(1), right_peer.read_exact(&mut request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&request, b"request");

        right_peer.write_all(b"response").await.unwrap();
        right_peer.shutdown().await.unwrap();

        let mut response = [0_u8; 8];
        timeout(Duration::from_secs(1), left_peer.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"response");
        bridge.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stdin_stdout_keeps_remote_output_after_input_eof() {
        let (stream, mut stream_peer) = duplex(1024);
        let (input, mut input_writer) = duplex(1024);
        let (mut output_reader, output) = duplex(1024);
        let proxy = tokio::spawn(stdin_stdout_with_io(stream, input, output));

        input_writer.write_all(b"script").await.unwrap();
        input_writer.shutdown().await.unwrap();

        let mut script = [0_u8; 6];
        timeout(Duration::from_secs(1), stream_peer.read_exact(&mut script))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&script, b"script");

        stream_peer.write_all(b"port").await.unwrap();
        stream_peer.shutdown().await.unwrap();

        let mut port = [0_u8; 4];
        timeout(Duration::from_secs(1), output_reader.read_exact(&mut port))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&port, b"port");
        proxy.await.unwrap().unwrap();
    }
}
