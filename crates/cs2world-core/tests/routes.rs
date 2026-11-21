use cs2world_core::{
    config::Config,
    route::{Action, Network, Target},
};
#[test]
fn route_precedence_network_suffix_cidr_and_private_addresses() {
    let client=Config::from_json(r#"{
        "inbounds":[{"type":"mixed"}],
        "outbounds":[{"type":"cs2world","tag":"node","server":"localhost","server_port":27015,"psk":"test"},
                     {"type":"direct","tag":"lan"},{"type":"block","tag":"deny"}],
        "route":{"final":"node","rules":[
            {"domain":"exception.cn","network":"udp","outbound":"deny"},
            {"domain_suffix":[".cn",".lan"],"outbound":"lan"},
            {"ip_cidr":["192.0.2.0/24"],"outbound":"deny"},
            {"ip_is_private":true,"outbound":"lan"}
        ]}}"#).unwrap();
    let Config::Client(client) = client else {
        panic!("expected client")
    };
    let verdict = |host: &str, network| {
        client.routes.action(
            &Target {
                host: host.into(),
                port: 443,
            },
            network,
        )
    };
    assert_eq!(verdict("exception.cn", Network::Udp), Action::Block);
    assert_eq!(verdict("EXCEPTION.CN.", Network::Tcp), Action::Direct);
    assert_eq!(verdict("printer.lan", Network::Tcp), Action::Direct);
    assert_eq!(verdict("badcn", Network::Tcp), Action::Proxy);
    assert_eq!(verdict("192.0.2.17", Network::Tcp), Action::Block);
    for ip in [
        "10.0.0.9",
        "::ffff:192.168.3.1",
        "fd00::42",
        "fe80::1",
        "127.0.0.1",
    ] {
        assert_eq!(verdict(ip, Network::Udp), Action::Direct);
    }
}
