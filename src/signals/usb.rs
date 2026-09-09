use crate::event::{Kind, SignalEvent, Source};
use futures_core::Stream;
use nusb::hotplug::HotplugEvent;
use nusb::{DeviceId, DeviceInfo, MaybeFuture};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

pub const VALVE_VID: u16 = 0x28de;

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

pub struct UsbWatch {
    watch: nusb::hotplug::HotplugWatch,
    valve: HashMap<DeviceId, (String, String)>,
}

impl UsbWatch {
    pub fn new() -> anyhow::Result<(Self, Vec<String>)> {
        let devices: Vec<DeviceInfo> = nusb::list_devices().wait()?.collect();
        let mut valve = HashMap::new();
        let mut audit = Vec::new();
        let mut by_hub: HashMap<String, Vec<String>> = HashMap::new();
        for d in devices.iter().filter(|d| d.vendor_id() == VALVE_VID) {
            valve.insert(d.id(), (describe(d), port_label(d)));
            let chain = d.port_chain();
            let hub = format!(
                "{}:{}",
                d.bus_id(),
                chain[..chain.len().saturating_sub(1)].iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
            );
            by_hub.entry(hub).or_default().push(describe(d));
            audit.push(format!("valve usb device: {}", describe(d)));
        }
        for (hub, devs) in &by_hub {
            if devs.len() > 2 {
                audit.push(format!(
                    "WARNING: {} Valve devices share hub {} - known bandwidth pitfall, spread dongles across USB controllers",
                    devs.len(),
                    hub
                ));
            }
        }
        let watch = nusb::watch_devices()?;
        Ok((Self { watch, valve }, audit))
    }

    pub fn poll(&mut self) -> Vec<SignalEvent> {
        let mut out = Vec::new();
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        while let Poll::Ready(Some(ev)) = Pin::new(&mut self.watch).poll_next(&mut cx) {
            match ev {
                HotplugEvent::Connected(d) => {
                    if d.vendor_id() == VALVE_VID {
                        let desc = describe(&d);
                        let port = port_label(&d);
                        self.valve.insert(d.id(), (desc.clone(), port.clone()));
                        out.push(SignalEvent::new(Source::Usb, None, Kind::UsbAttach { port }, desc));
                    }
                }
                HotplugEvent::Disconnected(id) => {
                    if let Some((desc, port)) = self.valve.remove(&id) {
                        out.push(SignalEvent::new(Source::Usb, None, Kind::UsbRemove { port }, desc));
                    }
                }
            }
        }
        out
    }
}
