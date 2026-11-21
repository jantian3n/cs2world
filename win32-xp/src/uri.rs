use crate::models::{gen_node_id, Cs2Node, KcpConfig, ShaperConfig};
use cs2world_proto::uri::{self, Dialect, Kcp, LinkShaper, Node};

fn into_model(node: Node) -> Cs2Node {
    // The Win32 dialect only accepts u32 values in these fields.
    let k = node.kcp;
    let s = node.shaper;
    Cs2Node {
        id: gen_node_id(),
        name: node.name,
        server: node.server,
        server_port: node.server_port,
        psk: node.psk,
        last_a2s: None,
        kcp: KcpConfig {
            data_shards: k.data_shards as u32,
            parity_shards: k.parity_shards as u32,
            mtu: k.mtu as u32,
            snd_wnd: k.snd_wnd as u32,
            rcv_wnd: k.rcv_wnd as u32,
        },
        shaper: ShaperConfig {
            enabled: s.enabled,
            tick_rate_hz: s.tick_rate_hz as u32,
            idle_sleep_seconds: s.idle_sleep_seconds as u32,
            combat_mean_bytes: s.combat_mean_bytes as u32,
            fastdl_chunk_size: s.fastdl_chunk_size as u32,
        },
    }
}
pub fn parse_batch_uris(raw: &str) -> (Vec<Cs2Node>, usize) {
    let batch = uri::parse_batch(raw);
    (
        batch.nodes.into_iter().map(into_model).collect(),
        batch.failed,
    )
}
pub fn encode_node_uri(node: &Cs2Node) -> Result<String, String> {
    let k = &node.kcp;
    let s = &node.shaper;
    let item = Node {
        name: node.name.clone(),
        server: node.server.clone(),
        server_port: node.server_port,
        psk: node.psk.clone(),
        kcp: Kcp {
            data_shards: k.data_shards.into(),
            parity_shards: k.parity_shards.into(),
            mtu: k.mtu.into(),
            snd_wnd: k.snd_wnd.into(),
            rcv_wnd: k.rcv_wnd.into(),
        },
        shaper: LinkShaper {
            enabled: s.enabled,
            tick_rate_hz: s.tick_rate_hz.into(),
            idle_sleep_seconds: s.idle_sleep_seconds.into(),
            combat_mean_bytes: s.combat_mean_bytes.into(),
            fastdl_chunk_size: s.fastdl_chunk_size.into(),
        },
    };
    uri::encode(&item, Dialect::Win32).map_err(|e| e.to_string())
}
