#!/usr/bin/env bash
# CS2WORLD_MANAGED_SCRIPT=1
#
# CS2World (sing-box-cs2world) 原生协议一键部署与管理脚本
#   Version: 0.0.1
#   Protocol: cs2world (1-RTT X25519 PFS + AES-256-GCM + 64-bit Anti-Replay + Valve A2S Defense + AI 64-Tick Shaper)
#   Features:
#     - 单进程原生 sing-box 内核部署（无需外挂端口转发）
#     - 服务端自动注入防 SSRF 私有网段拦截规则（ip_is_private -> block）
#     - 自动调优 Linux UDP 16MB 缓冲区与 BBR，显著提升跨国 UDP/KCP 吞吐
#     - 自动放行 UFW / Firewalld UDP 端口并配置 systemd 守护进程
#     - 一键导出即用型 v2rayN / sing-box 客户端配置（含 TUN/Mixed 双模式）
#     - 内置 Valve Steam A2S_INFO 与未授权主动探测自检工具
#     - 支持免重装快捷修改端口 / PSK / 房间伪装名 / AI 整形参数，校验失败自动回滚

set -Eeuo pipefail

SCRIPT_VERSION="0.0.1"
# 如果你已将 cs2-ghost 仓库托管到 GitHub，可在此填入 "用户名/仓库名"（例如 "yourname/cs2world"），实现全自动免询问安装！
GITHUB_REPO="${CS2WORLD_REPO:-jantian3n/cs2world}"
GO_BUILD_VERSION="1.25.0"
CS2WORLD_CMD="/usr/local/bin/cs2world"
BIN_PATH="/usr/local/bin/sing-box-cs2world"
CONF_DIR="/usr/local/etc/cs2world"
SERVER_CONF="${CONF_DIR}/server.json"
CLIENT_CONF="${CONF_DIR}/client.json"
STATE_FILE="${CONF_DIR}/state.json"
BACKUP_DIR="${CONF_DIR}/backups"
SYSCTL_FILE="/etc/sysctl.d/99-cs2world-udp.conf"
SERVICE_NAME="cs2world"
SERVICE_UNIT="/etc/systemd/system/${SERVICE_NAME}.service"

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log_info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_err()   { echo -e "${RED}[ERR]${NC} $*" >&2; }

require_root() {
    if [[ "${EUID}" -ne 0 ]]; then
        log_err "请使用 root 权限运行此脚本（sudo bash $0）"
        exit 1
    fi
}

detect_arch() {
    local arch
    arch="$(uname -m)"
    case "${arch}" in
        x86_64|amd64) echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        *)
            log_err "不支持的 CPU 架构: ${arch}（仅支持 amd64 / arm64）"
            exit 1
            ;;
    esac
}

