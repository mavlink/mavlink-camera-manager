use log::*;
use pnet;

pub fn get_ipv4_addresses() -> Vec<std::net::Ipv4Addr> {
    // Start with 0.0.0.0
    let mut ips = vec![std::net::Ipv4Addr::UNSPECIFIED];

    for interface in pnet::datalink::interfaces() {
        debug!("Checking interface {:#?}", interface);

        // We could run inside a docker..
        // but QGC should be running in an ground station computer
        if interface.name.contains("docker") {
            continue;
        }

        for interface_ip in interface.ips {
            match interface_ip {
                pnet::ipnetwork::IpNetwork::V4(interface_ip) => {
                    let ip = interface_ip.ip();
                    if ip.is_loopback() {
                        continue;
                    }

                    ips.push(ip);
                }
                _ => continue,
            }
        }
    }
    debug!("Valid IPs: {:#?}", ips);
    return ips;
}
