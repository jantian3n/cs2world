#!/usr/bin/env python3
"""
Windows v2rayN Compatibility & End-to-End Verification Suite for sing-box.exe (cs2world).
Verifies:
  1. `sing-box.exe version` and `sing-box.exe version -n` output (v2rayN core detection).
  2. `sing-box.exe check -c <cfg> --disable-color` (v2rayN CLI flags).
  3. Launch server and v2rayN-style client (`run -c <cfg> --disable-color`,
     `auto_detect_interface: true`, `experimental.clash_api.external_controller`).
  4. Valve Steam A2S_INFO query returns Counter-Strike 2 (AppID=730).
  5. Unauthorized UDP probe receives Valve GNS 0x02 ConnectionClosed ("Server is full").
  6. HTTP GET /ping + 256KB binary /blob SHA256 check through the local mixed proxy.
  7. Clash API GET /version returns HTTP 200 JSON (v2rayN statistics/traffic API).
"""

import hashlib
import http.client
import http.server
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
BIN_PATH = SCRIPT_DIR / "sing-box.exe"
TEST_PSK = "cs2world-v2rayn-win-e2e-test-psk-2026"
TEST_BLOB = os.urandom(256 * 1024)
TEST_BLOB_SHA = hashlib.sha256(TEST_BLOB).hexdigest()


def find_free_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def find_free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def read_cstr(data: bytes, offset: int):
    end = data.index(b"\x00", offset)
    return data[offset:end].decode("utf-8", errors="replace"), end + 1


class DummyOriginHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/ping":
            body = b"hello-from-cs2world-v2rayn-win"
        elif self.path == "/blob":
            body = TEST_BLOB
        else:
            body = b"ok"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


def wait_for_tcp_port(port: int, timeout_sec: float = 8.0):
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                return
        except OSError:
            time.sleep(0.1)
    raise TimeoutError(f"TCP port 127.0.0.1:{port} did not open within {timeout_sec}s")


def wait_for_a2s(udp_port: int, timeout_sec: float = 8.0):
    deadline = time.time() + timeout_sec
    last_err = None
    while time.time() < deadline:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.settimeout(0.5)
        try:
            sock.sendto(b"\xff\xff\xff\xffTSource Engine Query\x00", ("127.0.0.1", udp_port))
            data, _ = sock.recvfrom(2048)
            if data[:5] == b"\xff\xff\xff\xffI":
                return data
        except OSError as exc:
            last_err = exc
            time.sleep(0.1)
        finally:
            sock.close()
    raise TimeoutError(f"A2S server on UDP 127.0.0.1:{udp_port} not ready: {last_err}")


def test_version_commands():
    print("\n[Test 1] Checking sing-box.exe version & version -n ...")
    res = subprocess.run([str(BIN_PATH), "version"], capture_output=True, text=True, check=True)
    out = res.stdout.strip()
    assert "sing-box version 1.14.4-cs2world-0.0.1" in out, f"Unexpected version output:\n{out}"
    assert "Tags:" in out, f"Missing Tags line in version output:\n{out}"
    assert "cs2world" in out, f"Missing cs2world mention in version output:\n{out}"
    for tag in ("with_gvisor", "with_quic", "with_wireguard", "with_utls", "with_clash_api"):
        assert tag in out, f"Missing expected v2rayN build tag {tag} in:\n{out}"
    print("  [PASS] `version` reports expected version, build tags, and cs2world")

    res_n = subprocess.run([str(BIN_PATH), "version", "-n"], capture_output=True, text=True, check=True)
    assert res_n.stdout.strip() == "1.14.4-cs2world-0.0.1", f"Unexpected version -n output: {res_n.stdout!r}"
    print(f"  [PASS] `version -n` reports: {res_n.stdout.strip()}")


def test_check_disable_color(client_cfg_path: str):
    print("\n[Test 2] Checking `sing-box.exe check -c <cfg> --disable-color` ...")
    res = subprocess.run(
        [str(BIN_PATH), "check", "-c", client_cfg_path, "--disable-color"],
        capture_output=True,
        text=True,
        check=True,
    )
    print(f"  [PASS] Config check exited 0: {res.stdout.strip()}")


