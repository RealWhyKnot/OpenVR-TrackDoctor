use crate::event::{Kind, SignalEvent, Source};
use futures_core::Stream;
use nusb::hotplug::HotplugEvent;
use nusb::{DeviceId, DeviceInfo, MaybeFuture};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

pub const VALVE_VID: u16 = 0x28de;
pub const CROWDED: usize = 3;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UsbDongle {
    pub serial: Option<String>,
    pub product: String,
    pub controller: String,
    pub bus: String,
    pub ports: Vec<u8>,
    pub hubs: Vec<String>,
}

fn port_label(d: &DeviceInfo) -> String {
    let chain: Vec<String> = d.port_chain().iter().map(|p| p.to_string()).collect();
    format!("{}:{}", d.bus_id(), chain.join("."))
}

fn describe(d: &DeviceInfo) -> String {
    format!(
        "{:04x}:{:04x} {} sn={} port={}",
        d.vendor_id(),
        d.product_id(),
        d.product_string().unwrap_or("?"),
        d.serial_number().unwrap_or("?"),
        port_label(d)
    )
}

pub fn vendor_name(vid: u16) -> Option<&'static str> {
    Some(match vid {
        0x1022 => "AMD",
        0x8086 => "Intel",
        0x1912 => "Renesas",
        0x1b21 => "ASMedia",
        0x1106 => "VIA",
        0x104c => "Texas Instruments",
        0x1b73 => "Fresco Logic",
        0x10de => "NVIDIA",
        0x1d6a => "Aquantia",
        0x1b6f => "Etron",
        _ => return None,
    })
}

pub fn controller_label(parent_instance: &str, bus: &str) -> String {
    let vendor = parent_instance
        .to_ascii_uppercase()
        .split("VEN_")
        .nth(1)
        .and_then(|s| u16::from_str_radix(s.get(..4)?, 16).ok())
        .map(|v| vendor_name(v).map_or(format!("vendor {v:04X}"), str::to_string));
    let slots: Vec<&str> = bus
        .split('#')
        .filter_map(|s| s.strip_prefix("PCI(")?.strip_suffix(')'))
        .collect();
    let mut out = match vendor {
        Some(v) => format!("{v} USB controller"),
        None => "USB controller".to_string(),
    };
    if !slots.is_empty() {
        out.push_str(&format!(" (PCI {})", slots.join(".")));
    }
    out
}