ensure_deps() {
    local missing=()
    for cmd in curl jq openssl python3 git tar; do
        command -v "${cmd}" >/dev/null 2>&1 || missing+=("${cmd}")
    done
    if [[ ${#missing[@]} -eq 0 ]]; then
        return 0
    fi
    log_info "正在安装必要依赖: ${missing[*]} ..."
    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -qq
        apt-get install -y -qq "${missing[@]}"
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y -q "${missing[@]}"
    elif command -v yum >/dev/null 2>&1; then
        yum install -y -q "${missing[@]}"
    else
        log_warn "未识别包管理器，请手动安装: ${missing[*]}"
    fi
}

self_install_shortcut() {
    local src_path
    src_path="$(readlink -f "$0" 2>/dev/null || echo "$0")"
    if [[ "${src_path}" != "${CS2WORLD_CMD}" && -f "${src_path}" ]]; then
        cp -f "${src_path}" "${CS2WORLD_CMD}"
        chmod 755 "${CS2WORLD_CMD}"
        log_ok "已安装全局快捷命令：以后可直接输入 ${BOLD}cs2world${NC} 打开管理面板"
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

build_from_github_source() {
    local repo_input="$1"
    local arch="$2"
    local git_url=""

    if [[ "${repo_input}" =~ ^https?:// || "${repo_input}" =~ ^git@ ]]; then
        git_url="${repo_input}"
    else
        git_url="https://github.com/${repo_input}.git"
    fi

    local workdir swap_file=""
    workdir="$(mktemp -d /tmp/cs2world-build.XXXXXX)"
    cleanup_build() {
        if [[ -n "${swap_file}" && -f "${swap_file}" ]]; then
            swapoff "${swap_file}" >/dev/null 2>&1 || true
            rm -f "${swap_file}"
        fi
        rm -rf "${workdir}"
    }
    trap cleanup_build RETURN

    # 1. 小内存 VPS 防 OOM 保护：若总内存(RAM+Swap) < 2048MB，自动创建 2GB 临时 Swap
    local total_mem_mb
    total_mem_mb="$(free -m | awk '/^(Mem|Swap):/{sum+=$2} END{print sum+0}')"
    if [[ "${total_mem_mb}" -lt 2048 ]]; then
        log_warn "检测到 VPS 总内存仅 ${total_mem_mb}MB (<2GB)，正在挂载 2GB 临时编译 Swap 防 OOM..."
        swap_file="${workdir}/build.swap"
        if fallocate -l 2G "${swap_file}" 2>/dev/null || dd if=/dev/zero of="${swap_file}" bs=1M count=2048 status=none; then
            chmod 600 "${swap_file}"
            mkswap "${swap_file}" >/dev/null 2>&1 || true
            swapon "${swap_file}" >/dev/null 2>&1 || true
            log_ok "临时 2GB Swap 挂载成功（编译完成后自动卸载销毁）"
        fi
    fi

    # 2. 准备隔离 Go 编译环境（若系统无 Go 或版本低于 1.24，自动下载官方 Go 到临时目录）
    local go_bin=""
    if command -v go >/dev/null 2>&1; then
        local cur_go_minor
        cur_go_minor="$(go version | sed -nE 's/.*go1\.([0-9]+).*/\1/p')"
        if [[ -n "${cur_go_minor}" && "${cur_go_minor}" -ge 24 ]]; then
            go_bin="$(command -v go)"
            log_info "使用系统现有 Go 编译器: $(go version)"
        fi
    fi

    if [[ -z "${go_bin}" ]]; then
        local go_tar="go${GO_BUILD_VERSION}.linux-${arch}.tar.gz"
        local go_url="https://go.dev/dl/${go_tar}"
        log_info "正在下载临时 Go ${GO_BUILD_VERSION} 编译器 (${go_url}) ..."
        curl -fL --progress-bar "${go_url}" -o "${workdir}/${go_tar}"
        tar -C "${workdir}" -xzf "${workdir}/${go_tar}"
        rm -f "${workdir}/${go_tar}"
        go_bin="${workdir}/go/bin/go"
        log_ok "临时 Go 工具链就绪: $("${go_bin}" version)"
    fi

    # 3. 浅克隆 GitHub 源码仓库
    log_info "正在拉取 GitHub 源码: ${git_url} ..."
    git clone --depth 1 "${git_url}" "${workdir}/src"

    # 自动识别模块目录（支持根目录或 cs2-ghost/ 子目录）
    local mod_dir="${workdir}/src"
    if [[ -f "${workdir}/src/cs2-ghost/go.mod" ]]; then
        mod_dir="${workdir}/src/cs2-ghost"
    elif [[ ! -f "${workdir}/src/go.mod" ]]; then
        log_err "在仓库中未找到 go.mod，请确认仓库包含 cs2-ghost 源码！"
        return 1
    fi

    # 4. 隔离 GOPATH/GOCACHE 现场编译 sing-box-cs2world
    log_info "正在现场编译 sing-box-cs2world v${SCRIPT_VERSION}（首次编译约需 1~2 分钟，请稍候）..."
    (
        cd "${mod_dir}"
        export GOPATH="${workdir}/gopath"
        export GOCACHE="${workdir}/gocache"
        export CGO_ENABLED=0
        "${go_bin}" build \
            -tags "with_gvisor,with_quic,with_utls,with_clash_api" \
            -trimpath \
            -ldflags="-s -w -X 'github.com/sagernet/sing-box/constant.Version=1.14.4-cs2world-${SCRIPT_VERSION}'" \
            -o "${BIN_PATH}.tmp" \
            ./cmd/sing-box-cs2world
    )

    chmod 755 "${BIN_PATH}.tmp"
    if ! "${BIN_PATH}.tmp" version 2>/dev/null | grep -qi "cs2world"; then
        rm -f "${BIN_PATH}.tmp"
        log_err "编译产物校验失败！"
        return 1
    fi
    mv -f "${BIN_PATH}.tmp" "${BIN_PATH}"
    log_ok "现场编译并安装成功: $("${BIN_PATH}" version | head -n 1)"
}

install_binary() {
    local arch bin_url local_candidate
    arch="$(detect_arch)"

    # 1. 自动查找当前目录或常见路径下是否已存在二进制
    for local_candidate in \
        "./sing-box-cs2world-linux-${arch}" \
        "./bin/sing-box-cs2world-linux-${arch}" \
        "./sing-box-cs2world" \
        "/root/sing-box-cs2world-linux-${arch}" \
        "/root/sing-box-cs2world" \
        "${BIN_PATH}"; do
        if [[ -f "${local_candidate}" && -s "${local_candidate}" ]]; then
            if [[ "${local_candidate}" != "${BIN_PATH}" ]]; then
                log_info "检测到本地内核文件: ${local_candidate}，正在安装至 ${BIN_PATH} ..."
                install -m 755 "${local_candidate}" "${BIN_PATH}"
            else
                chmod 755 "${BIN_PATH}"
            fi
            if "${BIN_PATH}" version 2>/dev/null | grep -qi "cs2world"; then
                log_ok "内核校验通过: $("${BIN_PATH}" version | head -n 1)"
                return 0
            fi
        fi
    done

    # 2. 若脚本顶部或环境变量已预设 GITHUB_REPO，直接全自动拉取源码现场编译
    if [[ -n "${GITHUB_REPO}" ]]; then
        log_info "检测到预设 GitHub 仓库 (${GITHUB_REPO})，自动启动 VPS 现场源码编译..."
        build_from_github_source "${GITHUB_REPO}" "${arch}"
        return 0
    fi

    # 3. 交互选择安装方式（默认推荐方式 1：GitHub 源码自动编译）
    echo ""
    log_warn "未检测到本地 sing-box-cs2world-linux-${arch}，请选择自动获取方式："
    echo -e "  ${GREEN}1.${NC} 从 GitHub 仓库自动拉取源码并现场编译 ${BOLD}(懒人推荐，自动装临时 Go 并清理缓存)${NC}"
    echo -e "  ${GREEN}2.${NC} 从 GitHub Release 极速下载预编译二进制"
    echo -e "  ${GREEN}3.${NC} 输入自定义二进制直链 URL"
    read -rp "请选择 [1-3] (默认 1): " install_mode
    install_mode="${install_mode:-1}"

    case "${install_mode}" in
        1)
            local repo_in
            read -rp "请输入你的 GitHub 仓库 (例如 yourname/cs2world 或完整 https://github.com/...): " repo_in
            if [[ -z "${repo_in}" ]]; then
                log_err "仓库地址不能为空！"
                exit 1
            fi
            build_from_github_source "${repo_in}" "${arch}"
            ;;
        2)
            local repo_in rel_url
            read -rp "请输入你的 GitHub 仓库 (例如 yourname/cs2world): " repo_in
            if [[ -z "${repo_in}" ]]; then
                log_err "仓库地址不能为空！"
                exit 1
            fi
            rel_url="https://github.com/${repo_in}/releases/latest/download/sing-box-cs2world-linux-${arch}"
            log_info "正在从 Release 下载: ${rel_url} ..."
            curl -fL --progress-bar "${rel_url}" -o "${BIN_PATH}.tmp"
            chmod 755 "${BIN_PATH}.tmp"
            if ! "${BIN_PATH}.tmp" version 2>/dev/null | grep -qi "cs2world"; then
                rm -f "${BIN_PATH}.tmp"
                log_err "下载的文件校验失败，请确认该仓库已发布 Release！"
                exit 1
            fi
            mv -f "${BIN_PATH}.tmp" "${BIN_PATH}"
            log_ok "内核下载并校验通过: $("${BIN_PATH}" version | head -n 1)"
            ;;
        3)
            read -rp "请输入内核下载 URL: " bin_url
            if [[ -z "${bin_url}" ]]; then
                log_err "未提供内核 URL，安装中止。"
                exit 1
            fi
            log_info "正在下载 ${bin_url} ..."
            curl -fL --progress-bar "${bin_url}" -o "${BIN_PATH}.tmp"
            chmod 755 "${BIN_PATH}.tmp"
            if ! "${BIN_PATH}.tmp" version 2>/dev/null | grep -qi "cs2world"; then
                rm -f "${BIN_PATH}.tmp"
                log_err "下载的文件不是有效的 sing-box-cs2world 可执行文件！"
                exit 1
            fi
            mv -f "${BIN_PATH}.tmp" "${BIN_PATH}"
            log_ok "内核下载并校验通过: $("${BIN_PATH}" version | head -n 1)"
            ;;
        *)
            log_err "无效选择。"
            exit 1
            ;;
    esac
}

tune_udp_sysctl() {
    log_info "正在优化 Linux 内核 UDP 缓冲区与 BBR 拥塞控制 (${SYSCTL_FILE}) ..."
    cat > "${SYSCTL_FILE}" <<'EOF'
# CS2World UDP & KCP High-Throughput Tuning
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.core.rmem_default = 2097152
net.core.wmem_default = 2097152
net.core.netdev_max_backlog = 16384
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
EOF
    sysctl --system >/dev/null 2>&1 || sysctl -p "${SYSCTL_FILE}" >/dev/null 2>&1 || true
    log_ok "内核 UDP 缓冲区（16MB）与 BBR 优化已生效"
}

open_firewall_udp() {
    local port="$1"
    if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
        ufw allow "${port}/udp" comment "cs2world" >/dev/null 2>&1 || true
        log_ok "已在 UFW 放行 UDP ${port}"
    elif command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld 2>/dev/null; then
        firewall-cmd --permanent --add-port="${port}/udp" >/dev/null 2>&1 || true
        firewall-cmd --reload >/dev/null 2>&1 || true
        log_ok "已在 Firewalld 放行 UDP ${port}"
    else
        log_warn "未检测到活跃的 UFW/Firewalld，如使用云厂商安全组请手动放行 UDP ${port}"
    fi
}

backup_configs() {
    mkdir -p "${BACKUP_DIR}"
    local ts
    ts="$(date +%Y%m%d-%H%M%S)"
    if [[ -f "${SERVER_CONF}" ]]; then
        cp -f "${SERVER_CONF}" "${BACKUP_DIR}/server-${ts}.json"
    fi
    if [[ -f "${STATE_FILE}" ]]; then
        cp -f "${STATE_FILE}" "${BACKUP_DIR}/state-${ts}.json"
    fi
    # 仅保留最近 10 份备份
    ls -1t "${BACKUP_DIR}"/server-*.json 2>/dev/null | tail -n +11 | xargs -r rm -f
}

render_configs() {
    local port="$1"
    local psk="$2"
    local srv_name="$3"
    local tick_hz="$4"
    local shaper_enabled="$5"
    local fec_data="$6"
    local fec_parity="$7"
    local pub_ip="$8"

    mkdir -p "${CONF_DIR}"
    chmod 700 "${CONF_DIR}"

    # 1. 生成服务端配置（包含防 SSRF 私有地址拦截规则）
    jq -n \
        --argjson port "${port}" \
        --arg psk "${psk}" \
        --arg srv_name "${srv_name}" \
        --argjson tick_hz "${tick_hz}" \
        --argjson shaper_enabled "${shaper_enabled}" \
        --argjson fec_data "${fec_data}" \
        --argjson fec_parity "${fec_parity}" \
        '{
          log: { level: "info", timestamp: true },
          inbounds: [
            {
              type: "cs2world",
              tag: "cs2world-in",
              listen: "::",
              listen_port: $port,
              psk: $psk,
              kcp: {
                data_shards: $fec_data,
                parity_shards: $fec_parity,
                mtu: 1180,
                snd_wnd: 1024,
                rcv_wnd: 1024
              },
              a2s: {
                server_name: $srv_name,
                maps: ["de_mirage", "de_inferno", "de_dust2", "de_ancient", "de_nuke"],
                max_players: 20,
                game_version: "1.40.4.2",
                map_cycle_minutes: 30
              },
              shaper: {
                enabled: $shaper_enabled,
                tick_rate_hz: $tick_hz,
                idle_sleep_seconds: 600,
                combat_mean_bytes: 240,
                fastdl_chunk_size: 1248
              }
            }
          ],
          outbounds: [
            { type: "direct", tag: "direct" },
            { type: "block", tag: "block" }
          ],
          route: {
            rules: [
              { ip_is_private: true, outbound: "block" }
            ],
            final: "direct"
          }
        }' > "${SERVER_CONF}.new"

    chmod 600 "${SERVER_CONF}.new"

    # 校验新配置合法性
    if ! "${BIN_PATH}" check -c "${SERVER_CONF}.new" >/dev/null 2>&1; then
        log_err "服务端配置校验失败！详细错误："
        "${BIN_PATH}" check -c "${SERVER_CONF}.new" || true
        rm -f "${SERVER_CONF}.new"
        return 1
    fi
    mv -f "${SERVER_CONF}.new" "${SERVER_CONF}"

    # 2. 生成开箱即用的客户端配置（适配 v2rayN / Nekoray / macOS / Linux）
    jq -n \
        --arg pub_ip "${pub_ip}" \
        --argjson port "${port}" \
        --arg psk "${psk}" \
        --argjson tick_hz "${tick_hz}" \
        --argjson shaper_enabled "${shaper_enabled}" \
        --argjson fec_data "${fec_data}" \
        --argjson fec_parity "${fec_parity}" \
        '{
          log: { level: "info", timestamp: true },
          inbounds: [
            {
              type: "mixed",
              tag: "mixed-in",
              listen: "127.0.0.1",
              listen_port: 7890
            }
          ],
          outbounds: [
            {
              type: "cs2world",
              tag: "cs2world-out",
              server: $pub_ip,
              server_port: $port,
              psk: $psk,
              kcp: {
                data_shards: $fec_data,
                parity_shards: $fec_parity,
                mtu: 1180,
                snd_wnd: 1024,
                rcv_wnd: 1024
              },
              shaper: {
                enabled: $shaper_enabled,
                tick_rate_hz: $tick_hz,
                idle_sleep_seconds: 600,
                combat_mean_bytes: 180,
                fastdl_chunk_size: 1248
              }
            },
            { type: "direct", tag: "direct" }
          ],
          route: {
            rules: [
              { ip_is_private: true, outbound: "direct" }
            ],
            final: "cs2world-out"
          }
        }' > "${CLIENT_CONF}"
    chmod 600 "${CLIENT_CONF}"

    # 3. 保存状态元数据
    jq -n \
        --arg ver "${SCRIPT_VERSION}" \
        --arg pub_ip "${pub_ip}" \
        --argjson port "${port}" \
        --arg psk "${psk}" \
        --arg srv_name "${srv_name}" \
        --argjson tick_hz "${tick_hz}" \
        --argjson shaper_enabled "${shaper_enabled}" \
        --argjson fec_data "${fec_data}" \
        --argjson fec_parity "${fec_parity}" \
        '{
          version: $ver,
          public_ip: $pub_ip,
          port: $port,
          psk: $psk,
          server_name: $srv_name,
          tick_rate_hz: $tick_hz,
          shaper_enabled: $shaper_enabled,
          fec_data: $fec_data,
          fec_parity: $fec_parity
        }' > "${STATE_FILE}"
    chmod 600 "${STATE_FILE}"
}

