//! Battery, temperature and device health readouts.
//!
//! [`DeviceHealth`] mirrors the fields of `device.health` in the Python reference
//! (`gadgets/trimui-brick/pak/lib/musegadget/executor.py`): `version`, `hostname`, `uptime_s`,
//! `load`, `memory_mb {total, available}`, `disk_gb {total, free}`, `temperature_c`, `model`.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::config::PowerConfig;
use crate::error::{PlatformError, Result};

/// Battery and supply state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatteryStatus {
    /// Charge in percent, when reported.
    pub percent: Option<u8>,
    /// Text status from the kernel (`Charging`, `Discharging`, `Full`, ...).
    pub status: Option<String>,
    /// True when the battery reports `Charging` or an external supply is online.
    pub charging: bool,
    /// Whether an external supply is online, when a supply is declared.
    pub external_power: Option<bool>,
    /// Battery voltage in volts.
    pub voltage_v: Option<f32>,
    /// Battery temperature in degrees Celsius.
    pub temperature_c: Option<f32>,
}

/// Memory figures in MiB.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryMb {
    /// Total RAM.
    pub total: u64,
    /// RAM available without swapping.
    pub available: u64,
}

/// Root filesystem figures in GB (1e9 bytes).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskGb {
    /// Capacity.
    pub total: f64,
    /// Free space.
    pub free: f64,
}

/// Device status in the shape the `device.health` command reports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceHealth {
    /// Gadget software version.
    pub version: String,
    /// Host name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Seconds since boot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_s: Option<u64>,
    /// 1, 5 and 15 minute load averages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<[f64; 3]>,
    /// Memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<MemoryMb>,
    /// Disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_gb: Option<DiskGb>,
    /// SoC temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_c: Option<f64>,
    /// Hardware model string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().trim_matches('\0').to_owned())
}

/// Parse the contents of `/proc/uptime`.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

/// Parse the contents of `/proc/loadavg`.
pub fn parse_load(text: &str) -> Option<[f64; 3]> {
    let mut it = text.split_whitespace().map(|v| v.parse::<f64>().ok());
    let load = [it.next()??, it.next()??, it.next()??];
    Some(load.map(|v| (v * 100.0).round() / 100.0))
}

/// Parse the contents of `/proc/meminfo`.
pub fn parse_meminfo(text: &str) -> Option<MemoryMb> {
    let kib = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some(MemoryMb {
        total: kib("MemTotal")? / 1024,
        available: kib("MemAvailable")? / 1024,
    })
}

/// Parse `df -Pk <path>` output (POSIX format, 1024-byte blocks).
pub fn parse_df(text: &str) -> Option<DiskGb> {
    let line = text.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let blocks =
        |i: usize| -> Option<f64> { Some(fields.get(i)?.parse::<f64>().ok()? * 1024.0 / 1e9) };
    let round1 = |v: f64| (v * 10.0).round() / 10.0;
    Some(DiskGb {
        total: round1(blocks(1)?),
        free: round1(blocks(3)?),
    })
}

fn disk_usage() -> Option<DiskGb> {
    disk_usage_at(Path::new("/"))
}

/// Size and free space of the filesystem holding `path` (e.g. the data directory on the SD card).
pub fn disk_usage_at(path: &Path) -> Option<DiskGb> {
    let out = Command::new("df")
        .arg("-Pk")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// Read the SoC temperature from a millidegree file.
pub fn read_temperature_c(thermal_zone: &Path) -> Result<f64> {
    let text =
        std::fs::read_to_string(thermal_zone).map_err(|e| PlatformError::io(thermal_zone, e))?;
    let milli: f64 = text.trim().parse().map_err(|_| PlatformError::BadValue {
        source_name: thermal_zone.display().to_string(),
        value: text.clone(),
    })?;
    Ok(milli / 1000.0)
}

/// Read the battery and supply described by `config`.
pub fn read_battery(config: &PowerConfig) -> Result<BatteryStatus> {
    let dir = config
        .battery
        .as_deref()
        .map(Path::new)
        .ok_or(PlatformError::Unavailable("battery"))?;
    let read = |name: &str| read_trimmed(&dir.join(name));
    if !dir.is_dir() {
        return Err(PlatformError::io(
            dir,
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such power_supply"),
        ));
    }
    let status = read("status");
    let external_power = config
        .usb
        .as_deref()
        .and_then(|usb| read_trimmed(&Path::new(usb).join("online")))
        .map(|v| v == "1");
    Ok(BatteryStatus {
        percent: read("capacity")
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v.min(100)),
        charging: status.as_deref() == Some("Charging")
            || external_power == Some(true) && status.as_deref() != Some("Full"),
        status,
        external_power,
        voltage_v: read("voltage_now")
            .and_then(|v| v.parse::<f32>().ok())
            .map(|uv| uv / 1e6),
        // The AXP2202 reports tenths of a degree, the generic power_supply ABI does too.
        temperature_c: read("temp")
            .and_then(|v| v.parse::<f32>().ok())
            .map(|t| t / 10.0),
    })
}

