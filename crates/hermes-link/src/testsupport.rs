//! A scripted stand-in for the Hermes plugin, for tests here and in the daemon.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::{Device, LinkError, PairLink, Pairing, Pushed, Runner, Session};

/// The certificate the fake plugin serves, and its SHA-256.
pub const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/cert.der");
const KEY_DER: &[u8] = include_bytes!("../tests/fixtures/key.der");
pub const FINGERPRINT: &str = "ec4c4552c39e735b253c735df678687a182bceaec7cf9399f95e1e7cc20ebb1b";
/// The pairing code the fake starts with, and the token it gives for it.
pub const CODE: &str = "PAIRME42PAIRME42PAIRME42";
pub const TOKEN: &str = "device-token-0123456789abcdef0123456789";
/// The id of the handheld the fake knows from the start, holding [`TOKEN`].
pub const DEVICE: &str = "dev-1";

/// A token for each key: pairing codes not yet spent, or handhelds by their id.
pub type Tokens = Arc<Mutex<HashMap<String, String>>>;

/// How the fake serves its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    /// No TLS: for [`pair_plain`] and [`connect_plain`].
    Off,
    /// The fixture certificate and its key.
    On,
    /// The same, offering TLS 1.2 only.
    Tls12,
    /// The fixture certificate with a key that is not its own: someone who has seen the
    /// certificate but does not hold its key.
    WrongKey,
    /// The same impostor, offering TLS 1.2 only.
    WrongKeyTls12,
}

/// What the fake does with a turn.
#[derive(Clone, Default)]
pub struct Script {
    /// Sent after `turn.accepted`, each with `"turn"` filled in.
    pub turn_events: Vec<Value>,
    /// Refuse every turn with this reason instead.
    pub reject: Option<String>,
    /// Stop reading and answering after the hello, without closing: a host that vanished.
    pub silent: bool,
    /// What `hello.ok` says of the agent besides `"type"`; `{"agent": "Hermes"}` when null.
    pub identity: Value,
    /// The picture given for `avatar.get`, by revision, as base64; any other is refused.
    pub avatars: HashMap<String, String>,
}

const OTHER_KEY_DER: &[u8] = include_bytes!("../tests/fixtures/other-key.der");