install_systemd_unit() {
    cat > "${SERVICE_UNIT}" <<EOF
[Unit]
Description=CS2World (sing-box-cs2world v${SCRIPT_VERSION}) Native Protocol Service
Documentation=https://github.com/SagerNet/sing-box
After=network-online.target nss-lookup.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${BIN_PATH} run -c ${SERVER_CONF}
Restart=on-failure
RestartSec=3s
LimitNOFILE=1048576
AmbientCapabilities=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now "${SERVICE_NAME}"
    sleep 1
    if systemctl is-active --quiet "${SERVICE_NAME}"; then
        log_ok "systemd 服务 ${SERVICE_NAME} 已启动并设为开机自启！"
    else
        log_err "服务启动失败，请检查日志：journalctl -u ${SERVICE_NAME} -n 50 --no-pager"
        exit 1
    fi
}

do_install() {
    require_root
    ensure_deps
    self_install_shortcut
    install_binary

    local def_port="27015"
    local def_psk=""
    local def_name="[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick"
    local def_tick="64"
    local def_shaper="true"
    local def_fec_d="10"
    local def_fec_p="3"

    # 若重装，默认沿用旧端口与 PSK
    if [[ -f "${STATE_FILE}" ]]; then
        def_port="$(jq -r '.port // 27015' "${STATE_FILE}")"
        def_psk="$(jq -r '.psk // ""' "${STATE_FILE}")"
        def_name="$(jq -r '.server_name // "[Asia/HK] CS2 5v5 Competitive Practice #3 | 128Tick"' "${STATE_FILE}")"
    fi
    [[ -z "${def_psk}" ]] && def_psk="$(gen_psk)"

    echo ""
    echo -e "${BOLD}=== CS2World v${SCRIPT_VERSION} 服务端部署向导 ===${NC}"
    read -rp "1. UDP 监听端口 (推荐 27015~27050，默认 ${def_port}): " in_port
    local port="${in_port:-${def_port}}"

    read -rp "2. CS2 伪装房间名称 (默认 ${def_name}): " in_name
    local srv_name="${in_name:-${def_name}}"

    read -rp "3. 预共享密钥 PSK (回车使用高强度随机密钥): " in_psk
    local psk="${in_psk:-${def_psk}}"

    read -rp "4. 开启 AI 64-Tick 流量整形与心跳？[Y/n] (默认 Y): " in_shaper
    local shaper_enabled="true"
    [[ "${in_shaper,,}" == "n" ]] && shaper_enabled="false"

    read -rp "5. 开启 KCP 抗丢包 FEC 纠错 (10:3)？[Y/n] (晚高峰弱网推荐 Y): " in_fec
    local fec_d="${def_fec_d}" fec_p="${def_fec_p}"
    if [[ "${in_fec,,}" == "n" ]]; then
        fec_d=0
        fec_p=0
    fi

    local pub_ip
    pub_ip="$(detect_public_ip)"
    read -rp "6. 服务端公网 IP / 域名 (自动探测: ${pub_ip}，回车确认): " in_ip
    pub_ip="${in_ip:-${pub_ip}}"

    backup_configs
    tune_udp_sysctl
    render_configs "${port}" "${psk}" "${srv_name}" "${def_tick}" "${shaper_enabled}" "${fec_d}" "${fec_p}" "${pub_ip}"
    open_firewall_udp "${port}"
    install_systemd_unit

    echo ""
    show_client_config
    echo ""
    run_probe_check
}