/// Collect the device health summary. Fields that cannot be read are left out.
pub fn read_health(power: Option<&PowerConfig>) -> DeviceHealth {
    let proc = |name: &str| read_trimmed(&Path::new("/proc").join(name));
    DeviceHealth {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        hostname: read_trimmed(Path::new("/proc/sys/kernel/hostname")),
        uptime_s: proc("uptime").as_deref().and_then(parse_uptime),
        load: proc("loadavg").as_deref().and_then(parse_load),
        memory_mb: std::fs::read_to_string("/proc/meminfo")
            .ok()
            .as_deref()
            .and_then(parse_meminfo),
        disk_gb: disk_usage(),
        temperature_c: power
            .and_then(|p| p.thermal_zone.as_deref())
            .and_then(|z| read_temperature_c(Path::new(z)).ok()),
        model: read_trimmed(Path::new("/proc/device-tree/model")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_files() {
        assert_eq!(parse_uptime("12345.67 9999.1\n"), Some(12345));
        assert_eq!(parse_uptime("x"), None);
        assert_eq!(
            parse_load("0.12 0.345 1.0 1/100 42\n"),
            Some([0.12, 0.35, 1.0])
        );
        assert_eq!(parse_load("0.1"), None);
        let mem = "MemTotal:  1022464 kB\nMemFree: 1 kB\nMemAvailable:  512000 kB\n";
        assert_eq!(
            parse_meminfo(mem),
            Some(MemoryMb {
                total: 998,
                available: 500
            })
        );
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn parses_df() {
        let out = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 4000000 1000000 3000000 25% /\n";
        let d = parse_df(out).expect("df");
        assert_eq!(
            d,
            DiskGb {
                total: 4.1,
                free: 3.1
            }
        );
        assert_eq!(parse_df("header only\n"), None);
    }

    fn supply(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).expect("mkdir");
        for (n, v) in files {
            std::fs::write(dir.join(n), format!("{v}\n")).expect("w");
        }
    }

    #[test]
    fn battery_readout() {
        let root = tempfile::tempdir().expect("tempdir");
        let bat = root.path().join("bat");
        let usb = root.path().join("usb");
        supply(
            &bat,
            &[
                ("capacity", "87"),
                ("status", "Charging"),
                ("voltage_now", "4100000"),
                ("temp", "294"),
            ],
        );
        supply(&usb, &[("online", "1")]);
        let cfg = PowerConfig {
            battery: Some(bat.to_string_lossy().into_owned()),
            usb: Some(usb.to_string_lossy().into_owned()),
            ..PowerConfig::default()
        };
        let b = read_battery(&cfg).expect("battery");
        assert_eq!(b.percent, Some(87));
        assert!(b.charging);
        assert_eq!(b.external_power, Some(true));
        assert_eq!(b.temperature_c, Some(29.4));
        assert!((b.voltage_v.expect("v") - 4.1).abs() < 1e-6);
    }

    #[test]
    fn full_on_external_power_is_not_charging() {
        let root = tempfile::tempdir().expect("tempdir");
        let bat = root.path().join("bat");
        let usb = root.path().join("usb");
        supply(&bat, &[("capacity", "100"), ("status", "Full")]);
        supply(&usb, &[("online", "1")]);
        let cfg = PowerConfig {
            battery: Some(bat.to_string_lossy().into_owned()),
            usb: Some(usb.to_string_lossy().into_owned()),
            ..PowerConfig::default()
        };
        let b = read_battery(&cfg).expect("battery");
        assert!(!b.charging);
        assert_eq!(b.external_power, Some(true));
    }

    #[test]
    fn missing_battery_is_unavailable() {
        let err = read_battery(&PowerConfig::default()).expect_err("must fail");
        assert!(err.is_unavailable());
    }

    #[test]
    fn temperature_in_millidegrees() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("temp");
        std::fs::write(&f, "32857\n").expect("w");
        assert!((read_temperature_c(&f).expect("t") - 32.857).abs() < 1e-9);
        std::fs::write(&f, "hot").expect("w");
        assert!(read_temperature_c(&f).is_err());
    }

    #[test]
    fn health_has_version_and_serialises_without_missing_fields() {
        let h = read_health(None);
        assert!(!h.version.is_empty());
        assert!(h.temperature_c.is_none());
    }
}
