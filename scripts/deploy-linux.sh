#!/usr/bin/env bash
# Run on the destination Linux system. install takes an existing local binary and configuration.
#   sudo bash scripts/deploy-linux.sh install ./cs2world-server ./server.json
#   sudo bash scripts/deploy-linux.sh rollback
set -euo pipefail

base=/opt/cs2world-rust
unit=cs2world-rust.service
account=cs2world-rust
config_group=cs2world-config

usage() {
    echo "usage: $0 install BINARY CONFIG | rollback" >&2
    exit 2
}

link_release() {
    local name=$1 target=$2
    local link_tmp
    link_tmp=$(mktemp -d "$base/.link.XXXXXXXX")
    ln -s "$target" "$link_tmp/link"
    mv -Tf "$link_tmp/link" "$base/$name"
    rmdir "$link_tmp"
}

activate() {
    local target=$1 previous
    previous=$(readlink "$base/current" || true)
    link_release current "$target"
    if systemctl restart "$unit" && sleep 1 && systemctl is-active --quiet "$unit"; then
        if [[ -n "$previous" && "$previous" != "$target" ]]; then
            link_release previous "$previous"
        fi
        systemctl enable "$unit"
        printf 'Active release: %s\n' "$target"
        return 0
    fi
    echo "Service failed to start; inspect journalctl -u $unit" >&2
    if [[ -n "$previous" && "$previous" != "$target" ]]; then
        link_release current "$previous"
        systemctl restart "$unit"
    else
        systemctl stop "$unit" || true
    fi
    return 1
}

case "${1:-}" in
    install) [[ $# == 3 ]] || usage ;;
    rollback) [[ $# == 1 ]] || usage ;;
    *) usage ;;
esac
[[ $(id -u) == 0 ]] || { echo "Run the installer as root." >&2; exit 1; }
command -v systemctl >/dev/null
if [[ $1 == rollback ]]; then
    [[ -d "$base" ]] || { echo "No prior installation." >&2; exit 1; }
    exec 9>"$base/.install.lock"
    flock 9
    previous=$(readlink "$base/previous") || { echo "No previous release recorded." >&2; exit 1; }
    [[ "$previous" =~ ^releases/[a-f0-9]{64}$ && -x "$base/$previous/cs2world-server" ]] ||
        { echo "Previous release is missing." >&2; exit 1; }
    activate "$previous"
    exit
fi

binary=$(realpath "$2")
config=$(realpath "$3")
[[ -x "$binary" && -r "$config" ]] || { echo "Expected an executable binary and a readable config." >&2; exit 1; }
"$binary" check -c "$config"
# Include the config in release identity so rollback restores both artifacts.
release_hash=$({ sha256sum < "$binary"; sha256sum < "$config"; } | sha256sum | cut -d ' ' -f1)
target="releases/$release_hash"
install -d -m 0755 "$base" "$base/releases"
# Serialize two simultaneous installations and the activation of their systemd unit.
exec 9>"$base/.install.lock"
flock 9
getent group "$config_group" >/dev/null || groupadd --system "$config_group"
if [[ ! -d "$base/$target" ]]; then
    stage=$(mktemp -d "$base/releases/.stage.XXXXXXXX")
    install -m 0755 "$binary" "$stage/cs2world-server"
    install -m 0640 -g "$config_group" "$config" "$stage/config.json"
    chmod 0755 "$stage"
    mv -T "$stage" "$base/$target"
fi
unit_file=$(mktemp "/etc/systemd/system/.$unit.XXXXXXXX")
cat >"$unit_file" <<EOF
[Unit]
Description=CS2World Rust server
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
DynamicUser=yes
User=$account
SupplementaryGroups=$config_group
ExecStart=$base/current/cs2world-server run -c $base/current/config.json
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=2
NoNewPrivileges=yes
PrivateTmp=yes
ProtectHome=yes
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
EOF
chmod 0644 "$unit_file"
mv -Tf "$unit_file" "/etc/systemd/system/$unit"
systemctl daemon-reload
activate "$target"
