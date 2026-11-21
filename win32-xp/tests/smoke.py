"""Exercise only the packaged EXE. UI tests use a temporary state with manual proxy mode."""
import argparse
import contextlib
import http.client
import http.server
import json
import os
from pathlib import Path
import queue
import runpy
import secrets
import shutil
import socket
import socketserver
import subprocess
import tempfile
import threading
import time
import winreg

from window_latency import exercise_window, wait_for

ROOT = Path(__file__).resolve().parents[2]
ref = runpy.run_path(str(ROOT / "tests/interop/run.py"))
HOST = "127.0.0.1"


@contextlib.contextmanager
def headless(exe, state):
    proc = subprocess.Popen([str(exe), "--test-node", str(state), "0"], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, stdin=subprocess.PIPE, text=True)
    lines = queue.Queue()
    def consume():
        for line in proc.stdout:
            lines.put(line.strip())
        lines.put(None)
    reader = threading.Thread(target=consume, daemon=True)
    reader.start()
    try:
        while True:
            line = lines.get(timeout=45)
            if line is None:
                raise RuntimeError("embedded client exited before authentication")
            if line.startswith("A2S"):
                print(line, flush=True)
            if line.startswith("READY "):
                break
        yield int(line.split()[1])
    finally:
        if proc.poll() is None:
            proc.stdin.write("\n")
            proc.stdin.flush()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        reader.join(1)
    if proc.returncode:
        raise RuntimeError("embedded client returned failure")


def udp_exchange(mixed, ip, port, payload):
    with socket.create_connection((HOST, mixed), 10) as control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(10)
        relay = ref["socks_request"](control, 3, bytes([1]) + bytes(6))
        head = bytes([0, 0, 0, 1]) + socket.inet_aton(ip) + port.to_bytes(2, "big")
        s.sendto(head + payload, relay)
        packet = s.recv(65535)
        if packet[:10] != head:
            raise AssertionError("UDP source address or port mismatch")
        return packet[10:]


def real(exe, state):
    with headless(exe, state) as port:
        client = http.client.HTTPSConnection(HOST, port, timeout=15)
        client.set_tunnel("www.cloudflare.com", 443)
        try:
            client.request("GET", "/cdn-cgi/trace")
            response = client.getresponse()
            if response.status != 200 or b"h=www.cloudflare.com" not in response.read(65536):
                raise AssertionError("TLS origin did not return its expected page")
        finally:
            client.close()
        txid = secrets.token_bytes(2)
        query = txid + bytes.fromhex("01000001000000000000") + b"\x07example\x03com\x00\x00\x01\x00\x01"
        dns = udp_exchange(port, "1.1.1.1", 53, query)
        if len(dns) < 12 or dns[:2] != txid or not dns[2] & 128 or dns[3] & 15 or int.from_bytes(dns[6:8],"big") < 1:
            raise AssertionError("UDP DNS reply did not answer the query")
        print("Active node authenticated; HTTPS CONNECT and SOCKS5 UDP DNS passed.", flush=True)


def legacy_state(port, mixed, key):
    return {"nodes":[{"id":"fixture","name":"local","server":HOST,"server_port":port,"psk":key}],
            "settings":{"active_node_id":"fixture","proxy_mode":"manual","routing_mode":"global",
                        "mixed_port":mixed,"clash_port":19090}}


def proxy_settings():
    path = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings"
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
        snapshot = {}
        for name in ("ProxyEnable", "ProxyServer", "ProxyOverride", "AutoConfigURL"):
            try:
                snapshot[name] = winreg.QueryValueEx(key, name)
            except FileNotFoundError:
                pass
        return snapshot


@contextlib.contextmanager
def verify_system_proxy():
    before = proxy_settings()
    try:
        yield
    finally:
        if before != proxy_settings():
            raise AssertionError("current user's system proxy settings changed during smoke test")


