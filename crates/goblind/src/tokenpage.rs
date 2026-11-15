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
    /// A token was saved to the profile. With it, the name the owner typed for the profile,
    /// when it is not the one it has.
    Saved(Option<String>),
    /// A Hermes pairing link was pasted, and the name typed with it.
    Hermes(hermes_link::PairLink, Option<String>),
    /// The owner closed the pairing screen.
    Cancelled,
    /// Nobody sent a token in time.
    TimedOut,
    /// It was closed for another reason, in words for the owner.
    Failed(String),
}

/// Which kind of agent the page is for: it asks only for what that one needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum For {
    /// Not said (an older client): a Muse token, or a Hermes link in its place.
    Any,
    /// Muse: an SDK token.
    Muse,
    /// A Hermes Agent: its pairing link.
    Hermes,
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
    /// Serves the page until it has what it is for. `kind` says which agent's page it is.
    /// The page also takes a name for the profile; `taken` are the names it may not be.
    pub async fn serve(
        self,
        profile: Profile,
        cancel: Arc<Notify>,
        timeout: Duration,
        kind: For,
        taken: Vec<String>,
    ) -> Outcome {
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
                result = tokio::time::timeout(Duration::from_secs(10), handle(stream, &profile, kind, &taken)) => result,
            };
            match handled {
                Ok(Reply::Saved(name)) => return Outcome::Saved(name),
                Ok(Reply::Hermes(link, name)) => return Outcome::Hermes(link, name),
                _ => {}
            }
        }
    }
}

enum Reply {
    Saved(Option<String>),
    Hermes(hermes_link::PairLink, Option<String>),
    Other,
}

/// The name typed on the page: `Ok(None)` when it is the one the profile has (or nothing),
/// and why not when it cannot be a profile's name.
fn typed_name(
    typed: &str,
    profile: &Profile,
    taken: &[String],
) -> Result<Option<String>, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() || typed == profile.name() {
        return Ok(None);
    }
    if typed.eq_ignore_ascii_case(profile.name()) {
        // The handheld keeps names apart without regard to capitals.
        return Err("That is the name it has, in other capitals.");
    }
    if goblin_store::validate_profile_name(typed).is_err() {
        return Err("A name is up to 32 letters, digits, - or _.");
    }
    if taken.iter().any(|t| t.eq_ignore_ascii_case(typed)) {
        return Err("Another profile has that name.");
    }
    Ok(Some(typed.to_owned()))
}

