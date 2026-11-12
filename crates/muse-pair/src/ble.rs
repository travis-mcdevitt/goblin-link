//! The BlueZ GATT peripheral that carries setup (port of `ble_server_dbusnext.py`).
//!
//! One service with a write characteristic (app to device) and a notify characteristic
//! (device to app), and an advertisement with the service UUID, the local name and a
//! "paired" flag. `bluetoothd` must already be running; bringing the radio up is the
//! platform's job, as is repairing the advertising data on controllers whose kernel driver
//! does not program it (see the Brick's `bluetooth.sh` hook).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{fdo, interface, Connection};

/// The setup service.
pub const SERVICE_UUID: &str = "7fdd3d1c-38ea-46cf-8b46-314ecf5f240c";
const RX_UUID: &str = "4d593029-28a2-4a6e-a1f0-3c2d5e8f9b01";
const TX_UUID: &str = "d75dc4ca-7b2b-4e9c-8f0a-1d2e3f4a5b6c";
const PAIRED_FLAG_COMPANY_ID: u16 = 0xFFFF;

const APP_PATH: &str = "/org/musegadget/app0";
const SERVICE_PATH: &str = "/org/musegadget/app0/service0";
const RX_PATH: &str = "/org/musegadget/app0/service0/rx0";
const TX_PATH: &str = "/org/musegadget/app0/service0/tx0";
const ADV_PATH: &str = "/org/musegadget/advertisement0";
const BLUEZ: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
/// Bound on each step of putting the adapter back.
const TEARDOWN_STEP: Duration = Duration::from_millis(1500);

/// Why the peripheral could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum BleError {
    /// The system bus or BlueZ did not answer.
    #[error("Bluetooth service: {0}")]
    Bus(#[from] zbus::Error),
    /// A D-Bus call was refused.
    #[error("Bluetooth service: {0}")]
    Fdo(#[from] fdo::Error),
    /// BlueZ knows no adapter.
    #[error("no Bluetooth adapter found")]
    NoAdapter,
}

/// What the app did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleEvent {
    /// One write to the RX characteristic, with the ATT MTU when BlueZ reports it.
    Write {
        /// The bytes written.
        data: Vec<u8>,
        /// Negotiated MTU.
        mtu: Option<u16>,
    },
    /// The app subscribed to notifications.
    Subscribed,
    /// The app's device is no longer connected.
    Disconnected,
}

#[derive(Default)]
struct Shared {
    device: Option<OwnedObjectPath>,
    notifying: bool,
}

struct GattService;

#[interface(name = "org.bluez.GattService1")]
impl GattService {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        SERVICE_UUID.into()
    }
    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

struct RxCharacteristic {
    shared: Arc<Mutex<Shared>>,
    events: mpsc::Sender<BleEvent>,
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl RxCharacteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        RX_UUID.into()
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(SERVICE_PATH).into()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        vec!["write".into(), "write-without-response".into()]
    }
    fn write_value(&self, value: Vec<u8>, options: HashMap<String, OwnedValue>) {
        let mtu = match options.get("mtu").map(|v| &**v) {
            Some(Value::U16(m)) => Some(*m),
            _ => None,
        };
        if let Some(Value::ObjectPath(path)) = options.get("device").map(|v| &**v) {
            self.shared.lock().unwrap().device = Some(path.clone().into());
        }
        if self
            .events
            .try_send(BleEvent::Write { data: value, mtu })
            .is_err()
        {
            log::warn!("setup write dropped: the handler is not keeping up");
        }
    }
    fn read_value(&self, _options: HashMap<String, OwnedValue>) -> Vec<u8> {
        Vec::new()
    }
}

struct TxCharacteristic {
    shared: Arc<Mutex<Shared>>,
    events: mpsc::Sender<BleEvent>,
    value: Vec<u8>,
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl TxCharacteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        TX_UUID.into()
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(SERVICE_PATH).into()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        vec!["read".into(), "notify".into()]
    }
    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.value.clone()
    }
    fn read_value(&self, _options: HashMap<String, OwnedValue>) -> Vec<u8> {
        self.value.clone()
    }
    fn start_notify(&self) {
        log::info!("client subscribed to {TX_UUID}");
        self.shared.lock().unwrap().notifying = true;
        let _ = self.events.try_send(BleEvent::Subscribed);
    }
    fn stop_notify(&self) {
        self.shared.lock().unwrap().notifying = false;
    }
}

struct Advertisement {
    local_name: String,
    paired: bool,
}

#[interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "peripheral".into()
    }
    #[zbus(property)]
    fn local_name(&self) -> String {
        self.local_name.clone()
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        vec![SERVICE_UUID.into()]
    }
    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, OwnedValue> {
        let flag = Value::from(vec![u8::from(self.paired)]);
        let mut m = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(flag) {
            m.insert(PAIRED_FLAG_COMPANY_ID, v);
        }
        m
    }
    fn release(&self) {
        log::info!("advertisement released by BlueZ");
    }
}

