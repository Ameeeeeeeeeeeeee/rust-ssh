use anyhow::Result;
use tokio::io::{self, AsyncRead, AsyncWrite};

pub async fn bidirectional<A, B>(a: A, b: B) -> Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_reader, mut a_writer) = io::split(a);
    let (mut b_reader, mut b_writer) = io::split(b);

    tokio::select! {
        result = io::copy(&mut a_reader, &mut b_writer) => {
            result?;
        }
        result = io::copy(&mut b_reader, &mut a_writer) => {
            result?;
        }
    }

    Ok(())
}

pub async fn stdin_stdout<S>(stream: S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = io::split(stream);
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let upload = async {
        let result = io::copy(&mut stdin, &mut writer).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
        result
    };
    let download = async {
        let result = io::copy(&mut reader, &mut stdout).await;
        let _ = tokio::io::AsyncWriteExt::flush(&mut stdout).await;
        result
    };

    tokio::select! {
        result = upload => { result?; }
        result = download => { result?; }
    }

    Ok(())
}