show_client_config() {
    if [[ ! -f "${CLIENT_CONF}" ]]; then
        log_warn "尚未找到客户端配置文件，请先执行部署。"
        return 1
    fi
    echo -e "${BOLD}${GREEN}================ 客户端配置 (client.json) ================${NC}"
    echo -e "保存路径: ${CYAN}${CLIENT_CONF}${NC}"
    echo -e "用法 1 (v2rayN): 替换 bin/sing_box/sing-box.exe -> 导入下方自定义配置"
    echo -e "用法 2 (命令行): ./sing-box-cs2world run -c client.json"
    echo -e "${CYAN}----------------------------------------------------------${NC}"
    cat "${CLIENT_CONF}"
    echo -e "${CYAN}----------------------------------------------------------${NC}"
}

run_probe_check() {
    if [[ ! -f "${STATE_FILE}" ]]; then
        log_warn "未检测到已部署状态，无法自检。"
        return 1
    fi
    local port
    port="$(jq -r '.port' "${STATE_FILE}")"
    log_info "正在向本机 UDP 127.0.0.1:${port} 发送真实 Steam A2S_INFO 与非法探针自检..."

    python3 - "${port}" <<'PYEOF'
import os, socket, struct, sys

port = int(sys.argv[1])
target = ("127.0.0.1", port)

def read_cstr(d, off):
    end = d.index(b"\x00", off)
    return d[off:end].decode("utf-8", "replace"), end + 1

try:
    # 1. A2S_INFO Probe
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(2.0)
    s.sendto(b"\xff\xff\xff\xffTSource Engine Query\x00", target)
    data, _ = s.recvfrom(2048)
    s.close()
    assert data[:5] == b"\xff\xff\xff\xffI"
    srv_name, off = read_cstr(data, 6)
    map_name, off = read_cstr(data, off)
    _, off = read_cstr(data, off)
    game, off = read_cstr(data, off)
    app_id = struct.unpack_from("<H", data, off)[0]
    players, max_p = data[off+2], data[off+3]
    print(f"\033[0;32m[PASS]\033[0m A2S_INFO 主动探测伪装正常: {game} (AppID={app_id}) | 地图={map_name} | 在线={players}/{max_p}")
    print(f"       房间名: {srv_name}")

    # 2. Unauthorized GNS Probe
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(2.0)
    s.sendto(b"\x82\xde\xad\xbe\xef" + os.urandom(40), target)
    resp, _ = s.recvfrom(2048)
    s.close()
    assert resp[0] == 0x02
    reason = resp[9:].rstrip(b"\x00").decode("utf-8", "replace")
    print(f"\033[0;32m[PASS]\033[0m 非法探测防御正常: 返回 Valve GNS 0x02 -> {reason!r}")
except Exception as e:
    print(f"\033[0;31m[FAIL]\033[0m 自检失败: {e}")
    sys.exit(1)
PYEOF
}