/// The exact LE advertising and scan-response data, for controllers that must be programmed
/// by hand: flags, the service UUID and the paired flag; then the name.
pub fn legacy_advertising_payload(local_name: &str, paired: bool) -> Option<(Vec<u8>, Vec<u8>)> {
    // The UUID in little-endian byte order, as it goes on air.
    let hex: String = SERVICE_UUID.chars().filter(|c| *c != '-').collect();
    let mut uuid: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect();
    uuid.reverse();
    let mut advertising = vec![0x02, 0x01, 0x02, 0x11, 0x07];
    advertising.extend_from_slice(&uuid);
    advertising.extend_from_slice(&[0x04, 0xff, 0xff, 0xff, u8::from(paired)]);
    let name = local_name.as_bytes();
    let mut scan_response = vec![name.len() as u8 + 1, 0x09];
    scan_response.extend_from_slice(name);
    (advertising.len() <= 31 && scan_response.len() <= 31).then_some((advertising, scan_response))
}

/// The peripheral, registered and advertising until [`BlePeripheral::close`].
pub struct BlePeripheral {
    conn: Connection,
    adapter: OwnedObjectPath,
    shared: Arc<Mutex<Shared>>,
    saved: Vec<(&'static str, OwnedValue)>,
    watcher: tokio::task::JoinHandle<()>,
}

async fn adapter_call(
    conn: &Connection,
    adapter: &ObjectPath<'_>,
    interface: &str,
    method: &str,
    path: &'static str,
    with_options: bool,
) -> Result<(), zbus::Error> {
    let object = ObjectPath::from_static_str_unchecked(path);
    let options: HashMap<String, Value<'_>> = HashMap::new();
    if with_options {
        conn.call_method(
            Some(BLUEZ),
            adapter,
            Some(interface),
            method,
            &(object, options),
        )
        .await?;
    } else {
        conn.call_method(Some(BLUEZ), adapter, Some(interface), method, &(object,))
            .await?;
    }
    Ok(())
}

impl BlePeripheral {
    /// Registers the service and starts advertising as `local_name`. Writes and
    /// disconnects arrive on `events`.
    pub async fn open(
        local_name: &str,
        paired: bool,
        events: mpsc::Sender<BleEvent>,
    ) -> Result<Self, BleError> {
        let conn = Connection::system().await?;
        let adapter = find_adapter(&conn).await?;
        log::info!("using adapter {}", adapter.as_str());

        // Read every original value before changing any, so each can be put back.
        let props = fdo::PropertiesProxy::builder(&conn)
            .destination(BLUEZ)?
            .path(adapter.clone())?
            .build()
            .await?;
        let iface = zbus::names::InterfaceName::from_static_str_unchecked(ADAPTER_IFACE);
        let mut saved = Vec::new();
        for key in ["Alias", "Pairable", "Powered"] {
            saved.push((key, props.get(iface.clone(), key).await?));
        }
        props
            .set(iface.clone(), "Powered", Value::from(true))
            .await?;
        // The phone reads the GAP name as well as the advertised one.
        props
            .set(iface.clone(), "Alias", Value::from(local_name))
            .await?;
        props
            .set(iface.clone(), "Pairable", Value::from(false))
            .await?;

        let shared = Arc::new(Mutex::new(Shared::default()));
        let server = conn.object_server();
        server.at(APP_PATH, fdo::ObjectManager).await?;
        server.at(SERVICE_PATH, GattService).await?;
        server
            .at(
                RX_PATH,
                RxCharacteristic {
                    shared: shared.clone(),
                    events: events.clone(),
                },
            )
            .await?;
        server
            .at(
                TX_PATH,
                TxCharacteristic {
                    shared: shared.clone(),
                    events: events.clone(),
                    value: Vec::new(),
                },
            )
            .await?;
        server
            .at(
                ADV_PATH,
                Advertisement {
                    local_name: local_name.to_owned(),
                    paired,
                },
            )
            .await?;

        let watcher = tokio::spawn(watch_device(conn.clone(), shared.clone(), events));
        let this = Self {
            conn,
            adapter,
            shared,
            saved,
            watcher,
        };
        let registered = async {
            adapter_call(
                &this.conn,
                &this.adapter,
                "org.bluez.GattManager1",
                "RegisterApplication",
                APP_PATH,
                true,
            )
            .await?;
            log::info!("GATT service registered");
            adapter_call(
                &this.conn,
                &this.adapter,
                "org.bluez.LEAdvertisingManager1",
                "RegisterAdvertisement",
                ADV_PATH,
                true,
            )
            .await
        }
        .await;
        if let Err(e) = registered {
            this.close().await;
            return Err(e.into());
        }
        log::info!("advertising as {local_name}");
        Ok(this)
    }