/// Serves one certificate to every client.
#[derive(Debug)]
struct One(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for One {
    fn resolve(
        &self,
        _hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// A running fake plugin.
pub struct FakeHermes {
    pub port: u16,
    /// Every message the fake received, in order.
    pub received: Arc<Mutex<Vec<Value>>>,
    /// Sends a message to the connected handheld.
    pub push: mpsc::UnboundedSender<Value>,
    /// Pairing codes it accepts, once each, with the token each gives: [`CODE`] at first.
    pub codes: Tokens,
    /// The handhelds it knows, by id, each with its token: [`DEVICE`] at first. One entry to
    /// an id, as the plugin files them: a pairing under an id replaces the one before.
    pub devices: Tokens,
    /// Counts the times it has hung up: each open connection watches for the next.
    hang_ups: Arc<watch::Sender<u64>>,
}

impl FakeHermes {
    /// Starts it on a free local port.
    pub async fn start(script: Script, tls: Tls) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let (push, pushes) = mpsc::unbounded_channel::<Value>();
        let pushes = Arc::new(tokio::sync::Mutex::new(pushes));
        let table = |key: &str, token: &str| -> Tokens {
            Arc::new(Mutex::new(HashMap::from([(key.into(), token.into())])))
        };
        let (codes, devices) = (table(CODE, TOKEN), table(DEVICE, TOKEN));
        let tables = (codes.clone(), devices.clone());
        let hang_ups = Arc::new(watch::channel(0).0);
        let hanging_up = hang_ups.clone();
        let log = received.clone();
        tokio::spawn(async move {
            let acceptor = (tls != Tls::Off).then(|| {
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let key = if matches!(tls, Tls::WrongKey | Tls::WrongKeyTls12) {
                    OTHER_KEY_DER
                } else {
                    KEY_DER
                };
                let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key.to_vec());
                let signer = provider.key_provider.load_private_key(key.into()).unwrap();
                // Built by hand: rustls would refuse a key that is not the certificate's.
                let certified = Arc::new(rustls::sign::CertifiedKey::new(
                    vec![CERT_DER.to_vec().into()],
                    signer,
                ));
                let versions: &[&rustls::SupportedProtocolVersion] =
                    if matches!(tls, Tls::Tls12 | Tls::WrongKeyTls12) {
                        &[&rustls::version::TLS12]
                    } else {
                        rustls::ALL_VERSIONS
                    };
                let config = rustls::ServerConfig::builder_with_provider(provider)
                    .with_protocol_versions(versions)
                    .unwrap()
                    .with_no_client_auth()
                    .with_cert_resolver(Arc::new(One(certified)));
                tokio_rustls::TlsAcceptor::from(Arc::new(config))
            });
            while let Ok((tcp, _)) = listener.accept().await {
                let (script, log, pushes, tables) =
                    (script.clone(), log.clone(), pushes.clone(), tables.clone());
                let acceptor = acceptor.clone();
                // From here on: a connection made after a hang-up is not hung up by it.
                let hung_up = hanging_up.subscribe();
                tokio::spawn(async move {
                    match acceptor {
                        Some(a) => {
                            if let Ok(stream) = a.accept(tcp).await {
                                serve(stream, script, log, pushes, tables, hung_up).await;
                            }
                        }
                        None => serve(tcp, script, log, pushes, tables, hung_up).await,
                    }
                });
            }
        });
        Self {
            port,
            received,
            push,
            codes,
            devices,
            hang_ups,
        }
    }

    /// Drops every open connection, as a Hermes that starts again does. It goes on
    /// listening, and knows the same handhelds.
    pub fn hang_up(&self) {
        self.hang_ups.send_modify(|times| *times += 1);
    }

    /// A pairing link to this fake.
    pub fn link(&self, fingerprint: &str, code: &str) -> PairLink {
        PairLink {
            hosts: vec!["127.0.0.1".into()],
            port: self.port,
            fingerprint: fingerprint.into(),
            code: code.into(),
        }
    }

    /// The pairing a handheld holds after pairing with this fake.
    pub fn pairing(&self) -> Pairing {
        Pairing {
            hosts: vec!["127.0.0.1".into()],
            port: self.port,
            fingerprint: FINGERPRINT.into(),
            device_id: DEVICE.into(),
            token: TOKEN.into(),
        }
    }
}

async fn serve<S>(
    stream: S,
    script: Script,
    log: Arc<Mutex<Vec<Value>>>,
    pushes: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
    (codes, devices): (Tokens, Tokens),
    mut hung_up: watch::Receiver<u64>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    // Pushes go to the first handheld connected; a second connection gets none.
    let mut pushes = pushes.try_lock().ok();
    loop {
        let push = async {
            match pushes.as_mut() {
                Some(p) => p.recv().await,
                None => std::future::pending().await,
            }
        };
        let frame = tokio::select! {
            incoming = ws.next() => match incoming {
                Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text).unwrap(),
                Some(Ok(_)) => continue,
                _ => return,
            },
            Some(push) = push => {
                let _ = ws.send(Message::Text(push.to_string())).await;
                continue;
            }
            _ = hung_up.changed() => return,
        };
        log.lock().unwrap().push(frame.clone());
        let mut answers = Vec::new();
        match frame["type"].as_str().unwrap_or("") {
            "pair" => {
                let code = frame["code"].as_str().unwrap_or("");
                let token = codes.lock().unwrap().remove(code);
                answers.push(match (token, frame["device"]["id"].as_str()) {
                    (Some(token), Some(id)) => {
                        devices.lock().unwrap().insert(id.into(), token.clone());
                        json!({"type": "paired", "token": token})
                    }
                    _ => json!({"type": "error", "reason": "that code is spent or wrong"}),
                });
            }
            "avatar.get" => {
                let revision = frame["revision"].as_str().unwrap_or("");
                answers.push(match script.avatars.get(revision) {
                    Some(png) => json!({"type": "avatar", "id": frame["id"],
                                        "revision": revision, "png_b64": png}),
                    None => json!({"type": "avatar.failed", "id": frame["id"],
                                   "reason": "no such picture now"}),
                });
            }
            "hello" => answers.push(if known(&devices, &frame) {
                let mut ok = match &script.identity {
                    Value::Object(said) => Value::Object(said.clone()),
                    _ => json!({"agent": "Hermes"}),
                };
                ok["type"] = json!("hello.ok");
                ok
            } else {
                json!({"type": "error", "code": "unpaired", "reason": "this handheld is not paired"})
            }),
            "speech.get" => answers.push(if frame["text"] == "as wav" {
                // "RIFF": audio, but not MP3.
                json!({"type": "speech", "id": frame["id"], "mp3_b64": "UklGRgAA"})
            } else if frame["text"] == "" {
                json!({"type": "speech.failed", "id": frame["id"], "reason": "nothing to say"})
            } else {
                // "ID3" and a little more: enough to be taken for an MP3 by a test.
                json!({"type": "speech", "id": frame["id"], "mp3_b64": "SUQzAAAA"})
            }),
            "turn" => match &script.reject {
                Some(reason) => answers
                    .push(json!({"type": "turn.rejected", "id": frame["id"], "reason": reason})),
                None => {
                    answers.push(
                        json!({"type": "turn.accepted", "id": frame["id"], "message_id": "u1"}),
                    );
                    for event in &script.turn_events {
                        let mut event = event.clone();
                        event["turn"] = frame["id"].clone();
                        answers.push(event);
                    }
                }
            },
            _ => {}
        }
        for answer in answers {
            if ws.send(Message::Text(answer.to_string())).await.is_err() {
                return;
            }
        }
        if script.silent && frame["type"] == "hello" {
            std::future::pending::<()>().await;
        }
    }
}

/// Whether a hello names a handheld the fake knows and carries that handheld's token.
fn known(devices: &Tokens, hello: &Value) -> bool {
    let (Some(id), Some(token)) = (hello["device"].as_str(), hello["token"].as_str()) else {
        return false;
    };
    devices.lock().unwrap().get(id).map(String::as_str) == Some(token)
}

/// [`crate::pair`] over a connection without TLS.
pub async fn pair_plain(link: &PairLink, device: &Device) -> Result<Pairing, LinkError> {
    crate::pair_over(link, device, true).await
}

/// [`Session::connect`] over a connection without TLS.
pub async fn connect_plain(
    pairing: &Pairing,
    device: &Device,
    runner: Arc<dyn Runner>,
    pushed: mpsc::Sender<Pushed>,
) -> Result<Session, LinkError> {
    Session::connect_over(pairing, device, runner, pushed, true).await
}