def test_a2s_info(udp_port: int):
    print("\n[Test 4] Valve Steam A2S_INFO Probe against Windows server ...")
    data = wait_for_a2s(udp_port)
    off = 6
    srv_name, off = read_cstr(data, off)
    map_name, off = read_cstr(data, off)
    _, off = read_cstr(data, off)
    game, off = read_cstr(data, off)
    app_id = struct.unpack_from("<H", data, off)[0]
    players, max_players = data[off + 2], data[off + 3]
    print(f"  [PASS] Game={game} (AppID={app_id}) | Map={map_name} | Players={players}/{max_players} | Name={srv_name}")
    assert app_id == 730 and game == "Counter-Strike 2"


def test_unauthorized_probe(udp_port: int):
    print("\n[Test 5] Unauthorized UDP Probe Defense ...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    try:
        sock.sendto(b"\x82\x11\x22\x33\x44" + os.urandom(40), ("127.0.0.1", udp_port))
        resp, _ = sock.recvfrom(2048)
    finally:
        sock.close()
    assert resp[0] == 0x02, f"Expected GNS ConnectionClosed (0x02), got {resp[:4]!r}"
    reason = resp[9:].rstrip(b"\x00").decode("utf-8", errors="replace")
    print(f"  [PASS] Deflected with Valve GNS 0x02 ConnectionClosed: {reason!r}")
    assert "Server is full" in reason


def test_mixed_proxy(mixed_port: int, origin_port: int):
    print("\n[Test 6] v2rayN-style HTTP Proxy -> cs2world Outbound -> cs2world Inbound -> Origin ...")
    t0 = time.time()

    conn = http.client.HTTPConnection("127.0.0.1", mixed_port, timeout=10)
    try:
        conn.request("GET", f"http://127.0.0.1:{origin_port}/ping")
        resp = conn.getresponse()
        body = resp.read()
        assert resp.status == 200, f"Unexpected HTTP status for /ping: {resp.status}"
        assert body == b"hello-from-cs2world-v2rayn-win", f"Unexpected body: {body!r}"
        print(f"  [PASS] Small HTTP GET via cs2world: {body.decode()!r}")
    finally:
        conn.close()

    conn = http.client.HTTPConnection("127.0.0.1", mixed_port, timeout=15)
    try:
        conn.request("GET", f"http://127.0.0.1:{origin_port}/blob")
        resp = conn.getresponse()
        blob = resp.read()
        assert resp.status == 200, f"Unexpected HTTP status for /blob: {resp.status}"
        got_sha = hashlib.sha256(blob).hexdigest()
        assert len(blob) == len(TEST_BLOB) and got_sha == TEST_BLOB_SHA, "256KB SHA256 mismatch"
        print(f"  [PASS] 256KB Bulk HTTP Download via cs2world: SHA256={got_sha[:16]}... ({time.time() - t0:.2f}s)")
    finally:
        conn.close()


def test_clash_api(clash_port: int):
    print("\n[Test 7] Checking v2rayN Clash API (/version) ...")
    conn = http.client.HTTPConnection("127.0.0.1", clash_port, timeout=5)
    try:
        conn.request("GET", "/version")
        resp = conn.getresponse()
        raw = resp.read().decode("utf-8", errors="replace")
        assert resp.status == 200, f"Expected 200 from Clash API /version, got {resp.status}: {raw}"
        payload = json.loads(raw)
        assert "version" in payload, f"Expected 'version' field in Clash API response: {payload}"
        print(f"  [PASS] Clash API HTTP 200 JSON: {payload}")
    finally:
        conn.close()


def main():
    if not BIN_PATH.exists():
        print(f"[ERROR] Missing executable: {BIN_PATH}")
        sys.exit(1)

    test_version_commands()

    udp_port = find_free_udp_port()
    mixed_port = find_free_tcp_port()
    clash_port = find_free_tcp_port()
    origin_port = find_free_tcp_port()

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", origin_port), DummyOriginHandler)
    http_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    http_thread.start()

    srv_cfg = {
        "log": {"level": "info", "timestamp": True},
        "inbounds": [
            {
                "type": "cs2world",
                "tag": "cs2world-in",
                "listen": "127.0.0.1",
                "listen_port": udp_port,
                "psk": TEST_PSK,
                "kcp": {
                    "data_shards": 10,
                    "parity_shards": 3,
                    "mtu": 1180,
                    "snd_wnd": 1024,
                    "rcv_wnd": 1024,
                },
                "a2s": {
                    "server_name": "CS2World Windows Test Server",
                    "maps": ["de_mirage", "de_inferno"],
                    "max_players": 20,
                    "game_version": "1.40.4.2",
                    "map_cycle_minutes": 30,
                },
                "shaper": {
                    "enabled": True,
                    "tick_rate_hz": 64,
                    "idle_sleep_seconds": 600,
                    "combat_mean_bytes": 240,
                    "fastdl_chunk_size": 1248,
                },
            }
        ],
        "outbounds": [{"type": "direct", "tag": "direct"}],
        "route": {"final": "direct"},
    }

    cli_cfg = {
        "log": {"level": "info", "timestamp": True},
        "inbounds": [
            {
                "type": "mixed",
                "tag": "socks",
                "listen": "127.0.0.1",
                "listen_port": mixed_port,
            }
        ],
        "outbounds": [
            {
                "type": "cs2world",
                "tag": "proxy",
                "server": "127.0.0.1",
                "server_port": udp_port,
                "psk": TEST_PSK,
                "kcp": {
                    "data_shards": 10,
                    "parity_shards": 3,
                    "mtu": 1180,
                    "snd_wnd": 1024,
                    "rcv_wnd": 1024,
                },
                "shaper": {
                    "enabled": True,
                    "tick_rate_hz": 64,
                    "idle_sleep_seconds": 600,
                    "combat_mean_bytes": 180,
                    "fastdl_chunk_size": 1248,
                },
            },
            {"type": "direct", "tag": "direct"},
        ],
        "route": {
            "auto_detect_interface": True,
            "final": "proxy",
        },
        "experimental": {
            "clash_api": {
                "external_controller": f"127.0.0.1:{clash_port}",
            }
        },
    }

    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as sf, \
         tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as cf:
        json.dump(srv_cfg, sf, indent=2)
        json.dump(cli_cfg, cf, indent=2)
        srv_path, cli_path = sf.name, cf.name

    srv_proc = None
    cli_proc = None
    try:
        test_check_disable_color(cli_path)

        print(f"\n[Test 3] Launching server (UDP:{udp_port}) & v2rayN-style client (mixed:{mixed_port}, clash:{clash_port}) with --disable-color ...")
        srv_proc = subprocess.Popen(
            [str(BIN_PATH), "run", "-c", srv_path, "--disable-color"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        cli_proc = subprocess.Popen(
            [str(BIN_PATH), "run", "-c", cli_path, "--disable-color"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

        wait_for_a2s(udp_port)
        wait_for_tcp_port(mixed_port)
        wait_for_tcp_port(clash_port)
        assert srv_proc.poll() is None, "Server exited prematurely"
        assert cli_proc.poll() is None, "Client exited prematurely"
        print("  [PASS] Both server and client started cleanly with --disable-color")

        test_a2s_info(udp_port)
        test_unauthorized_probe(udp_port)
        test_mixed_proxy(mixed_port, origin_port)
        test_clash_api(clash_port)

        print("\n=== ALL 7 WINDOWS v2rayN E2E TESTS PASSED! ===")
    except Exception as exc:
        print(f"\n[ERROR] {exc}")
        for label, proc in (("SERVER", srv_proc), ("CLIENT", cli_proc)):
            if proc is not None:
                proc.terminate()
                try:
                    out, _ = proc.communicate(timeout=3)
                except Exception:
                    proc.kill()
                    out, _ = proc.communicate()
                print(f"--- {label} LOG ---\n{out}")
        sys.exit(1)
    finally:
        for proc in (cli_proc, srv_proc):
            if proc is not None and proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=3)
                except Exception:
                    proc.kill()
        httpd.shutdown()
        for path in (srv_path, cli_path):
            try:
                os.unlink(path)
            except OSError:
                pass


if __name__ == "__main__":
    main()