do_modify() {
    require_root
    if [[ ! -f "${STATE_FILE}" ]]; then
        log_err "尚未部署，请先执行安装。"
        return 1
    fi
    local port psk srv_name tick_hz shaper_enabled fec_d fec_p pub_ip
    port="$(jq -r '.port' "${STATE_FILE}")"
    psk="$(jq -r '.psk' "${STATE_FILE}")"
    srv_name="$(jq -r '.server_name' "${STATE_FILE}")"
    tick_hz="$(jq -r '.tick_rate_hz' "${STATE_FILE}")"
    shaper_enabled="$(jq -r '.shaper_enabled' "${STATE_FILE}")"
    fec_d="$(jq -r '.fec_data' "${STATE_FILE}")"
    fec_p="$(jq -r '.fec_parity' "${STATE_FILE}")"
    pub_ip="$(jq -r '.public_ip' "${STATE_FILE}")"

    echo ""
    echo -e "${BOLD}=== 快捷修改参数（回车保持当前值） ===${NC}"
    echo -e "  1) 修改监听端口 (当前: ${port})"
    echo -e "  2) 轮换高强度随机 PSK"
    echo -e "  3) 修改 CS2 房间伪装名 (当前: ${srv_name})"
    echo -e "  4) 切换 AI 64-Tick 流量整形 (当前: ${shaper_enabled})"
    echo -e "  0) 返回主菜单"
    read -rp "请选择: " m_choice

    case "${m_choice}" in
        1)
            read -rp "请输入新的 UDP 端口: " new_port
            [[ -n "${new_port}" ]] && port="${new_port}"
            ;;
        2)
            psk="$(gen_psk)"
            log_ok "已生成全新 64 位随机 PSK"
            ;;
        3)
            read -rp "请输入新的 CS2 伪装房间名: " new_name
            [[ -n "${new_name}" ]] && srv_name="${new_name}"
            ;;
        4)
            if [[ "${shaper_enabled}" == "true" ]]; then
                shaper_enabled="false"
            else
                shaper_enabled="true"
            fi
            log_ok "AI 流量整形已切换为: ${shaper_enabled}"
            ;;
        0) return 0 ;;
        *) log_warn "无效选择"; return 0 ;;
    esac

    backup_configs
    if render_configs "${port}" "${psk}" "${srv_name}" "${tick_hz}" "${shaper_enabled}" "${fec_d}" "${fec_p}" "${pub_ip}"; then
        open_firewall_udp "${port}"
        systemctl restart "${SERVICE_NAME}"
        log_ok "配置已热更新并重启生效！请同步更新客户端配置："
        show_client_config
    else
        log_err "修改失败，已保留原配置不受影响。"
    fi
}

