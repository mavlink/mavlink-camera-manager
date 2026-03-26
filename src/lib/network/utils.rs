use std::sync::RwLock;

use pnet;
use tracing::*;

use crate::cli::manager::vehicle_ddns;

static OBSERVED_ADDRESS: RwLock<Option<String>> = RwLock::new(None);

/// Store a known-reachable IP learned from an incoming HTTP request.
pub fn set_observed_address(address: String) {
    if let Ok(mut guard) = OBSERVED_ADDRESS.write() {
        *guard = Some(address);
    }
}

/// Priority: (1) CLI `--vehicle-ddns`, (2) IP observed from an HTTP
/// client request, (3) pnet interface detection (legacy fallback).
pub fn get_visible_qgc_address() -> String {
    match vehicle_ddns() {
        Some(ddns) => ddns.to_string(),
        None => {
            if let Some(addr) = OBSERVED_ADDRESS.read().ok().and_then(|g| g.clone()) {
                return addr;
            }
            get_ipv4_addresses()
                .last()
                .unwrap_or(&std::net::Ipv4Addr::UNSPECIFIED)
                .to_string()
        }
    }
}

pub fn get_ipv4_addresses() -> Vec<std::net::Ipv4Addr> {
    // Start with 0.0.0.0
    let mut ips = vec![std::net::Ipv4Addr::UNSPECIFIED];

    let interfaces = pnet::datalink::interfaces();
    let interface = match interfaces
        .iter()
        .find(|e| e.is_up() && !e.is_loopback() && !e.ips.is_empty() && !e.name.contains("docker"))
    {
        Some(interface) => interface,
        None => {
            warn!("Error while finding the default interface.");
            return ips;
        }
    };
    debug!("Found default interface: {interface:#?}");

    interface.ips.iter().for_each(|&ip_network| {
        if let pnet::ipnetwork::IpNetwork::V4(ipv4_network) = ip_network {
            ips.push(ipv4_network.ip());
        }
    });
    debug!("Valid IPs: {ips:#?}");

    ips
}
