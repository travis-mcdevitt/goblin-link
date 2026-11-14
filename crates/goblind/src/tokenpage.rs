//! A page the handheld serves to the owner's phone, for one thing: giving a profile its
//! SDK token without typing it on a D-pad or pulling the card.
//!
//! The pairing screen shows the page's address as a code to scan. The page links to where
//! tokens are made and takes the token, and that is all. It is open only while the pairing
//! screen waits for it and accepts one token. It is plain HTTP on the local network, with
//! nothing to say who is sending: someone already on that network could read the token as
//! it is sent, or send one of their own while the screen is open.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
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

/// No usable LAN connection for the phone setup page.
pub const NO_NETWORK: &str = "Connect to Wi-Fi, then try again.";
/// The LAN exists, but its setup page could not be opened.
pub const PAGE_UNAVAILABLE: &str = "Could not open the setup page. Try again.";

/// Prefer the route's address only when it belongs to a connected LAN interface.
/// A VPN route must not put an unreachable VPN address in the phone's QR code.
pub fn local_ip() -> Option<IpAddr> {
    let route = UdpSocket::bind("0.0.0.0:0").ok().and_then(|socket| {
        socket.connect("192.0.2.1:9").ok()?;
        socket.local_addr().ok().map(|a| a.ip())
    });
    let mut addresses = Vec::new();
    let mut head = std::ptr::null_mut();
    // getifaddrs owns this linked list until freeifaddrs; all pointers are checked
    // before reading their address family or casting to an IPv4 sockaddr.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut entry = head;
        while let Some(iface) = entry.as_ref() {
            if !iface.ifa_name.is_null()
                && !iface.ifa_addr.is_null()
                && (*iface.ifa_addr).sa_family as i32 == libc::AF_INET
            {
                let name = std::ffi::CStr::from_ptr(iface.ifa_name).to_string_lossy();
                let path = std::path::Path::new("/sys/class/net").join(name.as_ref());
                let physical = path.join("device").exists()
                    || path.join("wireless").exists()
                    || std::fs::read_dir(path.join("brif"))
                        .into_iter()
                        .flatten()
                        .flatten()
                        .any(|port| port.path().join("device").exists());
                let addr = &*(iface.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                if usable_address(physical, iface.ifa_flags, ip) {
                    addresses.push(IpAddr::V4(ip));
                }
            }
            entry = iface.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    select_address(&addresses, route)
}

fn usable_address(physical: bool, flags: u32, ip: Ipv4Addr) -> bool {
    physical
        && flags & (libc::IFF_UP | libc::IFF_RUNNING) as u32
            == (libc::IFF_UP | libc::IFF_RUNNING) as u32
        && flags & libc::IFF_LOOPBACK as u32 == 0
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_link_local()
}

fn select_address(addresses: &[IpAddr], route: Option<IpAddr>) -> Option<IpAddr> {
    route
        .filter(|ip| addresses.contains(ip))
        .or_else(|| addresses.first().copied())
}

impl Page {
    /// Opens the phone page, keeping network and listener failures distinct.
    pub async fn for_network(ip: Option<IpAddr>) -> Result<Self, &'static str> {
        let ip = ip.ok_or(NO_NETWORK)?;
        Self::open(ip).await.map_err(|_| PAGE_UNAVAILABLE)
    }

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
            let handled = tokio::select! {
                biased;
                () = cancel.notified() => return Outcome::Cancelled,
                () = &mut deadline => return Outcome::TimedOut,
                result = tokio::time::timeout(Duration::from_secs(10), handle(stream, &profile)) => result,
            };
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
    if length > MAX_REQUEST_BYTES - head_end {
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

fn steps(saved: bool) -> String {
    let current = if saved { 3 } else { 2 };
    let labels = [
        "Get an SDK token",
        "Save it on the handheld",
        "Pair in Muse",
    ];
    let mut html = String::from("<ol aria-label=\"Pairing steps\">");
    for (i, label) in labels.iter().enumerate() {
        if i + 1 == current {
            html.push_str(&format!(
                "<li aria-current=\"step\"><strong>{label}</strong></li>"
            ));
        } else {
            html.push_str(&format!("<li>{label}</li>"));
        }
    }
    html.push_str("</ol>");
    html
}

const MUSE_STEP: &str = "<p>Open <strong>Muse → Settings → Devices</strong>. Enable \
    <strong>Developer mode</strong>, tap <strong>+</strong>, then choose the device shown \
    on your handheld.</p><p>Turn on your phone’s Bluetooth. Keep pairing open on the handheld. \
    If Muse asks for Wi-Fi, choose the handheld’s current network.</p>";

fn form(profile: &str, error: &str) -> String {
    let error = if error.is_empty() {
        String::new()
    } else {
        format!("<p class=\"e\" role=\"alert\">{}</p>", escape(error))
    };
    shell(&format!(
        "<h1>Pair your handheld</h1><p>Profile: {profile}</p>{steps}{error}\
         <a class=\"b\" href=\"{TOKEN_SITE}\" target=\"_blank\" rel=\"noopener\">\
         Get or manage SDK tokens</a>\
         <p>Copy your token from Muse Gadgets → SDK tokens, then return here. \
         Reuse an existing token for this Muse account. Replacing it can disconnect other devices.</p>\
         <form method=\"post\" action=\"/\">\
         <label for=\"token\">SDK token</label>\
         <input id=\"token\" name=\"token\" placeholder=\"Paste your SDK token\" autocomplete=\"off\" \
         autocapitalize=\"off\" spellcheck=\"false\" required>\
         <button>Save token</button></form><h2>Next: pair in Muse</h2>{MUSE_STEP}",
         steps = steps(false)
    ))
}

fn done(profile: &str) -> String {
    shell(&format!(
        "<h1>Token saved</h1><p>Profile: {profile}</p>{steps}\
         <p>Finish pairing in Muse.</p>{MUSE_STEP}",
        steps = steps(true)
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
    async fn oversized_length_leaves_the_page_serving() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served = tokio::spawn(page.serve(p, Arc::new(Notify::new()), Duration::from_secs(30)));
        let reply = request(
            &url,
            "POST / HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n",
        )
        .await;
        assert!(reply.is_empty());
        assert!(!served.is_finished(), "bad request must not stop the page");
        let reply = request(&url, &post(&format!("token={TOKEN}"))).await;
        assert!(reply.contains("Token saved"));
        assert_eq!(served.await.unwrap(), Outcome::Saved);
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
        assert!(html.contains("<li aria-current=\"step\"><strong>Save it on the handheld"));
        assert!(html.contains("Developer mode"));

        let html = request(&url, &post("token=hello")).await;
        assert!(html.contains("not an SDK token"), "{html}");
        assert!(p.sdk_token().unwrap().is_none());
        assert!(html.contains("<li aria-current=\"step\"><strong>Save it on the handheld"));

        // Pasted with a stray space and a newline, as phones do.
        let html = request(&url, &post(&format!("token=+{TOKEN}%0A"))).await;
        assert!(html.contains("Token saved"), "{html}");
        assert!(html.contains("<li aria-current=\"step\"><strong>Pair in Muse"));
        assert!(html.contains("Finish pairing in Muse"));
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
    fn phone_address_requires_a_connected_lan_not_a_vpn_or_interface_name() {
        let up = (libc::IFF_UP | libc::IFF_RUNNING) as u32;
        let wifi = "192.168.1.12".parse::<Ipv4Addr>().unwrap();
        let wired = "10.0.0.4".parse::<IpAddr>().unwrap();
        let vpn = "100.64.0.2".parse::<IpAddr>().unwrap();
        assert!(usable_address(true, up, wifi));
        assert!(!usable_address(false, up, wifi));
        assert!(!usable_address(true, libc::IFF_UP as u32, wifi));
        assert!(!usable_address(true, up, Ipv4Addr::LOCALHOST));
        assert!(!usable_address(true, up, Ipv4Addr::UNSPECIFIED));
        assert!(!usable_address(true, up, Ipv4Addr::new(169, 254, 1, 1)));
        assert_eq!(
            select_address(&[wifi.into(), wired], Some(vpn)),
            Some(wifi.into())
        );
        assert_eq!(
            select_address(&[wifi.into(), wired], Some(wired)),
            Some(wired)
        );
        assert_eq!(select_address(&[], Some(vpn)), None);
    }

    #[tokio::test]
    async fn page_errors_distinguish_missing_network_from_listener_failure() {
        assert!(matches!(Page::for_network(None).await, Err(NO_NETWORK)));
        // An address not assigned to this container must not become an SD-card error.
        assert!(matches!(
            Page::for_network(Some("192.0.2.55".parse().unwrap())).await,
            Err(PAGE_UNAVAILABLE)
        ));
        let page = Page::for_network(Some("127.0.0.1".parse().unwrap()))
            .await
            .unwrap();
        assert!(page.url.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn form_values_decode() {
        assert_eq!(decode("a+b%20c%2Fd"), "a b c/d");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
    }
    #[tokio::test]
    async fn cancellation_interrupts_an_incomplete_phone_request() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile(dir.path());
        let cancel = Arc::new(Notify::new());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let addr = page.listener.local_addr().unwrap();
        let served =
            tokio::spawn(page.serve(profile.clone(), cancel.clone(), Duration::from_secs(30)));
        let mut phone = TcpStream::connect(addr).await.unwrap();
        phone
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 100\r\n\r\ntoken=")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(300), served)
                .await
                .unwrap()
                .unwrap(),
            Outcome::Cancelled
        );
        assert!(profile.sdk_token().unwrap().is_none());
    }

    #[tokio::test]
    async fn phone_pages_save_the_same_token_to_two_independent_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(Layout::new(dir.path().join("data"))).unwrap();
        for name in ["first", "second"] {
            let profile = store.create(name).unwrap();
            let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
            let url = page.url.clone();
            let task =
                tokio::spawn(page.serve(profile, Arc::new(Notify::new()), Duration::from_secs(30)));
            assert!(request(&url, &post(&format!("token={TOKEN}")))
                .await
                .contains("Token saved"));
            assert_eq!(task.await.unwrap(), Outcome::Saved);
        }
        for name in ["first", "second"] {
            assert_eq!(
                store
                    .profile(name)
                    .unwrap()
                    .sdk_token()
                    .unwrap()
                    .unwrap()
                    .expose(),
                TOKEN
            );
        }
    }
}
