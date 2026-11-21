use crate::route::{Action, Network, RouteTable, Rule};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;

pub use cs2world_proto::{a2s::Config as A2sConfig, shaper::Config as ShaperConfig};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct KcpConfig {
    pub data_shards: usize,
    pub parity_shards: usize,
    pub mtu: usize,
    pub snd_wnd: usize,
    pub rcv_wnd: usize,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            data_shards: 0,
            parity_shards: 0,
            mtu: 1180,
            snd_wnd: 1024,
            rcv_wnd: 1024,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Node {
    pub server: String,
    pub server_port: u16,
    pub psk: String,
    pub kcp: KcpConfig,
    pub shaper: ShaperConfig,
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("server_port", &self.server_port)
            .field("kcp", &self.kcp)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub psk: String,
    pub kcp: KcpConfig,
    pub shaper: ShaperConfig,
    pub a2s: A2sConfig,
    pub routes: RouteTable,
}
#[derive(Clone)]
pub struct ClientConfig {
    pub listen: SocketAddr,
    pub node: Option<Node>,
    pub routes: RouteTable,
}
#[derive(Clone)]
pub enum Config {
    Client(ClientConfig),
    Server(ServerConfig),
}

fn invalid(path: &str, reason: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{path}: {reason}"))
}

fn fields(v: &Value, allowed: &[&str], path: &str) -> io::Result<()> {
    let obj = v
        .as_object()
        .ok_or_else(|| invalid(path, "expected object"))?;
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(invalid(&format!("{path}.{k}"), "unsupported field"));
        }
    }
    Ok(())
}

fn as_type<T: DeserializeOwned>(v: &Value, path: &str) -> io::Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| invalid(path, "invalid field type or value"))
}

