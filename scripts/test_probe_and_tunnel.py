#!/usr/bin/env python3
"""
CS2-Ghost End-to-End Verification Suite:
1. Sends authentic Valve A2S_INFO & A2S_PLAYER queries to UDP :27015 and parses binary responses.
2. Sends unauthorized UDP active-probe packets to verify CS2 "Server is full" rejection.
3. Spins up a local TCP echo backend on :10086 and tests concurrent small + 256KB bulk transfers
   through the full [Client TCP:9000 -> smux -> KCP+FEC -> AI 64-Tick Shaper -> AES-GCM GNS -> UDP:27015 -> Server -> Backend] pipeline.
"""

import hashlib
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

SERVER_UDP = ("127.0.0.1", 27115)
CLIENT_TCP = ("127.0.0.1", 19115)
BACKEND_TCP = ("127.0.0.1", 19116)


def read_cstr(data: bytes, offset: int):
    end = data.index(b"\x00", offset)
    return data[offset:end].decode("utf-8", errors="replace"), end + 1


def test_a2s_info():
    print("\n[Test 1] Sending Valve Steam A2S_INFO Query to 127.0.0.1:27015 ...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    query = b"\xff\xff\xff\xffTSource Engine Query\x00"
    sock.sendto(query, SERVER_UDP)
    data, _ = sock.recvfrom(2048)
    sock.close()

    assert data[:5] == b"\xff\xff\xff\xffI", f"Unexpected A2S header: {data[:5]!r}"
    off = 5
    protocol_ver = data[off]
    off += 1
    server_name, off = read_cstr(data, off)
    map_name, off = read_cstr(data, off)
    folder, off = read_cstr(data, off)
    game, off = read_cstr(data, off)
    app_id = struct.unpack_from("<H", data, off)[0]
    off += 2
    players, max_players, bots = data[off], data[off + 1], data[off + 2]
    off += 3
    srv_type, env, vis, vac = chr(data[off]), chr(data[off + 1]), data[off + 2], data[off + 3]
    off += 4
    version, off = read_cstr(data, off)

    print(f"  ✓ Protocol: {protocol_ver} | Game: {game} (AppID={app_id}) | Version: {version}")
    print(f"  ✓ Server Name: {server_name}")
    print(f"  ✓ Map: {map_name} | Players: {players}/{max_players} | Type: {srv_type}/{env} | VAC: {vac}")
    assert app_id == 730, f"Expected CS2 AppID 730, got {app_id}"
    assert game == "Counter-Strike 2"


def test_unauthorized_probe():
    print("\n[Test 2] Sending Unauthorized Active Probe to 127.0.0.1:27015 ...")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(2.0)
    # Random non-A2S packet mimicking an active prober or random CS2 client
    sock.sendto(b"\x82\xde\xad\xbe\xef" + os.urandom(48), SERVER_UDP)
    resp, _ = sock.recvfrom(2048)
    sock.close()

    assert resp[0] == 0x02, f"Expected Valve GNS ConnectionClosed (0x02), got {resp[0]:#x}"
    reason = resp[9:].rstrip(b"\x00").decode("utf-8", errors="replace")
    print(f"  ✓ Active Probe Deflected! GNS Control Flag=0x02 | Reason: {reason!r}")
    assert "Server is full" in reason


def start_echo_backend(stop_event: threading.Event):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(BACKEND_TCP)
    srv.listen(32)
    srv.settimeout(0.5)

    def handle_conn(conn):
        try:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                conn.sendall(chunk)
        except Exception:
            pass
        finally:
            conn.close()

    while not stop_event.is_set():
        try:
            conn, _ = srv.accept()
            threading.Thread(target=handle_conn, args=(conn,), daemon=True).start()
        except socket.timeout:
            continue
    srv.close()


def run_single_stream_check(payload: bytes, label: str):
    expected_hash = hashlib.sha256(payload).hexdigest()
    sock = socket.create_connection(CLIENT_TCP, timeout=5.0)
    sock.settimeout(10.0)

    def sender():
        sock.sendall(payload)
        sock.shutdown(socket.SHUT_WR)

    t = threading.Thread(target=sender)
    t.start()

    received = bytearray()
    while len(received) < len(payload):
        chunk = sock.recv(65536)
        if not chunk:
            break
        received.extend(chunk)
    t.join()
    sock.close()

    actual_hash = hashlib.sha256(received).hexdigest()
    assert len(received) == len(payload), f"{label}: expected {len(payload)} bytes, got {len(received)}"
    assert actual_hash == expected_hash, f"{label}: SHA256 mismatch!"
    print(f"  ✓ {label}: {len(payload):,} bytes round-tripped cleanly (SHA256={actual_hash[:16]}...)")


def test_tunnel_forwarding():
    print("\n[Test 3] Testing End-to-End CS2-Ghost Tunnel (Combat Small + FastDL 256KB Bulk) ...")
    t0 = time.time()
    # 1. Small interactive packet (triggers 64-Tick Combat mode)
    run_single_stream_check(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n" * 4, "Combat Small Stream")
    # 2. Large 256KB bulk transfer (triggers FastDL 1248B chunk mode)
    run_single_stream_check(os.urandom(256 * 1024), "FastDL 256KB Bulk Stream")
    elapsed = time.time() - t0
    print(f"  ✓ All tunnel integrity checks PASSED in {elapsed:.2f}s!")


def main():
    stop_event = threading.Event()
    backend_thread = threading.Thread(target=start_echo_backend, args=(stop_event,), daemon=True)
    backend_thread.start()

    with open("configs/server.json", "r") as f:
        srv_cfg = json.load(f)
    srv_cfg["listen_addr"] = f"{SERVER_UDP[0]}:{SERVER_UDP[1]}"
    srv_cfg["target_addr"] = f"{BACKEND_TCP[0]}:{BACKEND_TCP[1]}"

    with open("configs/client.json", "r") as f:
        cli_cfg = json.load(f)
    cli_cfg["listen_addr"] = f"{CLIENT_TCP[0]}:{CLIENT_TCP[1]}"
    cli_cfg["target_addr"] = f"{SERVER_UDP[0]}:{SERVER_UDP[1]}"

    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as sf, \
         tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as cf:
        json.dump(srv_cfg, sf)
        json.dump(cli_cfg, cf)
        srv_cfg_path, cli_cfg_path = sf.name, cf.name

    srv_proc = subprocess.Popen(
        ["./bin/cs2-ghost", "-config", srv_cfg_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    cli_proc = subprocess.Popen(
        ["./bin/cs2-ghost", "-config", cli_cfg_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        time.sleep(1.5)
        test_a2s_info()
        test_unauthorized_probe()
        test_tunnel_forwarding()
        print("\n=== ALL CS2-GHOST VERIFICATION TESTS PASSED! ===")
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
        stop_event.set()
        try:
            cli_proc.wait(timeout=3)
            srv_proc.wait(timeout=3)
        except Exception:
            pass
        os.unlink(srv_cfg_path)
        os.unlink(cli_cfg_path)


if __name__ == "__main__":
    main()
