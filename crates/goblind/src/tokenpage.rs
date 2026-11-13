//! A page the handheld serves to the owner's phone, for one thing: giving a profile its
//! SDK token without typing it on a D-pad or pulling the card.
//!
//! The pairing screen shows the page's address as a code to scan. The page links to where
//! tokens are made and takes the token, and that is all. It is open only while the pairing
//! screen waits for it and accepts one token. It is plain HTTP on the local network, with
//! nothing to say who is sending: someone already on that network could read the token as
//! it is sent, or send one of their own while the screen is open.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use goblin_store::profiles::Profile;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// Where tokens are made.
pub const TOKEN_SITE: &str = "https://gadgets.muse.ai/settings/sdk-tokens";
/// The port tried first, so the address is the same each time.
const PORT: u16 = 8674;
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// How the page ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A token was saved to the profile.
    Saved,
    /// The owner closed the pairing screen.
    Cancelled,
    /// Nobody sent a token in time.
    TimedOut,
    /// It was closed for another reason, in words for the owner.
    Failed(String),
}

/// A page ready to be served.
pub struct Page {
    listener: TcpListener,
    /// The address to open on the phone.
    pub url: String,
}

/// The address this device has on the local network, if it is on one.
pub fn local_ip() -> Option<IpAddr> {
    // No packet is sent: connecting a UDP socket only picks the route.
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

impl Page {
    /// Opens the page on `ip`. Nothing is served until [`Page::serve`] runs.
    pub async fn open(ip: IpAddr) -> std::io::Result<Page> {
        let listener = match TcpListener::bind(SocketAddr::new(ip, PORT)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind(SocketAddr::new(ip, 0)).await?,
        };
        let addr = listener.local_addr()?;
        Ok(Page {
            listener,
            url: format!("http://{addr}/"),
        })
    }

    /// Serves the page until a token is saved to `profile`, `cancel` is notified, or
    /// `timeout` passes.
    pub async fn serve(self, profile: Profile, cancel: Arc<Notify>, timeout: Duration) -> Outcome {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            let stream = tokio::select! {
                () = cancel.notified() => return Outcome::Cancelled,
                () = &mut deadline => return Outcome::TimedOut,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => stream,
                    Err(_) => continue,
                },
            };
            // One phone at a time, and never for long.
            let handled =
                tokio::time::timeout(Duration::from_secs(10), handle(stream, &profile)).await;
            if let Ok(Reply::Saved) = handled {
                return Outcome::Saved;
            }
        }
    }
}

enum Reply {
    Saved,
    Other,
}

async fn handle(mut stream: TcpStream, profile: &Profile) -> Reply {
    let Some((method, path, body)) = read_request(&mut stream).await else {
        return Reply::Other;
    };
    let name = escape(profile.name());
    let (status, page, reply) = match (method.as_str(), path.as_str()) {
        ("GET", "/") => ("200 OK", form(&name, ""), Reply::Other),
        ("POST", "/") => {
            let field = |key: &str| {
                body.split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| decode(v))
                    .unwrap_or_default()
            };
            match goblin_store::profiles::SdkToken::parse(field("token").trim()) {
                Ok(token) => match profile.save_sdk_token(&token) {
                    Ok(()) => ("200 OK", done(&name), Reply::Saved),
                    Err(_) => (
                        "200 OK",
                        form(&name, "The handheld could not save it."),
                        Reply::Other,
                    ),
                },
                Err(_) => (
                    "200 OK",
                    form(&name, "That is not an SDK token. Copy it again."),
                    Reply::Other,
                ),
            }
        }
        _ => ("404 Not Found", String::new(), Reply::Other),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    reply
}

/// Reads one request: its method, its path, and its body as text.
async fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let (head_end, length) = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 || buf.len() + n > MAX_REQUEST_BYTES {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..at]).into_owned();
            let length = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (at + 4, length);
        }
    };
    if head_end + length > MAX_REQUEST_BYTES {
        return None;
    }
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut first = head.lines().next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.split('?').next()?.to_owned();
    let body = String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned();
    Some((method, path, body))
}