fn optional<T: Default + DeserializeOwned>(v: &Value, key: &str, path: &str) -> io::Result<T> {
    v.get(key)
        .map(|s| as_type(s, &format!("{path}.{key}")))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn list(v: &Value, path: &str) -> io::Result<Vec<String>> {
    if v.is_null() {
        Ok(vec![])
    } else if let Some(item) = v.as_str() {
        Ok(vec![item.into()])
    } else {
        as_type(v, path)
    }
}

fn secret(v: &Value, path: &str) -> io::Result<String> {
    v["psk"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(&format!("{path}.psk"), "nonempty string required"))
}

fn kcp(v: &Value, p: &str) -> io::Result<KcpConfig> {
    let path = format!("{p}.kcp");
    let Some(obj) = v.get("kcp") else {
        return Ok(KcpConfig::default());
    };
    fields(
        obj,
        &["data_shards", "parity_shards", "mtu", "snd_wnd", "rcv_wnd"],
        &path,
    )?;
    let num = |key: &str, default: usize| -> io::Result<usize> {
        let n: i64 = optional(obj, key, &path)?;
        Ok(if n <= 0 {
            default
        } else {
            usize::try_from(n).map_err(|_| invalid(&path, "integer out of range"))?
        })
    };
    let mut k = KcpConfig {
        data_shards: num("data_shards", 0)?,
        parity_shards: num("parity_shards", 0)?,
        mtu: num("mtu", 1180)?,
        snd_wnd: num("snd_wnd", 1024)?,
        rcv_wnd: num("rcv_wnd", 1024)?,
    };
    if k.data_shards == 0 || k.parity_shards == 0 {
        k.data_shards = 0;
        k.parity_shards = 0;
    }
    if k.data_shards.saturating_add(k.parity_shards) > 256 {
        return Err(invalid(&path, "FEC supports at most 256 shards"));
    }
    if k.mtu <= 24 + if k.data_shards != 0 { 8 } else { 0 } {
        return Err(invalid(&format!("{path}.mtu"), "too small for KCP headers"));
    }
    k.mtu = k.mtu.min(1472);
    if k.snd_wnd > 65535 || k.rcv_wnd > 65535 {
        return Err(invalid(&path, "KCP windows must fit in 16 bits"));
    }
    Ok(k)
}

fn shaper(v: &Value, p: &str) -> io::Result<ShaperConfig> {
    if let Some(s) = v.get("shaper") {
        fields(
            s,
            &[
                "enabled",
                "tick_rate_hz",
                "idle_sleep_seconds",
                "combat_mean_bytes",
                "fastdl_chunk_size",
            ],
            &format!("{p}.shaper"),
        )?;
    }
    optional(v, "shaper", p)
}

fn routes(v: &Value, tags: &HashMap<String, Action>, fallback: Action) -> io::Result<RouteTable> {
    if v.is_null() {
        return Ok(RouteTable {
            rules: vec![],
            fallback,
        });
    }
    fields(v, &["rules", "final", "auto_detect_interface"], "route")?;
    if let Some(det) = v.get("auto_detect_interface") {
        let _: bool = as_type(det, "route.auto_detect_interface")?;
    }
    let named = |v: &Value, p: &str| {
        tags.get(v.as_str().unwrap_or(""))
            .copied()
            .ok_or_else(|| invalid(p, "unknown outbound tag"))
    };
    let fallback = v
        .get("final")
        .map(|x| named(x, "route.final"))
        .transpose()?
        .unwrap_or(fallback);
    let mut rules = Vec::new();
    let raw: Vec<Value> = optional(v, "rules", "route")?;
    for (i, r) in raw.iter().enumerate() {
        let path = format!("route.rules[{i}]");
        fields(
            r,
            &[
                "action",
                "outbound",
                "network",
                "domain",
                "domain_suffix",
                "ip_cidr",
                "ip_is_private",
            ],
            &path,
        )?;
        if r["action"] == "sniff" && r.as_object().is_some_and(|s| s.len() == 1) {
            continue;
        }
        if r.get("action").is_some_and(|a| !a.is_string()) {
            return Err(invalid(&format!("{path}.action"), "expected string"));
        }
        let action = match r["action"].as_str() {
            Some("reject" | "block") => Action::Block,
            Some("route") | None => named(&r["outbound"], &format!("{path}.outbound"))?,
            _ => {
                return Err(invalid(
                    &format!("{path}.action"),
                    "unsupported route action",
                ))
            }
        };
        let mut networks = vec![];
        for n in list(&r["network"], &format!("{path}.network"))? {
            networks.push(match n.as_str() {
                "tcp" => Network::Tcp,
                "udp" => Network::Udp,
                _ => return Err(invalid(&format!("{path}.network"), "expected tcp or udp")),
            });
        }
        let cidrs = list(&r["ip_cidr"], &format!("{path}.ip_cidr"))?
            .into_iter()
            .map(|s| {
                s.parse()
                    .map_err(|_| invalid(&format!("{path}.ip_cidr"), "invalid CIDR"))
            })
            .collect::<io::Result<_>>()?;
        let private: bool = optional(r, "ip_is_private", &path)?;
        if r.get("ip_is_private").is_some() && !private {
            return Err(invalid(&format!("{path}.ip_is_private"), "expected true"));
        }
        rules.push(Rule {
            networks,
            domains: list(&r["domain"], &path)?,
            suffixes: list(&r["domain_suffix"], &path)?,
            cidrs,
            private,
            action,
        });
    }
    Ok(RouteTable { rules, fallback })
}

impl Config {
    pub fn from_input(input: &str) -> io::Result<Self> {
        let input = input.trim();
        if !input
            .get(..cs2world_proto::uri::PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(cs2world_proto::uri::PREFIX))
        {
            return Self::from_json(input);
        }
        let node = cs2world_proto::uri::parse(input, cs2world_proto::uri::Dialect::Win32)
            .map_err(|_| invalid("uri", "invalid CS2World sharing link"))?;
        let config = serde_json::json!({
            "inbounds":[{"type":"mixed","listen":"127.0.0.1","listen_port":17890}],
            "outbounds":[{"type":"cs2world","tag":"proxy","server":node.server,
                "server_port":node.server_port,"psk":node.psk,"kcp":node.kcp,"shaper":node.shaper},
                {"type":"direct","tag":"direct"}],
            "route":{"rules":[{"ip_is_private":true,"outbound":"direct"}],"final":"proxy"}
        });
        Self::from_json(&config.to_string())
    }

    pub fn from_json(s: &str) -> io::Result<Self> {
        let v: Value = serde_json::from_str(s).map_err(|e| invalid("$", e))?;
        fields(
            &v,
            &["inbounds", "outbounds", "log", "route", "experimental"],
            "$",
        )?;
        if let Some(log) = v.get("log") {
            fields(log, &["level", "timestamp", "disabled"], "log")?;
        }
        if let Some(exp) = v.get("experimental") {
            fields(exp, &["clash_api"], "experimental")?;
            if let Some(clash) = exp.get("clash_api") {
                fields(clash, &["external_controller"], "experimental.clash_api")?;
            }
        }
        let ins = v["inbounds"]
            .as_array()
            .ok_or_else(|| invalid("inbounds", "expected array"))?;
        if ins.len() != 1 {
            return Err(invalid(
                "inbounds",
                "exactly one mixed or cs2world listener is required",
            ));
        }
        let input = &ins[0];
        let server = match input["type"].as_str() {
            Some("mixed") => false,
            Some("cs2world") => true,
            _ => return Err(invalid("inbounds[0].type", "expected mixed or cs2world")),
        };
        let p = "inbounds[0]";
        if server {
            fields(
                input,
                &[
                    "type",
                    "tag",
                    "listen",
                    "listen_port",
                    "psk",
                    "kcp",
                    "shaper",
                    "a2s",
                ],
                p,
            )?;
        } else {
            fields(input, &["type", "tag", "listen", "listen_port"], p)?;
        }
        let host = match input.get("listen") {
            Some(h) => h
                .as_str()
                .ok_or_else(|| invalid("inbounds[0].listen", "expected IP string"))?,
            None if server => "0.0.0.0",
            None => "127.0.0.1",
        };
        let port = input
            .get("listen_port")
            .map(|x| as_type(x, "inbounds[0].listen_port"))
            .transpose()?
            .unwrap_or(if server { 27015 } else { 17890 });
        let listen = SocketAddr::new(
            host.parse()
                .map_err(|_| invalid("inbounds[0].listen", "expected IP address"))?,
            port,
        );
        let outs = v["outbounds"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| invalid("outbounds", "expected at least one outbound"))?;
        let mut tags = HashMap::new();
        let mut node = None;
        let mut fallback = Action::Direct;
        for (i, o) in outs.iter().enumerate() {
            let p = format!("outbounds[{i}]");
            let kind = o["type"]
                .as_str()
                .ok_or_else(|| invalid(&format!("{p}.type"), "required"))?;
            let action = match kind {
                "cs2world" if !server => {
                    fields(
                        o,
                        &[
                            "type",
                            "tag",
                            "server",
                            "server_port",
                            "psk",
                            "kcp",
                            "shaper",
                        ],
                        &p,
                    )?;
                    if node.is_some() {
                        return Err(invalid(&p, "only one cs2world outbound is supported"));
                    }
                    let server = o["server"]
                        .as_str()
                        .filter(|h| !h.trim().is_empty())
                        .ok_or_else(|| invalid(&format!("{p}.server"), "missing hostname"))?
                        .to_owned();
                    let server_port: u16 = as_type(&o["server_port"], &format!("{p}.server_port"))?;
                    if server_port == 0 {
                        return Err(invalid(
                            &format!("{p}.server_port"),
                            "nonzero port required",
                        ));
                    }
                    node = Some(Node {
                        server,
                        server_port,
                        psk: secret(o, &p)?,
                        kcp: kcp(o, &p)?,
                        shaper: shaper(o, &p)?,
                    });
                    Action::Proxy
                }
                "direct" | "block" => {
                    fields(o, &["type", "tag"], &p)?;
                    if kind == "direct" {
                        Action::Direct
                    } else {
                        Action::Block
                    }
                }
                _ => return Err(invalid(&format!("{p}.type"), "unsupported outbound")),
            };
            if i == 0 {
                fallback = action;
            }
            if tags
                .insert(o["tag"].as_str().unwrap_or(kind).to_string(), action)
                .is_some()
            {
                return Err(invalid(&format!("{p}.tag"), "duplicate outbound tag"));
            }
        }
        let routes = routes(&v["route"], &tags, fallback)?;
        if server {
            if let Some(a) = input.get("a2s") {
                fields(
                    a,
                    &[
                        "server_name",
                        "maps",
                        "max_players",
                        "game_version",
                        "map_cycle_minutes",
                    ],
                    "inbounds[0].a2s",
                )?;
            }
            Ok(Self::Server(ServerConfig {
                listen,
                psk: secret(input, p)?,
                kcp: kcp(input, p)?,
                shaper: shaper(input, p)?,
                a2s: optional(input, "a2s", p)?,
                routes,
            }))
        } else {
            Ok(Self::Client(ClientConfig {
                listen,
                node,
                routes,
            }))
        }
    }
}