def local(exe, go):
    with tempfile.TemporaryDirectory() as folder, contextlib.ExitStack() as stack:
        folder=Path(folder)
        isolated=folder/"CS2World-XP.exe"
        shutil.copyfile(exe,isolated)
        exercise_window(isolated, "empty state")
        if (folder/"binaries").exists():raise AssertionError("external core directory required")
        state=folder/"data/state.json"
        state.parent.mkdir(exist_ok=True)
        mixed=ref["port"]()
        timeout_port=ref["port"](socket.SOCK_DGRAM)
        # Bind a silent UDP endpoint to exercise the complete retry interval.
        trap=stack.enter_context(socket.socket(socket.AF_INET,socket.SOCK_DGRAM))
        trap.bind((HOST,timeout_port))
        key=secrets.token_hex(16)
        state.write_text(json.dumps(legacy_state(timeout_port,mixed,key)),encoding="utf-8")
        def timeout_handshake(api, hwnd):
            idle_caption = api.connect_caption(hwnd)
            trap.settimeout(7)
            start = time.perf_counter_ns()
            api.post(hwnd, 0x0111, 102)
            packets = [trap.recvfrom(2048) for _ in range(3)]
            if len(set(packets)) != 1:
                raise AssertionError("silent endpoint did not receive three matching handshake attempts")
            wait_for(lambda: api.connect_caption(hwnd) == idle_caption,
                     "failed connection did not return to idle")
            return start, time.perf_counter_ns()

        exercise_window(isolated, "handshake timeout", timeout_handshake)
        udp=ref["port"](socket.SOCK_DGRAM)
        cfg={"inbounds":[{"type":"cs2world","tag":"in","listen":HOST,"listen_port":udp,"psk":key,
                           "kcp":{"data_shards":10,"parity_shards":3},"shaper":{"enabled":True}}],
             "outbounds":[{"type":"direct","tag":"out"}]}
        config=folder/"server.json"
        config.write_text(json.dumps(cfg),encoding="utf-8")
        proc=subprocess.Popen([str(go),"run","-c",str(config)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        stack.callback(ref["stop"],proc)
        relay=stack.enter_context(ref["Relay"](udp,False))
        state.write_text(json.dumps(legacy_state(relay.port,mixed,key)),encoding="utf-8")
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as readiness:
            readiness.settimeout(.1)
            for _ in range(50):
                readiness.sendto(ref["INFO"],(HOST,udp))
                try:
                    if readiness.recv(2048).startswith(bytes([255])*4+b"I"):break
                except (TimeoutError,ConnectionResetError):pass
                time.sleep(.01)
            else:raise RuntimeError("reference server not ready")
        class SlowOrigin(ref["HTTPOrigin"]):
            def do_GET(self):
                if self.path != "/paced":
                    return super().do_GET()
                self.send_response(200)
                self.send_header("Content-Length",str(len(ref["BLOB"])))
                self.end_headers()
                for i in range(0,len(ref["BLOB"]),4096):
                    self.wfile.write(ref["BLOB"][i:i+4096])
                    if i + 4096 < len(ref["BLOB"]):
                        time.sleep(.04)
        http_port=stack.enter_context(ref["origin"](http.server.ThreadingHTTPServer,SlowOrigin))
        udp_port=stack.enter_context(ref["origin"](socketserver.ThreadingUDPServer,ref["DatagramEcho"]))
        with headless(isolated,state) as port:
            if ref["request_http"](port,http_port,"blob")!=(200,ref["BLOB"]):raise AssertionError("256 KiB payload mismatch")
            for n in (7,512,1200):
                data=os.urandom(n)
                if udp_exchange(port,HOST,udp_port,data)!=data:raise AssertionError("UDP echo mismatch")
        def download(api, hwnd):
            api.post(hwnd, 0x0111, 102)
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                client = http.client.HTTPConnection(HOST, mixed, timeout=8)
                try:
                    client.connect()
                except ConnectionRefusedError:
                    client.close()
                    time.sleep(.05)
                    continue
                try:
                    client.request("GET", f"http://localhost:{http_port}/paced")
                    response = client.getresponse()
                    if response.status != 200:
                        raise AssertionError(f"GUI download returned HTTP {response.status}")
                    first = response.read(1)
                    start = time.perf_counter_ns()
                    data = first + response.read()
                    end = time.perf_counter_ns()
                    if data != ref["BLOB"]:
                        raise AssertionError("GUI download payload mismatch")
                    return start, end
                finally:
                    client.close()
            raise TimeoutError("window-initiated Mixed listener did not start")

        before=list(relay.count)
        exercise_window(isolated, "256 KiB download", download)
        if relay.count[1]-before[1]<100:raise AssertionError("GUI download did not traverse the Go UDP relay")
    print("Single EXE, old state, GUI timeout/download responsiveness, loopback Go TCP/UDP and tray cleanup passed.",flush=True)


if __name__=="__main__":
    ap=argparse.ArgumentParser()
    ap.add_argument("--exe",type=Path,required=True)
    ap.add_argument("--go",type=Path)
    ap.add_argument("--real-state",type=Path)
    args=ap.parse_args()
    if not (args.go or args.real_state):ap.error("supply --go or --real-state")
    if args.go:
        with verify_system_proxy():
            local(args.exe.resolve(),args.go.resolve())
    if args.real_state:real(args.exe.resolve(),args.real_state.resolve())
