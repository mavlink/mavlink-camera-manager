use pnet;
use tracing::*;

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

    return ips;
}
