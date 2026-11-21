use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KcpConfig {
    pub data_shards: u32,
    pub parity_shards: u32,
    pub mtu: u32,
    pub snd_wnd: u32,
    pub rcv_wnd: u32,
}

impl Default for KcpConfig {
    fn default() -> Self {
        Self {
            data_shards: 10,
            parity_shards: 3,
            mtu: 1180,
            snd_wnd: 1024,
            rcv_wnd: 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShaperConfig {
    pub enabled: bool,
    pub tick_rate_hz: u32,
    pub idle_sleep_seconds: u32,
    pub combat_mean_bytes: u32,
    pub fastdl_chunk_size: u32,
}

impl Default for ShaperConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tick_rate_hz: 64,
            idle_sleep_seconds: 600,
            combat_mean_bytes: 180,
            fastdl_chunk_size: 1248,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2SProbeResult {
    pub ok: bool,
    pub game: String,
    pub app_id: u16,
    pub server_name: String,
    pub map_name: String,
    pub players: u8,
    pub max_players: u8,
    pub rtt_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cs2Node {
    pub id: String,
    pub name: String,
    pub server: String,
    pub server_port: u16,
    pub psk: String,
    #[serde(default)]
    pub kcp: KcpConfig,
    #[serde(default)]
    pub shaper: ShaperConfig,
    #[serde(default)]
    pub last_a2s: Option<A2SProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub active_node_id: Option<String>,
    pub routing_mode: String, // "rule" | "global" | "direct"
    pub proxy_mode: String,   // "system" | "tun"
    pub mixed_port: u16,
    pub clash_port: u16,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            active_node_id: None,
            routing_mode: "rule".to_string(),
            proxy_mode: "system".to_string(),
            mixed_port: 17890,
            clash_port: 19090,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedState {
    pub nodes: Vec<Cs2Node>,
    pub settings: AppSettings,
}

#[derive(Debug, Clone, Default)]
pub struct TrafficSnapshot {
    pub up_bps: u64,
    pub down_bps: u64,
    pub up_total: u64,
    pub down_total: u64,
    pub active_connections: usize,
}

pub fn gen_node_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("node-{ts:x}-{seq}")
}