show_status() {
    echo ""
    echo -e "${BOLD}=== CS2World 运行状态 ===${NC}"
    if [[ -x "${BIN_PATH}" ]]; then
        echo -e "内核版本: ${GREEN}$("${BIN_PATH}" version | head -n 1)${NC}"
    else
        echo -e "内核版本: ${RED}未安装${NC}"
    fi
    if systemctl is-active --quiet "${SERVICE_NAME}" 2>/dev/null; then
        echo -e "服务状态: ${GREEN}运行中 (active)${NC}"
    else
        echo -e "服务状态: ${RED}未运行${NC}"
    fi
    if [[ -f "${STATE_FILE}" ]]; then
        echo -e "监听端口: UDP ${CYAN}$(jq -r '.port' "${STATE_FILE}")${NC}"
        echo -e "公网地址: ${CYAN}$(jq -r '.public_ip' "${STATE_FILE}")${NC}"
        echo -e "伪装房间: $(jq -r '.server_name' "${STATE_FILE}")"
        echo -e "AI 整形 : $(jq -r '.shaper_enabled' "${STATE_FILE}") ($(jq -r '.tick_rate_hz' "${STATE_FILE}") Hz)"
    fi
}

show_logs() {
    journalctl -u "${SERVICE_NAME}" -n 80 --no-pager || true
}