    /// Notifies `packets` in order, pausing between them. Nothing is sent to a client that
    /// has not subscribed.
    pub async fn notify(&self, packets: &[Vec<u8>]) {
        let Ok(tx) = self
            .conn
            .object_server()
            .interface::<_, TxCharacteristic>(TX_PATH)
            .await
        else {
            return;
        };
        for (i, packet) in packets.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(crate::framing::CHUNK_STAGGER_MS)).await;
            }
            if !self.shared.lock().unwrap().notifying {
                return;
            }
            let mut iface = tx.get_mut().await;
            iface.value = packet.clone();
            if let Err(e) = iface.value_changed(tx.signal_emitter()).await {
                log::warn!("notify failed: {e}");
            }
        }
    }

    /// Drops the link to the app's device.
    pub async fn disconnect(&self) {
        let device = self.shared.lock().unwrap().device.clone();
        let Some(device) = device else { return };
        if let Err(e) = self
            .conn
            .call_method(
                Some(BLUEZ),
                &device,
                Some("org.bluez.Device1"),
                "Disconnect",
                &(),
            )
            .await
        {
            log::warn!("disconnect failed: {e}");
        }
    }

    /// Stops advertising, unregisters the service and puts the adapter back as it was.
    /// Each step has its own time limit, so one that hangs cannot keep the rest from running.
    pub async fn close(self) {
        self.watcher.abort();
        for (interface, method, path) in [
            (
                "org.bluez.LEAdvertisingManager1",
                "UnregisterAdvertisement",
                ADV_PATH,
            ),
            ("org.bluez.GattManager1", "UnregisterApplication", APP_PATH),
        ] {
            // Registration may not have completed; a refusal here is expected then.
            let _ = tokio::time::timeout(
                TEARDOWN_STEP,
                adapter_call(&self.conn, &self.adapter, interface, method, path, false),
            )
            .await;
        }
        let iface = zbus::names::InterfaceName::from_static_str_unchecked(ADAPTER_IFACE);
        if let Ok(Ok(props)) = tokio::time::timeout(TEARDOWN_STEP, async {
            fdo::PropertiesProxy::builder(&self.conn)
                .destination(BLUEZ)?
                .path(self.adapter.clone())?
                .build()
                .await
        })
        .await
        {
            // Powered is restored last.
            for (key, value) in &self.saved {
                let set = props.set(
                    iface.clone(),
                    key,
                    (**value).try_clone().unwrap_or(Value::from(false)),
                );
                match tokio::time::timeout(TEARDOWN_STEP, set).await {
                    Ok(Ok(())) => {}
                    _ => log::warn!("could not restore adapter {key}"),
                }
            }
        } else {
            log::warn!("could not restore adapter properties");
        }
    }
}

async fn find_adapter(conn: &Connection) -> Result<OwnedObjectPath, BleError> {
    let manager = fdo::ObjectManagerProxy::builder(conn)
        .destination(BLUEZ)?
        .path("/")?
        .build()
        .await?;
    let objects = manager.get_managed_objects().await?;
    let mut adapters: Vec<OwnedObjectPath> = objects
        .into_iter()
        .filter(|(_, ifaces)| ifaces.keys().any(|i| i.as_str() == ADAPTER_IFACE))
        .map(|(path, _)| path)
        .collect();
    adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    adapters.into_iter().next().ok_or(BleError::NoAdapter)
}

/// Every two seconds, asks BlueZ whether the device that last wrote is still connected.
async fn watch_device(
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    events: mpsc::Sender<BleEvent>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let device = shared.lock().unwrap().device.clone();
        let Some(device) = device else { continue };
        let connected = async {
            let props = fdo::PropertiesProxy::builder(&conn)
                .destination(BLUEZ)?
                .path(device.clone())?
                .build()
                .await?;
            let iface = zbus::names::InterfaceName::from_static_str_unchecked("org.bluez.Device1");
            let value = props.get(iface, "Connected").await?;
            Ok::<bool, zbus::Error>(bool::try_from(value).unwrap_or(false))
        }
        .await
        .unwrap_or(false);
        if !connected {
            log::info!("device {} disconnected", device.as_str());
            {
                let mut s = shared.lock().unwrap();
                s.device = None;
                s.notifying = false;
            }
            if events.send(BleEvent::Disconnected).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_payload_is_what_the_sdk_programs() {
        let (adv, scan) = legacy_advertising_payload("MuseGadgetA1B2C3", false).unwrap();
        // Flags; 128-bit UUID list with the UUID reversed; manufacturer data FFFF + flag.
        assert_eq!(&adv[..5], &[0x02, 0x01, 0x02, 0x11, 0x07]);
        assert_eq!(adv[5], 0x0c, "the UUID's last byte goes first");
        assert_eq!(adv[20], 0x7f);
        assert_eq!(&adv[21..], &[0x04, 0xff, 0xff, 0xff, 0x00]);
        assert_eq!(adv.len(), 26);
        assert_eq!(scan[0] as usize, "MuseGadgetA1B2C3".len() + 1);
        assert_eq!(scan[1], 0x09);
        assert_eq!(&scan[2..], b"MuseGadgetA1B2C3");
        assert_eq!(legacy_advertising_payload("x", true).unwrap().0[25], 1);
        assert!(legacy_advertising_payload(&"n".repeat(30), false).is_none());
    }
}
