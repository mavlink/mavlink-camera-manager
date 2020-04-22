pub fn get_valid_ip_address() -> Vec<std::net::IpAddr> {
    let mut ips = Vec::new();
    for interface in pnet::datalink::interfaces() {
        for interface_ip in interface.ips {
            if !interface_ip.is_ipv4() {
                continue;
            }
            ips.push(interface_ip.ip());
        }
    }
    return ips;
}