/// Decodes a form value (`+` and `%XX`).
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = "body{font:17px system-ui,sans-serif;margin:0;padding:24px;max-width:28em;\
margin:auto;background:#f2f3f5;color:#111}h1{font-size:22px}a.b,button{display:block;\
box-sizing:border-box;width:100%;padding:14px;margin:12px 0;border:0;border-radius:12px;\
background:#0a63d6;color:#fff;font:inherit;text-align:center;text-decoration:none}\
input{box-sizing:border-box;width:100%;padding:12px;margin:8px 0;border:1px solid #bbb;\
border-radius:10px;font:inherit}p.e{color:#b3261e}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}input{background:#222;\
color:#eee;border-color:#555}}";

fn shell(body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Goblin Link</title><style>{STYLE}</style>{body}</html>"
    )
}

fn form(profile: &str, error: &str) -> String {
    let error = if error.is_empty() {
        String::new()
    } else {
        format!("<p class=\"e\">{}</p>", escape(error))
    };
    shell(&format!(
        "<h1>Muse SDK token for \u{201c}{profile}\u{201d}</h1>{error}\
         <a class=\"b\" href=\"{TOKEN_SITE}\" target=\"_blank\" rel=\"noopener\">\
         1. Get a token</a>\
         <form method=\"post\" action=\"/\">\
         <input name=\"token\" placeholder=\"2. Paste the token\" autocomplete=\"off\" \
         autocapitalize=\"off\" spellcheck=\"false\" required>\
         <button>Send to handheld</button></form>"
    ))
}

fn done(profile: &str) -> String {
    shell(&format!(
        "<h1>Sent</h1><p>\u{201c}{profile}\u{201d} has its token. Carry on in the Muse app.</p>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use goblin_store::{Layout, ProfileStore};

    const TOKEN: &str = "mgst_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPA";

    async fn request(url: &str, text: &str) -> String {
        let addr = url.trim_start_matches("http://").trim_end_matches('/');
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(text.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    fn post(body: &str) -> String {
        format!(
            "POST / HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn profile(dir: &std::path::Path) -> Profile {
        let store = ProfileStore::open(Layout::new(dir.join("data"))).unwrap();
        store.create("work").unwrap()
    }

    #[tokio::test]
    async fn the_page_takes_one_token() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served =
            tokio::spawn(page.serve(p.clone(), Arc::new(Notify::new()), Duration::from_secs(30)));

        let html = request(&url, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(
            html.contains(TOKEN_SITE) && html.contains("target=\"_blank\""),
            "{html}"
        );
        assert!(html.contains("work"));

        let html = request(&url, &post("token=hello")).await;
        assert!(html.contains("not an SDK token"), "{html}");
        assert!(p.sdk_token().unwrap().is_none());

        // Pasted with a stray space and a newline, as phones do.
        let html = request(&url, &post(&format!("token=+{TOKEN}%0A"))).await;
        assert!(html.contains("Sent"), "{html}");
        assert_eq!(served.await.unwrap(), Outcome::Saved);
        assert!(p.sdk_token().unwrap().is_some());
    }

    #[tokio::test]
    async fn the_page_closes_on_cancel_and_time() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        let cancel = Arc::new(Notify::new());
        let page = Page::open(ip).await.unwrap();
        let served = tokio::spawn(page.serve(p.clone(), cancel.clone(), Duration::from_secs(30)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.notify_one();
        assert_eq!(served.await.unwrap(), Outcome::Cancelled);

        let page = Page::open(ip).await.unwrap();
        let out = page
            .serve(p, Arc::new(Notify::new()), Duration::from_millis(50))
            .await;
        assert_eq!(out, Outcome::TimedOut);
    }

    #[test]
    fn form_values_decode() {
        assert_eq!(decode("a+b%20c%2Fd"), "a b c/d");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
}
