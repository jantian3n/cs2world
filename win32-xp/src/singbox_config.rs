use crate::models::{AppSettings, Cs2Node};
use cs2world_core::config::Config;
use serde_json::json;
use std::io;

pub fn core_config(node: &Cs2Node, s: &AppSettings) -> io::Result<Config> {
    let (rules, final_tag) = match s.routing_mode.as_str() {
        "direct" => (vec![], "direct"),
        "global" => (
            vec![json!({"ip_is_private":true,"outbound":"direct"})],
            "proxy",
        ),
        "rule" => (
            vec![
                json!({"ip_is_private":true,"outbound":"direct"}),
                json!({"domain_suffix":[".cn",".local",".lan"],"outbound":"direct"}),
            ],
            "proxy",
        ),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "未知分流模式")),
    };
    if !matches!(s.proxy_mode.as_str(), "system" | "tun" | "manual") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "未知代理方式"));
    }
    let config = json!({
        "inbounds":[{"type":"mixed","listen":"127.0.0.1","listen_port":s.mixed_port}],
        "outbounds":[{"type":"cs2world","tag":"proxy","server":node.server,"server_port":node.server_port,"psk":node.psk,"kcp":node.kcp,"shaper":node.shaper},{"type":"direct","tag":"direct"}],
        "route":{"rules":rules,"final":final_tag}
    });
    Config::from_json(&config.to_string())
}
