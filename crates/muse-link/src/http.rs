//! A tiny HTTP/1.1 client used only by [`crate::api`] for the two device-API
//! calls (`GET /fetch_vms`, `POST /device_token/refresh`).
//!
//! It is deliberately minimal — no connection reuse, no redirects — and runs
//! over any async stream, so the TLS layer is injected by the caller and the
//! parser is testable over a plain TCP loopback. It understands
//! `Content-Length` and `Transfer-Encoding: chunked` responses and bounds the
//! response body.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::ApiError;

/// Largest response body accepted from the device API.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// One request header (`name: value`). Values must be header-safe ASCII.
pub struct HttpHeader<'a> {
    /// Header name.
    pub name: &'a str,
    /// Header value.
    pub value: &'a str,
}

/// A parsed response: status code and body bytes.
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: Vec<u8>,
}

fn header_value_ok(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b != b'\r' && b != b'\n' && b != 0 && b.is_ascii())
}

/// Sends a request over `stream` and reads one response.
///
/// `host` goes in the `Host` header; `headers` are added verbatim. A body
/// implies `Content-Length`. The caller supplies any `Authorization` header.
pub async fn request<S>(
    stream: &mut S,
    host: &str,
    method: &str,
    path: &str,
    headers: &[HttpHeader<'_>],
    body: &[u8],
) -> Result<HttpResponse, ApiError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !header_value_ok(host) {
        return Err(ApiError::Request("invalid host".into()));
    }
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for h in headers {
        if !header_value_ok(h.value) || h.name.bytes().any(|b| !b.is_ascii_graphic()) {
            return Err(ApiError::Request(format!("invalid header {}", h.name)));
        }
        req.push_str(h.name);
        req.push_str(": ");
        req.push_str(h.value);
        req.push_str("\r\n");
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");

    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
    }
    stream
        .flush()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;

    read_response(stream).await
}

async fn read_response<S>(stream: &mut S) -> Result<HttpResponse, ApiError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    // Read until the end of the header block (\r\n\r\n).
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(ApiError::BadResponse);
        }
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(ApiError::Transport(
                "connection closed before headers".into(),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = std::str::from_utf8(&buf[..header_end]).map_err(|_| ApiError::BadResponse)?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(parse_status_line)
        .ok_or(ApiError::BadResponse)?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "content-length" {
            content_length = value.parse::<usize>().ok();
        } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }

    let mut rest = buf.split_off(header_end);
    let body = if chunked {
        read_chunked(stream, &mut rest).await?
    } else if let Some(len) = content_length {
        if len > MAX_RESPONSE_BYTES {
            return Err(ApiError::BadResponse);
        }
        read_exact_body(stream, rest, len).await?
    } else {
        read_until_close(stream, rest).await?
    };

    Ok(HttpResponse { status, body })
}

fn parse_status_line(line: &str) -> Option<u16> {
    let mut parts = line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

async fn read_exact_body<S>(
    stream: &mut S,
    mut have: Vec<u8>,
    len: usize,
) -> Result<Vec<u8>, ApiError>
where
    S: AsyncRead + Unpin,
{
    if have.len() > len {
        have.truncate(len);
    }
    while have.len() < len {
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(ApiError::Transport("connection closed mid-body".into()));
        }
        have.extend_from_slice(&chunk[..n]);
        if have.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::BadResponse);
        }
    }
    have.truncate(len);
    Ok(have)
}

async fn read_until_close<S>(stream: &mut S, mut have: Vec<u8>) -> Result<Vec<u8>, ApiError>
where
    S: AsyncRead + Unpin,
{
    loop {
        if have.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::BadResponse);
        }
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        if n == 0 {
            return Ok(have);
        }
        have.extend_from_slice(&chunk[..n]);
    }
}

async fn read_chunked<S>(stream: &mut S, have: &mut Vec<u8>) -> Result<Vec<u8>, ApiError>
where
    S: AsyncRead + Unpin,
{
    let mut out = Vec::new();
    loop {
        // Read a chunk-size line.
        let line = read_line(stream, have).await?;
        let size_hex = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| ApiError::BadResponse)?;
        if size == 0 {
            // Consume the trailing CRLF (and any trailers up to a blank line).
            loop {
                let trailer = read_line(stream, have).await?;
                if trailer.is_empty() {
                    break;
                }
            }
            return Ok(out);
        }
        if out.len() + size > MAX_RESPONSE_BYTES {
            return Err(ApiError::BadResponse);
        }
        while have.len() < size + 2 {
            let mut chunk = [0u8; 4096];
            let n = stream
                .read(&mut chunk)
                .await
                .map_err(|e| ApiError::Transport(e.to_string()))?;
            if n == 0 {
                return Err(ApiError::Transport("connection closed mid-chunk".into()));
            }
            have.extend_from_slice(&chunk[..n]);
        }
        out.extend_from_slice(&have[..size]);
        // Drop the chunk data and its trailing CRLF.
        have.drain(..size + 2);
    }
}

async fn read_line<S>(stream: &mut S, have: &mut Vec<u8>) -> Result<String, ApiError>
where
    S: AsyncRead + Unpin,
{
    loop {
        if let Some(pos) = find_subslice(have, b"\r\n") {
            let line = have[..pos].to_vec();
            have.drain(..pos + 2);
            return String::from_utf8(line).map_err(|_| ApiError::BadResponse);
        }
        if have.len() > MAX_HEADER_BYTES {
            return Err(ApiError::BadResponse);
        }
        let mut chunk = [0u8; 4096];
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(ApiError::Transport("connection closed mid-line".into()));
        }
        have.extend_from_slice(&chunk[..n]);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn serve_once(body: &'static [u8]) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(body).await.unwrap();
            sock.shutdown().await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn parses_content_length_body() {
        let addr = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello").await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let resp = request(&mut s, "x", "GET", "/", &[], b"").await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello");
    }

    #[tokio::test]
    async fn parses_chunked_body() {
        let addr = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        )
        .await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let resp = request(&mut s, "x", "GET", "/", &[], b"").await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello world");
    }

    #[tokio::test]
    async fn parses_close_delimited_body() {
        let addr = serve_once(b"HTTP/1.1 401 Unauthorized\r\n\r\nnope").await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let resp = request(&mut s, "x", "GET", "/", &[], b"").await.unwrap();
        assert_eq!(resp.status, 401);
        assert_eq!(resp.body, b"nope");
    }
}
