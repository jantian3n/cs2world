#!/usr/bin/env python3
"""
End-to-End Verification for Native sing-box-cs2world Protocol ("type": "cs2world"):
  HTTP Client -> [sing-box-cs2world client: mixed HTTP/SOCKS5 inbound -> cs2world outbound]
              ===(Valve GNS + AES-256-GCM + AI 64-Tick Shaper over UDP:27215)===>
                 [sing-box-cs2world server: cs2world inbound -> direct outbound]
              -> Local HTTP Target Server

Also verifies:
  1. Valve Steam A2S_INFO query returns authentic Counter-Strike 2 (AppID 730) server info.
  2. Unauthorized active UDP probes receive Valve GNS 0x02 "Server is full (20/20)".
  3. Real HTTP GET + 256KB binary download through the native sing-box mixed proxy passes SHA256 check!
"""

import hashlib
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
import urllib.request

CS2WORLD_UDP = ("127.0.0.1", 27215)
MIXED_PROXY_PORT = 17890
HTTP_ORIGIN_PORT = 18080
TEST_BLOB = os.urandom(256 * 1024)
TEST_BLOB_SHA = hashlib.sha256(TEST_BLOB).hexdigest()


def read_cstr(data: bytes, offset: int):
    end = data.index(b"\x00", offset)
    return data[offset:end].decode("utf-8", errors="replace"), end + 1


class DummyOriginHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/ping":
            body = b"hello-from-cs2world-native-singbox"
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


def test_a2s_info():
    print("\n[Test 1] Valve Steam A2S_INFO Probe against sing-box-cs2world server ...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    sock.sendto(b"\xff\xff\xff\xffTSource Engine Query\x00", CS2WORLD_UDP)
    data, _ = sock.recvfrom(2048)
    sock.close()

    assert data[:5] == b"\xff\xff\xff\xffI", f"Unexpected A2S header: {data[:5]!r}"
    off = 6
    srv_name, off = read_cstr(data, off)
    map_name, off = read_cstr(data, off)
    _, off = read_cstr(data, off)
    game, off = read_cstr(data, off)
    app_id = struct.unpack_from("<H", data, off)[0]
    players, max_players = data[off + 2], data[off + 3]
    print(f"  ✓ Game: {game} (AppID={app_id}) | Map: {map_name} | Players: {players}/{max_players}")
    print(f"  ✓ Server Name: {srv_name}")
    assert app_id == 730 and game == "Counter-Strike 2"


def test_unauthorized_probe():
    print("\n[Test 2] Unauthorized UDP Probe Defense ...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    sock.sendto(b"\x82\x11\x22\x33\x44" + os.urandom(40), CS2WORLD_UDP)
    resp, _ = sock.recvfrom(2048)
    sock.close()
    assert resp[0] == 0x02
    reason = resp[9:].rstrip(b"\x00").decode("utf-8", errors="replace")
    print(f"  ✓ Deflected with Valve GNS 0x02 ConnectionClosed: {reason!r}")
    assert "Server is full" in reason


def test_native_singbox_proxy():
    print("\n[Test 3] Native sing-box HTTP Proxy -> cs2world Outbound -> cs2world Inbound -> Origin ...")
    proxy_handler = urllib.request.ProxyHandler({
        "http": f"http://127.0.0.1:{MIXED_PROXY_PORT}",
    })
    opener = urllib.request.build_opener(proxy_handler)

    # 1. Small HTTP request (Combat 64-Tick mode)
    t0 = time.time()
    with opener.open(f"http://127.0.0.1:{HTTP_ORIGIN_PORT}/ping", timeout=5) as resp:
        body = resp.read()
    assert body == b"hello-from-cs2world-native-singbox", f"Unexpected body: {body!r}"
    print(f"  ✓ Small HTTP GET via cs2world: {body.decode()!r}")

    # 2. 256KB binary download (FastDL 1248B chunk mode)
    with opener.open(f"http://127.0.0.1:{HTTP_ORIGIN_PORT}/blob", timeout=10) as resp:
        blob = resp.read()
    got_sha = hashlib.sha256(blob).hexdigest()
    assert len(blob) == len(TEST_BLOB) and got_sha == TEST_BLOB_SHA
    print(f"  ✓ 256KB Bulk HTTP Download via cs2world: SHA256={got_sha[:16]}... ({time.time()-t0:.2f}s)")


def main():
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", HTTP_ORIGIN_PORT), DummyOriginHandler)
    http_thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    http_thread.start()

    with open("configs/cs2world_server.json", "r") as f:
        srv_cfg = json.load(f)
    srv_cfg["inbounds"][0]["listen"] = CS2WORLD_UDP[0]
    srv_cfg["inbounds"][0]["listen_port"] = CS2WORLD_UDP[1]

    with open("configs/cs2world_client.json", "r") as f:
        cli_cfg = json.load(f)
    cli_cfg["inbounds"][0]["listen_port"] = MIXED_PROXY_PORT
    cli_cfg["outbounds"][0]["server"] = CS2WORLD_UDP[0]
    cli_cfg["outbounds"][0]["server_port"] = CS2WORLD_UDP[1]

    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as sf, \
         tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as cf:
        json.dump(srv_cfg, sf)
        json.dump(cli_cfg, cf)
        srv_path, cli_path = sf.name, cf.name

    srv_proc = subprocess.Popen(
        ["./bin/sing-box-cs2world", "run", "-c", srv_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    cli_proc = subprocess.Popen(
        ["./bin/sing-box-cs2world", "run", "-c", cli_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        time.sleep(1.5)
        test_a2s_info()
        test_unauthorized_probe()
        test_native_singbox_proxy()
        print("\n=== ALL NATIVE sing-box-cs2world TESTS PASSED! ===")
    except Exception as e:
        print(f"\n[ERROR] {e}")
        cli_proc.terminate()
        srv_proc.terminate()
        print("--- SERVER LOG ---")
        print(srv_proc.stdout.read())
        print("--- CLIENT LOG ---")
        print(cli_proc.stdout.read())
        sys.exit(1)
    finally:
        cli_proc.terminate()
        srv_proc.terminate()
        httpd.shutdown()
        try:
            cli_proc.wait(timeout=3)
            srv_proc.wait(timeout=3)
        except Exception:
            pass
        os.unlink(srv_path)
        os.unlink(cli_path)


if __name__ == "__main__":
    main()
