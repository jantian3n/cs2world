#!/usr/bin/env bash
# CS2WORLD_MANAGED_SCRIPT=2
#
# CS2World Rust Server (v0.2.0) 一键部署、平滑升级与管理脚本
#   Repository: https://github.com/jantian3n/cs2world
#   Features:
#     - 纯 Rust 单文件 cs2world-server（零 GC 抖动、内嵌真实 CS2 64Hz 经验分布整形 + Valve GNS 0x24 拒绝响应）
#     - 自动检测旧版 Go (sing-box-cs2world) 配置并原地无缝继承端口、PSK、房间名与 KCP 参数
#     - 优先拉取 GitHub Releases 预编译二进制，缺失时自动安装 Rust 工具链源码编译
#     - 自动调优 Linux UDP 16MB 缓冲区与 BBR、放行防火墙 UDP 端口并注册 systemd 服务（支持 SIGHUP 热重载）
#     - 内置 Valve A2S_INFO 与真实 CS2 (0x20 -> 0x24) 主动连接防御双项自检
#     - 一键生成标准 cs2world:// 分享链接（兼容 CS2World-XP 与 v2rayN）

set -Eeuo pipefail
trap 'echo -e "\033[0;31m[ERR]\033[0m 脚本在第 ${LINENO} 行执行失败: ${BASH_COMMAND}" >&2' ERR

SCRIPT_VERSION="0.2.0"
GITHUB_REPO="${CS2WORLD_REPO:-jantian3n/cs2world}"
GIT_REF="${CS2WORLD_REF:-main}"
CS2WORLD_CMD="/usr/local/bin/cs2world"
BIN_PATH="/usr/local/bin/cs2world-server"
CONF_DIR="/usr/local/etc/cs2world"
SERVER_CONF="${CONF_DIR}/server.json"
SHARE_URI_FILE="${CONF_DIR}/share.uri"
BACKUP_DIR="${CONF_DIR}/backups"
SYSCTL_FILE="/etc/sysctl.d/99-cs2world-udp.conf"
SERVICE_NAME="cs2world"
SERVICE_UNIT="/etc/systemd/system/${SERVICE_NAME}.service"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log_info() { echo -e "${CYAN}[INFO]${NC} $*"; }
log_ok()   { echo -e "${GREEN}[OK]${NC} $*"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_err()  { echo -e "${RED}[ERR]${NC} $*" >&2; }

require_root() {
    if [[ "${EUID}" -ne 0 ]]; then
        log_err "请使用 root 权限运行此脚本（sudo bash $0）"
        exit 1
    fi
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        *)
            log_err "不支持的 CPU 架构: $(uname -m)（仅支持 amd64 / arm64）"
            exit 1
            ;;
    esac
}

