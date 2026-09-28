use serde::Serialize;
use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};

/// Write one NDJSON frame (JSON + `\n`, flushed).
pub async fn write_frame(
    w: &mut (impl AsyncWrite + Unpin),
    msg: &(impl Serialize + ?Sized),
) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

/// Read one NDJSON frame. Returns `Ok(None)` on clean EOF before any bytes.
pub async fn read_frame(r: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Option<Value>> {
    let mut line = String::new();
    let n = r.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(Some(Value::Null));
    }
    serde_json::from_str(trimmed)
        .map_err(io::Error::other)
        .map(Some)
}

/// Convenience: a buffered NDJSON reader half.
pub fn reader(r: impl AsyncRead + Unpin) -> impl AsyncBufRead + Unpin {
    io::BufReader::new(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut client, mut server) = duplex(1024);
        let msg = json!({"id": 1, "method": "initialize"});
        let mut w = &mut client;
        write_frame(&mut w, &msg).await.unwrap();
        drop(client);
        let mut r = reader(&mut server);
        let got = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(got, msg);
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }
}
