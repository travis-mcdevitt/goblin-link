//! A scripted stand-in for the Hermes plugin, for tests here and in the daemon.

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::{Device, LinkError, PairLink, Pairing, Pushed, Runner, Session};

/// The certificate the fake plugin serves, and its SHA-256.
pub const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/cert.der");
const KEY_DER: &[u8] = include_bytes!("../tests/fixtures/key.der");
pub const FINGERPRINT: &str = "ec4c4552c39e735b253c735df678687a182bceaec7cf9399f95e1e7cc20ebb1b";
/// The one pairing code the fake accepts, once, and the token it gives.
pub const CODE: &str = "PAIRME42PAIRME42PAIRME42";
pub const TOKEN: &str = "device-token-0123456789abcdef0123456789";

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
}

impl FakeHermes {
    /// Starts it on a free local port.
    pub async fn start(script: Script, tls: Tls) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = Arc::new(Mutex::new(Vec::new()));
        let (push, pushes) = mpsc::unbounded_channel::<Value>();
        let pushes = Arc::new(tokio::sync::Mutex::new(pushes));
        let code_spent = Arc::new(Mutex::new(false));
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
                let (script, log, pushes, spent) = (
                    script.clone(),
                    log.clone(),
                    pushes.clone(),
                    code_spent.clone(),
                );
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor {
                        Some(a) => {
                            if let Ok(stream) = a.accept(tcp).await {
                                serve(stream, script, log, pushes, spent).await;
                            }
                        }
                        None => serve(tcp, script, log, pushes, spent).await,
                    }
                });
            }
        });
        Self {
            port,
            received,
            push,
        }
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
            device_id: "dev-1".into(),
            token: TOKEN.into(),
        }
    }
}

async fn serve<S>(
    stream: S,
    script: Script,
    log: Arc<Mutex<Vec<Value>>>,
    pushes: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>>,
    spent: Arc<Mutex<bool>>,
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
        };
        log.lock().unwrap().push(frame.clone());
        let mut answers = Vec::new();
        match frame["type"].as_str().unwrap_or("") {
            "pair" => {
                let fresh = !std::mem::replace(&mut *spent.lock().unwrap(), true);
                answers.push(if frame["code"] == CODE && fresh {
                    json!({"type": "paired", "token": TOKEN})
                } else {
                    json!({"type": "error", "reason": "that code is spent or wrong"})
                });
            }
            "hello" => answers.push(if frame["token"] == TOKEN {
                json!({"type": "hello.ok", "agent": "Hermes"})
            } else {
                json!({"type": "error", "code": "unpaired", "reason": "this handheld is not paired"})
            }),
            "speech.get" => answers.push(if frame["text"] == "" {
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