async fn handle(mut stream: TcpStream, profile: &Profile, kind: For, taken: &[String]) -> Reply {
    let hermes = kind == For::Hermes;
    let Some((method, path, body)) = read_request(&mut stream).await else {
        return Reply::Other;
    };
    let name = escape(profile.name());
    let (status, page, reply) = match (method.as_str(), path.as_str()) {
        ("GET", "/") => ("200 OK", form(&name, "", kind), Reply::Other),
        ("POST", "/") => {
            let field = |key: &str| {
                body.split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| decode(v))
                    .unwrap_or_default()
            };
            let pasted = field("token");
            let pasted = pasted.trim();
            // The name is settled first: nothing is saved or used with a name that cannot be.
            let rename = typed_name(&field("name"), profile, taken);
            let shown = match &rename {
                Ok(Some(new)) => escape(new),
                _ => name.clone(),
            };
            if let Err(why) = rename {
                ("200 OK", form(&name, why, kind), Reply::Other)
            } else if pasted.starts_with("goblinlink:") && kind != For::Muse {
                // A link from a Hermes in place of a Muse's token: this profile's agent is
                // a Hermes. It is used at once, by the daemon, and not kept.
                match hermes_link::PairLink::parse(pasted) {
                    Ok(link) => (
                        "200 OK",
                        received(&shown),
                        Reply::Hermes(link, rename.ok().flatten()),
                    ),
                    Err(_) => (
                        "200 OK",
                        form(
                            &name,
                            "That is not a whole pairing link. Copy it again.",
                            kind,
                        ),
                        Reply::Other,
                    ),
                }
            } else if hermes {
                (
                    "200 OK",
                    form(
                        &name,
                        "That is not a pairing link. It starts with goblinlink://",
                        kind,
                    ),
                    Reply::Other,
                )
            } else {
                match goblin_store::profiles::SdkToken::parse(pasted) {
                    Ok(token) => match profile.save_sdk_token(&token) {
                        Ok(()) => ("200 OK", done(&shown), Reply::Saved(rename.ok().flatten())),
                        Err(_) => (
                            "200 OK",
                            form(&name, "The handheld could not save it.", kind),
                            Reply::Other,
                        ),
                    },
                    Err(_) => (
                        "200 OK",
                        form(&name, "That is not an SDK token. Copy it again.", kind),
                        Reply::Other,
                    ),
                }
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

/// The page's look: one column made for a phone, light or dark as the phone is, nothing
/// fetched from anywhere (the handheld may be the only thing the phone can reach).
const STYLE: &str = ":root{--a:#0a63d6;--on:#fff;--bg:#f3f4f6;--card:#fff;--t:#14161a;--m:#5d6470;\
--l:#e2e5ea;--eb:#fde8e6;--et:#8c1d18}\
@media(prefers-color-scheme:dark){:root{--a:#4c9bff;--on:#08101c;--bg:#0d0e10;--card:#1a1c20;\
--t:#f1f2f4;--m:#9ba2ad;--l:#2c2f36;--eb:#3a1614;--et:#ffb4ab}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--t);font:17px/1.45 system-ui,-apple-system,sans-serif;\
-webkit-text-size-adjust:100%}\
main{max-width:30em;margin:auto;padding:22px 18px 44px}\
.brand{font-size:13px;font-weight:600;letter-spacing:.09em;text-transform:uppercase;color:var(--m);\
margin:4px 2px 16px}\
h1{font-size:30px;line-height:1.12;letter-spacing:-.015em;margin:0 0 6px}\
h2{font-size:18px;margin:0 0 8px}\
p{margin:0 0 10px}p:last-child{margin-bottom:0}\
.sub{color:var(--m);margin:0 0 18px}\
.card{background:var(--card);border-radius:18px;padding:18px;margin:14px 0;\
box-shadow:0 1px 2px rgba(0,0,0,.06)}\
ol{list-style:none;margin:0;padding:0;counter-reset:s}\
ol li{counter-increment:s;position:relative;padding:2px 0 18px 44px;color:var(--m)}\
ol li:last-child{padding-bottom:2px}\
ol li::before{content:counter(s);position:absolute;left:0;top:0;width:30px;height:30px;\
border-radius:50%;border:2px solid var(--l);display:flex;align-items:center;justify-content:center;\
font-weight:600;font-size:15px;line-height:1}\
ol li::after{content:'';position:absolute;left:14px;top:34px;bottom:4px;width:2px;background:var(--l)}\
ol li:last-child::after{display:none}\
ol li[aria-current],ol.all li{color:var(--t)}\
ol li[aria-current]::before{background:var(--a);border-color:var(--a);color:var(--on)}\
ol li.d::before{content:'\\2713';background:var(--l)}\
label{display:block;font-size:14px;font-weight:600;color:var(--m);margin:16px 0 6px}\
form label:first-child{margin-top:0}\
input{width:100%;padding:14px;border:1.5px solid var(--l);border-radius:12px;background:var(--bg);\
color:var(--t);font:inherit}\
input:focus{outline:2px solid var(--a);outline-offset:1px;border-color:var(--a)}\
button,a.b{display:block;width:100%;padding:15px;margin:18px 0 0;border:0;border-radius:14px;\
background:var(--a);color:var(--on);font:inherit;font-weight:600;text-align:center;\
text-decoration:none}\
a.b{margin:0 0 12px;background:transparent;color:var(--a);border:1.5px solid var(--l)}\
p.e{background:var(--eb);color:var(--et);border-radius:12px;padding:12px 14px;margin:14px 0}\
code{display:block;font:14px ui-monospace,monospace;background:var(--bg);padding:8px 10px;\
border-radius:8px;margin:6px 0 2px;white-space:pre-wrap;word-break:break-all}\
.n{color:var(--m);font-size:15px}\
.ok{width:60px;height:60px;border-radius:50%;background:var(--a);color:var(--on);display:flex;\
align-items:center;justify-content:center;font-size:32px;margin:6px 0 18px}";

fn shell(body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>Goblin Link</title><style>{STYLE}</style>\
         <main><div class=\"brand\">Goblin Link</div>{body}</main></html>"
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
        } else if i + 1 < current {
            html.push_str(&format!("<li class=\"d\">{label}</li>"));
        } else {
            html.push_str(&format!("<li>{label}</li>"));
        }
    }
    html.push_str("</ol>");
    html
}

const MUSE_STEP: &str = "<p>Open <strong>Muse → Settings → Devices</strong>. Enable \
    <strong>Developer mode</strong>, tap <strong>+</strong>, then choose the device shown \
    on your handheld.</p><p class=\"n\">Turn on your phone’s Bluetooth. Keep pairing open on \
    the handheld. If Muse asks for Wi-Fi, choose the handheld’s current network.</p>";

/// How to install the plugin, for a Hermes that does not have it yet.
pub const HERMES_PLUGIN: &str = "travis-mcdevitt/goblin-link#hermes-plugin";

fn hermes_form(profile: &str, error: &str) -> String {
    let name_box = name_box(profile);
    shell(&format!(
        "<h1>Pair with Hermes</h1><p class=\"sub\">Profile: {profile}</p>{error}\
         <div class=\"card\"><ol class=\"all\"><li>If your Hermes does not have the Goblin Link \
         plugin yet, ask it to install <strong>{HERMES_PLUGIN}</strong> and restart its gateway. \
         <span class=\"n\">Or, on its machine: <code>hermes plugins install {HERMES_PLUGIN}</code>\
         </span></li>\
         <li>Send <strong>/goblinlink pair</strong> to your Hermes in a private chat.</li>\
         <li>Copy the link it answers with and paste it below.</li></ol></div>\
         <div class=\"card\"><form method=\"post\" action=\"/\">{name_box}\
         <label for=\"token\">Pairing link</label>\
         <input id=\"token\" name=\"token\" placeholder=\"goblinlink://…\" autocomplete=\"off\" \
         autocapitalize=\"off\" spellcheck=\"false\" required>\
         <button>Pair</button></form></div>\
         <p class=\"n\">The link works once, for ten minutes. Your handheld and your Hermes must \
         be able to reach each other: on the same network, or over a VPN.</p>"
    ))
}

/// The box for the profile's name, holding the one it has.
fn name_box(profile: &str) -> String {
    format!(
        "<label for=\"name\">Profile name</label>\
         <input id=\"name\" name=\"name\" value=\"{profile}\" maxlength=\"32\" autocomplete=\"off\" \
         autocapitalize=\"off\" spellcheck=\"false\">"
    )
}

fn form(profile: &str, error: &str, kind: For) -> String {
    let error = if error.is_empty() {
        String::new()
    } else {
        format!("<p class=\"e\" role=\"alert\">{}</p>", escape(error))
    };
    if kind == For::Hermes {
        return hermes_form(profile, &error);
    }
    // Asked for a Muse, the page says nothing of a Hermes.
    let (title, hint, other) = if kind == For::Muse {
        ("Pair with Muse", "Paste your SDK token", "")
    } else {
        (
            "Pair your handheld",
            "Paste your SDK token or Hermes link",
            "<div class=\"card\"><h2>Using Hermes instead?</h2><p>Send <strong>/goblinlink \
             pair</strong> to your Hermes in a private chat, and paste the link it answers with \
             in the box above.</p></div>",
        )
    };
    let name_box = name_box(profile);
    shell(&format!(
        "<h1>{title}</h1><p class=\"sub\">Profile: {profile}</p>{error}\
         <div class=\"card\">{steps}</div>\
         <div class=\"card\"><a class=\"b\" href=\"{TOKEN_SITE}\" target=\"_blank\" rel=\"noopener\">\
         Get or manage SDK tokens</a>\
         <p class=\"n\">Copy your token from Muse Gadgets → SDK tokens, then return here. \
         Reuse an existing token for this Muse account. Replacing it can disconnect other devices.</p>\
         <form method=\"post\" action=\"/\">\
         {name_box}<label for=\"token\">SDK token</label>\
         <input id=\"token\" name=\"token\" placeholder=\"{hint}\" autocomplete=\"off\" \
         autocapitalize=\"off\" spellcheck=\"false\" required>\
         <button>Save token</button></form></div>\
         <div class=\"card\"><h2>Next: pair in Muse</h2>{MUSE_STEP}</div>{other}",
        steps = steps(false)
    ))
}

fn received(profile: &str) -> String {
    shell(&format!(
        "<div class=\"ok\" aria-hidden=\"true\">&#10003;</div><h1>Link received</h1>\
         <p class=\"sub\">Profile: {profile}</p>\
         <div class=\"card\"><p>The handheld is pairing with your Hermes now. Watch its \
         screen.</p></div>"
    ))
}

fn done(profile: &str) -> String {
    shell(&format!(
        "<div class=\"ok\" aria-hidden=\"true\">&#10003;</div><h1>Token saved</h1>\
         <p class=\"sub\">Profile: {profile}</p><div class=\"card\">{steps}</div>\
         <div class=\"card\"><h2>Finish pairing in Muse</h2>{MUSE_STEP}</div>",
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
        let served = tokio::spawn(page.serve(
            p,
            Arc::new(Notify::new()),
            Duration::from_secs(30),
            For::Any,
            Vec::new(),
        ));
        let reply = request(
            &url,
            "POST / HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n",
        )
        .await;
        assert!(reply.is_empty());
        assert!(!served.is_finished(), "bad request must not stop the page");
        let reply = request(&url, &post(&format!("token={TOKEN}"))).await;
        assert!(reply.contains("Token saved"));
        assert_eq!(served.await.unwrap(), Outcome::Saved(None));
    }

    /// The page for a new profile: the owner names it on the phone, with what its agent
    /// needs. A name that cannot be a profile's is said, and nothing is saved with it.
    #[tokio::test]
    async fn the_page_takes_a_name_with_the_token_or_the_link() {
        let fp = "ab".repeat(32);
        let link = format!(
            "goblinlink%3A%2F%2Fhost.example%3A47832%2F%3Ffp%3D{fp}%26code%3DABCD2345ABCD2345ABCD2345"
        );
        for kind in [For::Muse, For::Hermes] {
            let t = tempfile::tempdir().unwrap();
            let p = profile(t.path());
            let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
            let url = page.url.clone();
            let served = tokio::spawn(page.serve(
                p.clone(),
                Arc::new(Notify::new()),
                Duration::from_secs(30),
                kind,
                vec!["work".into(), "Home".into()],
            ));
            let pasted = if kind == For::Muse {
                TOKEN.to_owned()
            } else {
                link.clone()
            };
            let html = request(&url, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
            assert!(
                html.contains("name=\"name\" value=\"work\""),
                "{kind:?}: {html}"
            );
            // Asked for one agent, the page says nothing of the other.
            assert_eq!(html.contains("Hermes"), kind == For::Hermes, "{kind:?}");
            for (bad, why) in [
                ("two%20words", "up to 32"),
                ("home", "Another profile"),
                ("Work", "other capitals"),
            ] {
                let html = request(&url, &post(&format!("name={bad}&token={pasted}"))).await;
                assert!(html.contains(why), "{kind:?} {bad}: {html}");
                assert!(
                    p.sdk_token().unwrap().is_none(),
                    "nothing saved with a bad name"
                );
                assert!(!served.is_finished());
            }
            let html = request(&url, &post(&format!("name=den&token={pasted}"))).await;
            assert!(html.contains("Profile: den"), "{kind:?}: {html}");
            match (kind, served.await.unwrap()) {
                (For::Muse, Outcome::Saved(name)) => assert_eq!(name.as_deref(), Some("den")),
                (For::Hermes, Outcome::Hermes(_, name)) => {
                    assert_eq!(name.as_deref(), Some("den"));
                }
                other => panic!("{other:?}"),
            }
        }
        // The name it already has is no change, and a Muse page does not take a link.
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served = tokio::spawn(page.serve(
            p,
            Arc::new(Notify::new()),
            Duration::from_secs(30),
            For::Muse,
            vec!["work".into()],
        ));
        let html = request(&url, &post(&format!("name=work&token={link}"))).await;
        assert!(html.contains("not an SDK token"), "{html}");
        let html = request(&url, &post(&format!("name=+work+&token={TOKEN}"))).await;
        assert!(html.contains("Token saved"), "{html}");
        assert_eq!(served.await.unwrap(), Outcome::Saved(None));
    }

    /// Writes the pages out to look at (only when asked: `GOBLIN_PAGES=<dir>`).
    #[test]
    fn pages_for_looking_at() {
        let Ok(dir) = std::env::var("GOBLIN_PAGES") else {
            return;
        };
        let dir = std::path::Path::new(&dir);
        for (name, html) in [
            ("muse.html", form("den", "", For::Muse)),
            (
                "muse-error.html",
                form("den", "That is not an SDK token. Copy it again.", For::Muse),
            ),
            ("hermes.html", form("den", "", For::Hermes)),
            ("any.html", form("den", "", For::Any)),
            ("saved.html", done("den")),
            ("received.html", received("den")),
        ] {
            std::fs::write(dir.join(name), html).unwrap();
        }
    }

    #[tokio::test]
    async fn the_page_takes_one_token() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served = tokio::spawn(page.serve(
            p.clone(),
            Arc::new(Notify::new()),
            Duration::from_secs(30),
            For::Any,
            Vec::new(),
        ));

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
        assert_eq!(served.await.unwrap(), Outcome::Saved(None));
        assert!(p.sdk_token().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_hermes_link_is_taken_in_place_of_a_token_and_not_kept() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served = tokio::spawn(page.serve(
            p.clone(),
            Arc::new(Notify::new()),
            Duration::from_secs(30),
            For::Any,
            Vec::new(),
        ));
        let html = request(&url, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(html.contains("/goblinlink pair"), "{html}");

        // Half a link is neither a link nor a token, and the page stays open.
        let html = request(
            &url,
            &post("token=goblinlink%3A%2F%2Fhost%3A1%2F%3Ffp%3D00"),
        )
        .await;
        assert!(html.contains("not a whole pairing link"), "{html}");

        let fp = "ab".repeat(32);
        let code = "ABCD2345ABCD2345ABCD2345";
        let link = format!(
            "goblinlink%3A%2F%2F192.168.1.20%2Chost.example%3A47832%2F%3Ffp%3D{fp}%26code%3D{code}"
        );
        let html = request(&url, &post(&format!("token=+{link}%0A"))).await;
        assert!(html.contains("Link received"), "{html}");
        assert!(!html.contains(code), "the page does not show the code back");
        let Outcome::Hermes(link, None) = served.await.unwrap() else {
            panic!("the page did not hand the link over");
        };
        assert_eq!(link.hosts, ["192.168.1.20", "host.example"]);
        assert_eq!(
            (link.port, link.fingerprint, link.code.as_str()),
            (47832, fp, code)
        );
        // Nothing of it is written to the profile by the page.
        assert!(p.sdk_token().unwrap().is_none());
        assert!(p.hermes_pairing().unwrap().is_none());
    }

    #[tokio::test]
    async fn the_hermes_page_says_how_to_get_a_link_and_takes_nothing_else() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let page = Page::open("127.0.0.1".parse().unwrap()).await.unwrap();
        let url = page.url.clone();
        let served = tokio::spawn(page.serve(
            p.clone(),
            Arc::new(Notify::new()),
            Duration::from_secs(30),
            For::Hermes,
            Vec::new(),
        ));
        let html = request(&url, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(html.contains("Pair with Hermes"), "{html}");
        assert!(html.contains(HERMES_PLUGIN) && html.contains("restart its gateway"));
        assert!(html.contains("/goblinlink pair"));
        assert!(
            !html.contains(TOKEN_SITE),
            "nothing about SDK tokens: {html}"
        );
        // An SDK token is not what this page is for, and is not saved.
        let html = request(&url, &post(&format!("token={TOKEN}"))).await;
        assert!(html.contains("not a pairing link"), "{html}");
        assert!(p.sdk_token().unwrap().is_none());
        let fp = "ab".repeat(32);
        let link = format!("goblinlink%3A%2F%2Fhost.example%3A47832%2F%3Ffp%3D{fp}%26code%3DABCD2345ABCD2345ABCD2345");
        let html = request(&url, &post(&format!("token={link}"))).await;
        assert!(html.contains("Link received"), "{html}");
        assert!(matches!(served.await.unwrap(), Outcome::Hermes(_, None)));
    }

    #[tokio::test]
    async fn the_page_closes_on_cancel_and_time() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(t.path());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        let cancel = Arc::new(Notify::new());
        let page = Page::open(ip).await.unwrap();
        let served = tokio::spawn(page.serve(
            p.clone(),
            cancel.clone(),
            Duration::from_secs(30),
            For::Any,
            Vec::new(),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.notify_one();
        assert_eq!(served.await.unwrap(), Outcome::Cancelled);

        let page = Page::open(ip).await.unwrap();
        let out = page
            .serve(
                p,
                Arc::new(Notify::new()),
                Duration::from_millis(50),
                For::Any,
                Vec::new(),
            )
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
        let served = tokio::spawn(page.serve(
            profile.clone(),
            cancel.clone(),
            Duration::from_secs(30),
            For::Any,
            Vec::new(),
        ));
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
            let task = tokio::spawn(page.serve(
                profile,
                Arc::new(Notify::new()),
                Duration::from_secs(30),
                For::Any,
                Vec::new(),
            ));
            assert!(request(&url, &post(&format!("token={TOKEN}")))
                .await
                .contains("Token saved"));
            assert_eq!(task.await.unwrap(), Outcome::Saved(None));
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