do_uninstall() {
    require_root
    read -rp "确定要彻底卸载 CS2World 及其配置与内核优化？[y/N]: " confirm
    [[ "${confirm,,}" != "y" ]] && return 0

    systemctl disable --now "${SERVICE_NAME}" 2>/dev/null || true
    rm -f "${SERVICE_UNIT}"
    systemctl daemon-reload
    rm -f "${BIN_PATH}" "${CS2WORLD_CMD}" "${SYSCTL_FILE}"
    rm -rf "${CONF_DIR}"
    sysctl --system >/dev/null 2>&1 || true
    log_ok "CS2World 已彻底卸载清理完毕（如有自定义防火墙规则请按需清理）。"
}

show_menu() {
    while true; do
        echo ""
        echo -e "${BOLD}${CYAN}====================================================${NC}"
        echo -e "${BOLD}   CS2World (sing-box 原生 CS2 协议) 管理脚本 v${SCRIPT_VERSION}${NC}"
        echo -e "${BOLD}${CYAN}====================================================${NC}"
        echo -e "  ${GREEN}1.${NC} 一键部署 / 重装 CS2World 服务端"
        echo -e "  ${GREEN}2.${NC} 查看运行状态与监听信息"
        echo -e "  ${GREEN}3.${NC} 查看 / 导出客户端配置 (client.json)"
        echo -e "  ${GREEN}4.${NC} 执行 Valve A2S_INFO 与主动探测防伪自检"
        echo -e "  ${GREEN}5.${NC} 快捷修改配置 (端口 / 轮换 PSK / 房间名 / AI 整形)"
        echo -e "  ${GREEN}6.${NC} 重启服务"
        echo -e "  ${GREEN}7.${NC} 查看最近 80 行实时日志"
        echo -e "  ${RED}8.${NC} 彻底卸载 CS2World"
        echo -e "  ${YELLOW}0.${NC} 退出"
        echo -e "${CYAN}----------------------------------------------------${NC}"
        read -rp "请输入选项 [0-8]: " choice
        case "${choice}" in
            1) do_install ;;
            2) show_status ;;
            3) show_client_config ;;
            4) run_probe_check ;;
            5) do_modify ;;
            6) require_root && systemctl restart "${SERVICE_NAME}" && log_ok "已重启 ${SERVICE_NAME}" ;;
            7) show_logs ;;
            8) do_uninstall; exit 0 ;;
            0) exit 0 ;;
            *) log_warn "无效输入，请输入 0-8" ;;
        esac
    done
}

# 支持命令行子命令直达（如 cs2world status / cs2world probe）
case "${1:-}" in
    install)   do_install ;;
    status)    show_status ;;
    client)    show_client_config ;;
    probe)     run_probe_check ;;
    modify)    do_modify ;;
    restart)   require_root && systemctl restart "${SERVICE_NAME}" && log_ok "已重启 ${SERVICE_NAME}" ;;
    log|logs)  show_logs ;;
    uninstall) do_uninstall ;;
    ""|menu)   show_menu ;;
    *)
        echo "用法: cs2world [install|status|client|probe|modify|restart|log|uninstall]"
        exit 1
        ;;
esac