ensure_deps() {
    local missing=()
    for cmd in curl jq openssl python3 git tar gcc; do
        command -v "${cmd}" >/dev/null 2>&1 || missing+=("${cmd}")
    done
    if [[ ${#missing[@]} -eq 0 ]]; then
        return 0
    fi
    log_info "正在安装必要依赖: ${missing[*]} ..."
    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -qq
        apt-get install -y -qq "${missing[@]}" ca-certificates pkg-config build-essential
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y -q "${missing[@]}" ca-certificates pkgconfig
    elif command -v yum >/dev/null 2>&1; then
        yum install -y -q "${missing[@]}" ca-certificates pkgconfig
    else
        log_warn "未识别包管理器，请手动确认依赖已安装: ${missing[*]}"
    fi
}

self_install_shortcut() {
    local src_path
    src_path="$(readlink -f "$0" 2>/dev/null || echo "$0")"
    if [[ -f "${src_path}" && "${src_path}" != /dev/fd/* ]]; then
        if [[ "${src_path}" != "${CS2WORLD_CMD}" ]]; then
            cp -f "${src_path}" "${CS2WORLD_CMD}"
            chmod 755 "${CS2WORLD_CMD}"
            log_ok "已更新全局快捷命令：以后可直接输入 ${BOLD}cs2world${NC} 打开管理面板"
        fi
    else
        if curl -fsSL "https://raw.githubusercontent.com/${GITHUB_REPO}/${GIT_REF}/scripts/cs2world-deploy-v0.2.0.sh" -o "${CS2WORLD_CMD}.tmp" 2>/dev/null; then
            chmod 755 "${CS2WORLD_CMD}.tmp"
            mv -f "${CS2WORLD_CMD}.tmp" "${CS2WORLD_CMD}"
            log_ok "已安装全局快捷命令：以后可直接输入 ${BOLD}cs2world${NC} 打开管理面板"
        fi
    fi
}

detect_public_ip() {
    local ip=""
    for url in "https://api.ipify.org" "https://ifconfig.me/ip" "https://icanhazip.com"; do
        ip="$(curl -4 -fsS --max-time 4 "${url}" 2>/dev/null | tr -d '[:space:]' || true)"
        if [[ "${ip}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            echo "${ip}"
            return 0
        fi
    done
    echo "YOUR_VPS_IP"
}

gen_psk() {
    openssl rand -hex 32
}

install_binary() {
    local arch="$1"
    local tmp_bin="/tmp/cs2world-server.new.$$"
    rm -f "${tmp_bin}"

    # 1) 优先尝试下载与当前脚本版本严格匹配的预编译二进制（不拉取可能陈旧的 latest）
    local release_urls=(
        "https://github.com/${GITHUB_REPO}/releases/download/v${SCRIPT_VERSION}/cs2world-server-linux-${arch}"
    )
    for url in "${release_urls[@]}"; do
        log_info "尝试下载预编译 Rust 服务端: ${url}"
        if curl -fsSL --max-time 30 "${url}" -o "${tmp_bin}" 2>/dev/null && [[ -s "${tmp_bin}" ]]; then
            chmod 755 "${tmp_bin}"
            if "${tmp_bin}" version >/dev/null 2>&1; then
                mv -f "${tmp_bin}" "${BIN_PATH}"
                log_ok "已安装预编译二进制: $(${BIN_PATH} version)"
                return 0
            fi
        fi
        rm -f "${tmp_bin}"
    done

    # 2) 回退：自动安装 Rust 工具链并从 GitHub 源码编译 cs2world-server
    log_warn "未检测到预编译 Release 资产，将从 GitHub (${GITHUB_REPO}@${GIT_REF}) 源码编译纯 Rust 服务端..."
    if ! command -v cargo >/dev/null 2>&1; then
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        log_info "正在安装 Rust 工具链 (rustup)..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
        export PATH="$HOME/.cargo/bin:$PATH"
    fi

    local build_dir="/tmp/cs2world-build.$$"
    rm -rf "${build_dir}"
    git clone --depth 1 --branch "${GIT_REF}" "https://github.com/${GITHUB_REPO}.git" "${build_dir}"
    log_info "开始编译 cs2world-server (release)，首次编译约需 1-3 分钟..."
    (cd "${build_dir}" && cargo build --release -p cs2world-server)
    install -m 755 "${build_dir}/target/release/cs2world-server" "${BIN_PATH}"
    rm -rf "${build_dir}"
    log_ok "源码编译并安装完成: $(${BIN_PATH} version)"
}

tune_kernel_udp() {
    cat > "${SYSCTL_FILE}" <<'EOF'
# CS2World Rust Server UDP & BBR tuning
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.core.rmem_default = 4194304
net.core.wmem_default = 4194304
net.core.netdev_max_backlog = 16384
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
EOF
    sysctl --system >/dev/null 2>&1 || sysctl -p "${SYSCTL_FILE}" >/dev/null 2>&1 || true
    log_ok "已应用 Linux UDP 16MB 缓冲区与 BBR 优化"
}

open_firewall_udp() {
    local port="$1"
    if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -qi "active"; then
        ufw allow "${port}/udp" >/dev/null 2>&1 || true
        log_ok "UFW 已放行 ${port}/udp"
    elif command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
        firewall-cmd --permanent --add-port="${port}/udp" >/dev/null 2>&1 || true
        firewall-cmd --reload >/dev/null 2>&1 || true
        log_ok "firewalld 已放行 ${port}/udp"
    fi
}

write_server_config() {
    local port="$1" psk="$2" server_name="$3" data_shards="$4" parity_shards="$5" mtu="$6" shaper_enabled="$7"
    mkdir -p "${CONF_DIR}" "${BACKUP_DIR}"
    if [[ -f "${SERVER_CONF}" ]]; then
        cp -f "${SERVER_CONF}" "${BACKUP_DIR}/server.$(date +%Y%m%d-%H%M%S).json"
    fi
    cat > "${SERVER_CONF}" <<EOF
{
  "log": {
    "level": "info",
    "timestamp": true
  },
  "inbounds": [
    {
      "type": "cs2world",
      "tag": "cs2world-in",
      "listen": "0.0.0.0",
      "listen_port": ${port},
      "psk": "${psk}",
      "kcp": {
        "data_shards": ${data_shards},
        "parity_shards": ${parity_shards},
        "mtu": ${mtu},
        "snd_wnd": 1024,
        "rcv_wnd": 1024
      },
      "a2s": {
        "server_name": "${server_name}",
        "maps": [
          "de_mirage",
          "de_inferno",
          "de_dust2",
          "de_ancient",
          "de_nuke"
        ],
        "max_players": 20,
        "game_version": "1.40.4.2",
        "map_cycle_minutes": 30
      },
      "shaper": {
        "enabled": ${shaper_enabled},
        "tick_rate_hz": 64,
        "idle_sleep_seconds": 600,
        "combat_mean_bytes": 720,
        "fastdl_chunk_size": 1257
      }
    }
  ],
  "outbounds": [
    {
      "type": "direct",
      "tag": "direct"
    }
  ],
  "route": {
    "rules": [
      {
        "ip_is_private": true,
        "action": "reject"
      }
    ],
    "final": "direct"
  }
}
EOF
    chmod 600 "${SERVER_CONF}"
    "${BIN_PATH}" check -c "${SERVER_CONF}"
}

export_share_uri() {
    local ip="$1"
    python3 - "${SERVER_CONF}" "${ip}" "${SHARE_URI_FILE}" <<'PY'
import json, sys, urllib.parse
conf_path, ip, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
cfg = json.load(open(conf_path, 'r', encoding='utf-8'))
inb = cfg['inbounds'][0]
kcp = inb.get('kcp', {})
sh = inb.get('shaper', {})
a2s = inb.get('a2s', {})
params = {
    'ds': kcp.get('data_shards', 10),
    'ps': kcp.get('parity_shards', 3),
    'mtu': kcp.get('mtu', 1180),
    'sw': kcp.get('snd_wnd', 1024),
    'rw': kcp.get('rcv_wnd', 1024),
    'shaper': 1 if sh.get('enabled', True) else 0,
    'tick': sh.get('tick_rate_hz', 64),
    'idle': sh.get('idle_sleep_seconds', 600),
    'mean': sh.get('combat_mean_bytes', 720),
    'fastdl': sh.get('fastdl_chunk_size', 1257),
}
q = urllib.parse.urlencode(params)
tag = urllib.parse.quote(a2s.get('server_name', 'CS2World'), safe='')
uri = f"cs2world://{urllib.parse.quote(inb['psk'], safe='')}@{ip}:{inb['listen_port']}?{q}#{tag}"
open(out_path, 'w', encoding='utf-8').write(uri + '\n')
print(uri)
PY
}

write_systemd_unit() {
    # 兼容清理可能冲突的旧 cs2world-rust 或旧版 Go sing-box-cs2world 进程
    if systemctl is-active --quiet cs2world-rust 2>/dev/null; then
        systemctl stop cs2world-rust || true
        systemctl disable cs2world-rust || true
    fi
    pkill -x sing-box-cs2world 2>/dev/null || true
    cat > "${SERVICE_UNIT}" <<EOF
[Unit]
Description=CS2World Rust Server (v${SCRIPT_VERSION})
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${BIN_PATH} run -c ${SERVER_CONF}
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=2
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable "${SERVICE_NAME}" >/dev/null 2>&1
    systemctl restart "${SERVICE_NAME}"
    sleep 1
    if systemctl is-active --quiet "${SERVICE_NAME}"; then
        log_ok "systemd 服务 ${SERVICE_NAME} 已启动并设为开机自启"
    else
        log_err "服务启动失败，请查看日志: journalctl -u ${SERVICE_NAME} -n 50 --no-pager"
        exit 1
    fi
}

run_self_test() {
    if [[ ! -f "${SERVER_CONF}" ]]; then
        log_err "未找到配置文件 ${SERVER_CONF}"
        return 1
    fi
    local port
    port="$(jq -r '.inbounds[0].listen_port' "${SERVER_CONF}")"
    log_info "正在对 127.0.0.1:${port} 执行 A2S_INFO + Valve GNS (0x20 -> 0x24) 双项自检..."
    python3 - "${port}" <<'PY'
import socket, struct, sys
port = int(sys.argv[1])
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(3.0)

# 1) A2S_INFO query
a2s_req = b'\xff\xff\xff\xffTSource Engine Query\x00'
s.sendto(a2s_req, ('127.0.0.1', port))
resp, _ = s.recvfrom(2048)
if len(resp) >= 5 and resp[:4] == b'\xff\xff\xff\xff' and resp[4] == 0x41:
    # Challenge response -> resend with 4-byte challenge
    s.sendto(a2s_req + resp[5:9], ('127.0.0.1', port))
    resp, _ = s.recvfrom(2048)
assert len(resp) > 10 and resp[:5] == b'\xff\xff\xff\xffI', f'A2S_INFO 响应异常: {resp[:16]!r}'
name = resp[6:].split(b'\x00', 1)[0].decode('utf-8', errors='replace')
print(f'[OK] A2S_INFO 伪装正常: 房间名 = {name}')

# 2) Real CS2 SteamNetSockets 0x20 ChallengeRequest -> expect 0x24 ConnectionClosed echoing conn_id
cid = 0x20260202
gns_req = bytes([0x20, 0x0d]) + struct.pack('<I', cid) + bytes(506)
s.sendto(gns_req, ('127.0.0.1', port))
resp2, _ = s.recvfrom(2048)
assert len(resp2) >= 6 and resp2[0] == 0x24, f'GNS 拒绝帧首字节异常: 0x{resp2[0]:02x} (期望 0x24)'
assert resp2[1] == 0x25 and struct.unpack('<I', resp2[2:6])[0] == cid, 'GNS 0x24 未正确回显 to_connection_id'
print(f'[OK] Valve GNS 0x24 拒绝防御正常: 首字节=0x24, 回显 conn_id=0x{cid:08x}, 长度={len(resp2)}B')
PY
}

install_or_upgrade() {
    require_root
    ensure_deps
    self_install_shortcut
    local arch
    arch="$(detect_arch)"

    # 自动检测旧版配置（Go v0.0.1 或旧 Rust），实现零痛感继承
    local def_port="27015"
    local def_psk=""
    local def_name="[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick"
    local def_ds="10"
    local def_ps="3"
    local def_mtu="1180"
    local def_shaper="true"

    if [[ -f "${SERVER_CONF}" ]]; then
        log_ok "检测到已有配置 ${SERVER_CONF}，将默认继承原端口、PSK 与房间名（客户端无需重导链接）"
        def_port="$(jq -r '.inbounds[0].listen_port // 27015' "${SERVER_CONF}" 2>/dev/null || echo 27015)"
        def_psk="$(jq -r '.inbounds[0].psk // ""' "${SERVER_CONF}" 2>/dev/null || echo "")"
        def_name="$(jq -r '.inbounds[0].a2s.server_name // "[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick"' "${SERVER_CONF}" 2>/dev/null || echo "${def_name}")"
        def_ds="$(jq -r '.inbounds[0].kcp.data_shards // 10' "${SERVER_CONF}" 2>/dev/null || echo 10)"
        def_ps="$(jq -r '.inbounds[0].kcp.parity_shards // 3' "${SERVER_CONF}" 2>/dev/null || echo 3)"
        def_mtu="$(jq -r '.inbounds[0].kcp.mtu // 1180' "${SERVER_CONF}" 2>/dev/null || echo 1180)"
        def_shaper="$(jq -r 'if .inbounds[0].shaper.enabled == false then "false" else "true" end' "${SERVER_CONF}" 2>/dev/null || echo true)"
    fi
    [[ -n "${def_psk}" ]] || def_psk="$(gen_psk)"

    local port="${def_port}" psk="${def_psk}" server_name="${def_name}"
    if [[ -t 0 ]]; then
        read -rp "监听 UDP 端口 [${def_port}]: " in_port || true
        port="${in_port:-${def_port}}"
        read -rp "预共享密钥 PSK [回车保留当前/随机生成]: " in_psk || true
        psk="${in_psk:-${def_psk}}"
        read -rp "A2S 房间伪装名 [${def_name}]: " in_name || true
        server_name="${in_name:-${def_name}}"
    fi

    install_binary "${arch}"
    tune_kernel_udp
    write_server_config "${port}" "${psk}" "${server_name}" "${def_ds}" "${def_ps}" "${def_mtu}" "${def_shaper}"
    open_firewall_udp "${port}"
    write_systemd_unit
    run_self_test

    local pub_ip uri
    pub_ip="$(detect_public_ip)"
    uri="$(export_share_uri "${pub_ip}")"
    echo ""
    echo -e "${GREEN}${BOLD}================ CS2World Rust Server v${SCRIPT_VERSION} 部署完成 ================${NC}"
    echo -e "内核版本 : $(${BIN_PATH} version)"
    echo -e "监听地址 : ${BOLD}${pub_ip}:${port}/udp${NC}"
    echo -e "分享链接 : ${CYAN}${uri}${NC}"
    echo -e "管理命令 : 随时输入 ${BOLD}cs2world${NC} 打开管理面板"
    echo -e "${GREEN}========================================================================${NC}"
}

uninstall_all() {
    require_root
    systemctl stop "${SERVICE_NAME}" 2>/dev/null || true
    systemctl disable "${SERVICE_NAME}" 2>/dev/null || true
    rm -f "${SERVICE_UNIT}" "${BIN_PATH}" "${SYSCTL_FILE}" "${CS2WORLD_CMD}"
    systemctl daemon-reload
    log_ok "已卸载服务与二进制（配置目录 ${CONF_DIR} 已保留，如需彻底清除请手动 rm -rf ${CONF_DIR}）"
}

show_menu() {
    echo ""
    echo -e "${BOLD}===== CS2World Rust 服务端管理面板 (v${SCRIPT_VERSION}) =====${NC}"
    echo "  1) 安装 / 从 Go v0.0.1 平滑升级到纯 Rust v${SCRIPT_VERSION}"
    echo "  2) 查看服务状态与实时日志"
    echo "  3) 显示 cs2world:// 分享链接"
    echo "  4) 执行 A2S + Valve GNS 0x24 双项自检"
    echo "  5) 重启服务"
    echo "  6) 卸载服务"
    echo "  0) 退出"
    read -rp "请选择 [0-6]: " choice || exit 0
    case "${choice}" in
        1) install_or_upgrade ;;
        2) systemctl status "${SERVICE_NAME}" --no-pager; journalctl -u "${SERVICE_NAME}" -n 30 --no-pager ;;
        3) [[ -f "${SHARE_URI_FILE}" ]] && cat "${SHARE_URI_FILE}" || export_share_uri "$(detect_public_ip)" ;;
        4) run_self_test ;;
        5) systemctl restart "${SERVICE_NAME}" && log_ok "已重启 ${SERVICE_NAME}" ;;
        6) uninstall_all ;;
        0) exit 0 ;;
        *) log_warn "无效选项" ;;
    esac
}

case "${1:-}" in
    install|upgrade) install_or_upgrade ;;
    test)            run_self_test ;;
    uri)             [[ -f "${SHARE_URI_FILE}" ]] && cat "${SHARE_URI_FILE}" || export_share_uri "$(detect_public_ip)" ;;
    uninstall)       uninstall_all ;;
    *)
        if [[ ! -x "${BIN_PATH}" || ! -t 0 ]]; then
            install_or_upgrade
        else
            show_menu
        fi
        ;;
esac