#[cfg(windows)]
fn bus_labels() -> HashMap<String, String> {
    nusb::list_buses()
        .wait()
        .map(|buses| {
            buses
                .map(|b| {
                    let parent = b.parent_instance_id().to_string_lossy().into_owned();
                    let label = controller_label(&parent, b.bus_id());
                    (b.bus_id().to_string(), label)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(windows))]
fn bus_labels() -> HashMap<String, String> {
    nusb::list_buses()
        .wait()
        .map(|buses| {
            buses
                .map(|b| {
                    let name = b.system_name().unwrap_or("USB controller").to_string();
                    (b.bus_id().to_string(), name)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn hub_label(d: &DeviceInfo) -> String {
    match d.product_string() {
        Some(p) if !p.is_empty() => {
            format!("{p} {:04X}:{:04X}", d.vendor_id(), d.product_id())
        }
        _ => format!("USB hub {:04X}:{:04X}", d.vendor_id(), d.product_id()),
    }
}

pub fn snapshot() -> Vec<UsbDongle> {
    let Ok(devices) = nusb::list_devices().wait() else {
        return Vec::new();
    };
    let devices: Vec<DeviceInfo> = devices.collect();
    let labels = bus_labels();
    let mut out: Vec<UsbDongle> = devices
        .iter()
        .filter(|d| d.vendor_id() == VALVE_VID)
        .map(|d| {
            let chain = d.port_chain();
            let hubs = (1..chain.len())
                .map(|n| {
                    devices
                        .iter()
                        .find(|h| h.bus_id() == d.bus_id() && h.port_chain() == &chain[..n])
                        .map_or_else(|| "hub".to_string(), hub_label)
                })
                .collect();
            UsbDongle {
                serial: d.serial_number().map(str::to_string),
                product: d.product_string().unwrap_or("Valve USB device").to_string(),
                controller: labels
                    .get(d.bus_id())
                    .cloned()
                    .unwrap_or_else(|| controller_label("", d.bus_id())),
                bus: d.bus_id().to_string(),
                ports: chain.to_vec(),
                hubs,
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.controller, &a.bus, &a.ports).cmp(&(&b.controller, &b.bus, &b.ports)));
    out
}

pub fn crowding(dongles: &[UsbDongle]) -> Vec<String> {
    let mut by_hub: HashMap<(String, Vec<u8>), (usize, String)> = HashMap::new();
    for d in dongles {
        let parent = d.ports[..d.ports.len().saturating_sub(1)].to_vec();
        let where_ = d
            .hubs
            .last()
            .map_or(d.controller.clone(), |h| format!("{h} on {}", d.controller));
        by_hub
            .entry((d.bus.clone(), parent))
            .or_insert((0, where_))
            .0 += 1;
    }
    let mut out: Vec<String> = by_hub
        .into_values()
        .filter(|(n, _)| *n >= CROWDED)
        .map(|(n, where_)| {
            format!(
                "WARNING: {n} Valve USB devices share {where_}. If their trackers drop together, move some dongles to another USB controller."
            )
        })
        .collect();
    out.sort();
    out
}

#[derive(Clone)]
struct Dongle {
    desc: String,
    port: String,
    serial: Option<String>,
}

fn dongle_of(d: &DeviceInfo) -> Dongle {
    Dongle {
        desc: describe(d),
        port: port_label(d),
        serial: d.serial_number().map(str::to_string),
    }
}

pub struct UsbWatch {
    watch: nusb::hotplug::HotplugWatch,
    valve: HashMap<DeviceId, Dongle>,
}

impl UsbWatch {
    pub fn new() -> anyhow::Result<Self> {
        let watch = nusb::watch_devices()?;
        let valve = nusb::list_devices()
            .wait()?
            .filter(|d| d.vendor_id() == VALVE_VID)
            .map(|d| (d.id(), dongle_of(&d)))
            .collect();
        Ok(Self { watch, valve })
    }

    pub fn poll(&mut self) -> Vec<SignalEvent> {
        let mut out = Vec::new();
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        while let Poll::Ready(Some(ev)) = Pin::new(&mut self.watch).poll_next(&mut cx) {
            match ev {
                HotplugEvent::Connected(d) => {
                    if d.vendor_id() == VALVE_VID {
                        let dongle = dongle_of(&d);
                        out.push(SignalEvent::new(
                            Source::Usb,
                            None,
                            Kind::UsbAttach {
                                port: dongle.port.clone(),
                                dongle: dongle.serial.clone(),
                            },
                            dongle.desc.clone(),
                        ));
                        self.valve.insert(d.id(), dongle);
                    }
                }
                HotplugEvent::Disconnected(id) => {
                    if let Some(d) = self.valve.remove(&id) {
                        out.push(SignalEvent::new(
                            Source::Usb,
                            None,
                            Kind::UsbRemove {
                                port: d.port,
                                dongle: d.serial,
                            },
                            d.desc,
                        ));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dongle(serial: &str, ports: &[u8], hubs: &[&str]) -> UsbDongle {
        UsbDongle {
            serial: Some(serial.into()),
            product: "Watchman Dongle".into(),
            controller: "AMD USB controller (PCI 0801.0003)".into(),
            bus: "PCIROOT(0)#PCI(0801)#PCI(0003)".into(),
            ports: ports.to_vec(),
            hubs: hubs.iter().map(|h| h.to_string()).collect(),
        }
    }

    #[test]
    fn controller_label_names_vendor_and_slot() {
        assert_eq!(
            controller_label(
                r"PCI\VEN_1022&DEV_149C&SUBSYS_7C911462&REV_00\4&2ee6fba0&0&0341",
                "PCIROOT(0)#PCI(0801)#PCI(0003)"
            ),
            "AMD USB controller (PCI 0801.0003)"
        );
        assert_eq!(
            controller_label(r"PCI\VEN_ABCD&DEV_0001", "PCIROOT(0)#PCI(0102)"),
            "vendor ABCD USB controller (PCI 0102)"
        );
        assert_eq!(controller_label("", "bus7"), "USB controller");
    }

    #[test]
    fn crowding_counts_devices_per_hub() {
        let three_on_root = vec![
            dongle("A", &[1], &[]),
            dongle("B", &[3], &[]),
            dongle("C", &[4], &[]),
            dongle("D", &[5, 1], &["Generic USB Hub 045B:0209"]),
            dongle("E", &[5, 2], &["Generic USB Hub 045B:0209"]),
        ];
        let w = crowding(&three_on_root);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].starts_with(
                "WARNING: 3 Valve USB devices share AMD USB controller (PCI 0801.0003)."
            ),
            "{w:?}"
        );
        assert!(crowding(&three_on_root[3..]).is_empty());
    }
}
