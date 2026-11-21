"""Cross-check both live implementations using loopback sockets and explicit Mixed clients."""
import argparse
import concurrent.futures
import contextlib
import hashlib
import http.client
import http.server
import json
import math
import os
from pathlib import Path
import secrets
import runpy
import socket
import socketserver
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import zipfile

ROOT = Path(__file__).resolve().parents[2]
GO_COMMIT = "ea7b5e100d898c8a67a25babe2be608652fbcc89"
HOST = "127.0.0.1"
EXE = ".exe" if os.name == "nt" else ""
TIMEOUT = 12
BLOB = bytes(range(256)) * 1024
INFO = b"\xff\xff\xff\xffTSource Engine Query\0"
PREFIX = b"\xff" * 4
UDP_RELEASE = threading.Event()

# Use the locked sing uot.Client.DialContext API to exercise the connected mode.
# The Go standard library also provides an ephemeral loopback HTTPS origin and its CA.
GO_HELPER = r'''
package main
import (
    "context"
    "encoding/pem"
    "fmt"
    "io"
    "net/http"
    "net/http/httptest"
    "os"
    "time"
    M "github.com/sagernet/sing/common/metadata"
    N "github.com/sagernet/sing/common/network"
    "github.com/sagernet/sing/common/uot"
    "github.com/sagernet/sing/protocol/socks"
)
func run() error {
    if os.Args[1]=="https" {
        srv:=httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter,r *http.Request) {
            fmt.Fprint(w, "local HTTPS via CONNECT")
        }))
        defer srv.Close()
        cert:=pem.EncodeToMemory(&pem.Block{Type:"CERTIFICATE",Bytes:srv.Certificate().Raw})
        if err:=os.WriteFile(os.Args[2],cert,0600);err!=nil{return err}
        fmt.Println(srv.Listener.Addr().String())
        _,err:=io.Copy(io.Discard,os.Stdin)
        return err
    }
    client:=socks.NewClient(N.SystemDialer,M.ParseSocksaddr(os.Args[2]),socks.Version5,"","")
    u:=uot.Client{Dialer:client,Version:uot.Version}
    ctx,cancel:=context.WithTimeout(context.Background(),10*time.Second)
    defer cancel()
    c,err:=u.DialContext(ctx,"udp",M.ParseSocksaddr(os.Args[3]))
    if err!=nil{return err}
    defer c.Close()
    c.SetDeadline(time.Now().Add(10*time.Second))
    const text="uot-v2-connected"
    if _,err=c.Write([]byte(text));err!=nil{return err}
    out:=make([]byte,256)
    n,err:=c.Read(out)
    if err!=nil{return err}
    if string(out[:n])!=text{return fmt.Errorf("unexpected UoT response: %q",out[:n])}
    return nil
}
func main(){if err:=run();err!=nil{fmt.Fprintln(os.Stderr,err);os.Exit(1)}}
'''


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def port(sock_type=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, sock_type) as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


class HTTPOrigin(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        data = BLOB if self.path == "/blob" else b"loopback origin"
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        # Reply only after FIN, so receiving the response proves TCP half-close.
        self.request.settimeout(TIMEOUT)
        chunks = []
        while data := self.request.recv(32768):
            chunks.append(data)
        self.request.sendall(b"".join(chunks))


class DatagramEcho(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        if data.startswith(b"cs2world-hold-first:"):
            UDP_RELEASE.wait(TIMEOUT)
        sock.sendto(data, self.client_address)


@contextlib.contextmanager
def origin(kind, handler):
    with kind((HOST, 0), handler) as srv:
        thread = threading.Thread(target=srv.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        try:
            yield srv.server_address[1]
        finally:
            srv.shutdown()
            thread.join()


class Relay:
    """Forward opaque UDP payloads and optionally drop bursts longer than the parity budget."""
    def __init__(self, server_port, impair):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind((HOST, 0))
        self.sock.settimeout(0.1)
        self.port = self.sock.getsockname()[1]
        self.upstream = (HOST, server_port)
        self.client = None
        self.impair = impair
        self.count = [0, 0]
        self.dropped = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self):
        while not self.stop.is_set():
            try:
                data, addr = self.sock.recvfrom(65535)
            except (socket.timeout, ConnectionResetError):
                continue
            downstream = addr == self.upstream
            if downstream:
                target = self.client
            else:
                self.client, target = addr, self.upstream
            if target is None:
                continue
            if data[:1] == b"\x82":
                side = int(downstream)
                self.count[side] += 1
                if self.impair and 35 <= self.count[side] % 101 <= 39:
                    self.dropped += 1
                    continue
            self.sock.sendto(data, target)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        self.thread.join()
        self.sock.close()


def exact(sock, n):
    out = bytearray()
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise EOFError("unexpected EOF")
        out.extend(chunk)
    return bytes(out)


def addr_bytes(number):
    return b"\x01\x7f\x00\x00\x01" + number.to_bytes(2, "big")


def socks_request(conn, command, addr):
    conn.sendall(b"\x05\x01\x00")
    check(exact(conn, 2) == b"\x05\x00", "SOCKS5 auth response")
    conn.sendall(bytes([5, command, 0]) + addr)
    header = exact(conn, 4)
    check(header[:3] == b"\x05\x00\x00", f"SOCKS5 request rejected {header.hex()}")
    if header[3] == 1:
        host = socket.inet_ntop(socket.AF_INET, exact(conn, 4))
    elif header[3] == 4:
        host = socket.inet_ntop(socket.AF_INET6, exact(conn, 16))
    else:
        host = exact(conn, exact(conn, 1)[0]).decode()
    bound_port = int.from_bytes(exact(conn, 2), "big")
    return (HOST if host in ("0.0.0.0", "::") else host, bound_port)


def tcp_halfclose(mixed, origin_port, payload):
    with socket.create_connection((HOST, mixed), TIMEOUT) as s:
        socks_request(s, 1, addr_bytes(origin_port))
        s.sendall(payload)
        s.shutdown(socket.SHUT_WR)
        check(exact(s, len(payload)) == payload, "half-close response differs")


def request_http(mixed, origin_port, path, timeout=TIMEOUT):
    conn = http.client.HTTPConnection(HOST, mixed, timeout=timeout)
    try:
        conn.request("GET", f"http://{HOST}:{origin_port}/{path}")
        reply = conn.getresponse()
        return reply.status, reply.read()
    finally:
        conn.close()


def test_business(mixed, origins, helper, ca):
    http_port, echo_port, tls_port, udp_ports = origins
    check(request_http(mixed, http_port, "ping") == (200, b"loopback origin"), "absolute-form HTTP proxy")
    status, blob = request_http(mixed, http_port, "blob")
    check(status == 200 and hashlib.sha256(blob).digest() == hashlib.sha256(BLOB).digest(), "256 KiB integrity")

    conn = http.client.HTTPSConnection(HOST, mixed, context=ssl.create_default_context(cafile=str(ca)), timeout=TIMEOUT)
    try:
        conn.set_tunnel(HOST, tls_port)
        conn.request("GET", "/ping")
        resp = conn.getresponse()
        check(resp.status == 200 and resp.read() == b"local HTTPS via CONNECT", "CONNECT + actual TLS origin")
    finally:
        conn.close()

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        work = [pool.submit(tcp_halfclose, mixed, echo_port, bytes([n]) + BLOB[:4096*n]) for n in range(1, 5)]
        for future in work:
            future.result()
    with socket.create_connection((HOST, mixed), TIMEOUT) as control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        bound = socks_request(control, 3, b"\x01" + b"\0" * 6)
        s.settimeout(TIMEOUT)
        for i, target_port in enumerate([*udp_ports, *reversed(udp_ports)]):
            payload = bytes([i]) + BLOB[:31+i*480]
            prefix = b"\0\0\0" + addr_bytes(target_port)
            s.sendto(prefix + payload, bound)
            wire, _ = s.recvfrom(65535)
            check(wire == prefix + payload, "UDP association lost source, payload or packet boundary")
        # Keep two distinct peers in flight on the same SOCKS UDP association. The first
        # destination replies only after the client has verified the second destination.
        UDP_RELEASE.clear()
        first = b"\0\0\0" + addr_bytes(udp_ports[0]) + b"cs2world-hold-first:1"
        second = b"\0\0\0" + addr_bytes(udp_ports[1]) + b"second peer"
        try:
            s.sendto(first, bound)
            s.sendto(second, bound)
            check(s.recvfrom(65535)[0] == second, "second UDP peer or source lost")
        finally:
            UDP_RELEASE.set()
        check(s.recvfrom(65535)[0] == first, "deferred UDP response sent with a stale source")
    subprocess.run([str(helper), "udp", f"{HOST}:{mixed}", f"{HOST}:{udp_ports[0]}"], check=True, timeout=TIMEOUT+2)


def stop(proc):
    if proc.poll() is None:
        proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def wait_start(client, server, mixed, server_port):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if server.poll() is not None or client.poll() is not None:
            raise RuntimeError("node exited during startup")
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
                s.settimeout(0.2)
                s.sendto(INFO, (HOST, server_port))
                data = s.recv(4096)
                check(data.startswith(PREFIX + b"I"), "unexpected A2S INFO")
                s.sendto(PREFIX + b"U" + PREFIX, (HOST, server_port))
                challenge = s.recv(4096)
                check(len(challenge) == 9 and challenge[:5] == PREFIX+b"A", "A2S PLAYER challenge")
                s.sendto(PREFIX + b"U" + challenge[5:], (HOST, server_port))
                check(s.recv(4096).startswith(PREFIX+b"D"), "A2S PLAYER response")
                for value in (PREFIX, challenge[5:]):
                    s.sendto(PREFIX + b"V" + value, (HOST, server_port))
                    check(s.recv(4096).startswith(PREFIX+b"A"), "A2S RULES challenge")
            with socket.create_connection((HOST, mixed), 0.2):
                return
        except (TimeoutError, OSError):
            time.sleep(0.05)
    raise TimeoutError("nodes did not bind their loopback ports")


def process_usage(pid):
    stats = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
    wanted = {"VmRSS", "Threads"}
    return {key: int(value.split()[0]) for line in stats.splitlines()
            if (parts := line.split(":", 1)) and len(parts) == 2
            for key, value in [parts] if key in wanted}


def measure_linux(server_pid, mixed, echo_port, idle, label):
    workers = 8
    opened = threading.Barrier(workers + 1, timeout=TIMEOUT)
    release = threading.Event()

    def bulk():
        with socket.create_connection((HOST, mixed), TIMEOUT) as stream:
            socks_request(stream, 1, addr_bytes(echo_port))
            opened.wait()
            check(release.wait(TIMEOUT), "benchmark release timed out")
            stream.sendall(BLOB)
            stream.shutdown(socket.SHUT_WR)
            check(exact(stream, len(BLOB)) == BLOB, "benchmark bulk integrity")

    def short(_):
        start = time.monotonic()
        tcp_halfclose(mixed, echo_port, b"x" * 1024)
        return (time.monotonic() - start) * 1000

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        waiting = [pool.submit(bulk) for _ in range(workers)]
        try:
            opened.wait()
            loaded = process_usage(server_pid)
            start = time.monotonic()
        finally:
            release.set()
        for request in waiting:
            request.result()
        elapsed = time.monotonic() - start
        times = sorted(pool.map(short, range(64)))
    duplex_rate = 2 * workers * len(BLOB) / elapsed / (1024 ** 2)
    p50, p99 = [times[math.ceil(len(times) * p) - 1] for p in (.50, .99)]
    print(f"METRICS {label} idle={idle} eight_streams={loaded} (VmRSS KiB), "
          f"duplex_payload_MiB/s={duplex_rate:.2f}, 64x1KiB SOCKS half-close "
          f"p50_ms={p50:.2f} p99_ms={p99:.2f}", flush=True)


def run_pair(work, cli, srv, label, fec, origins, helper, ca, impair):
    mixed, udp = port(), port(socket.SOCK_DGRAM)
    data_shards, parity_shards = fec
    kcp = {"data_shards": data_shards, "parity_shards": parity_shards,
           "mtu": 1180, "snd_wnd": 1024, "rcv_wnd": 1024}
    wire = {"type":"cs2world", "psk":secrets.token_hex(32), "kcp":kcp, "shaper":{"enabled":True}}
    with Relay(udp, impair) as relay, contextlib.ExitStack() as stack:
        server_json = {"inbounds":[dict(wire, listen=HOST, listen_port=udp, tag="node")],
                       "outbounds":[{"type":"direct","tag":"direct"}],"route":{"final":"direct"}}
        client_json = {"inbounds":[{"type":"mixed","listen":HOST,"listen_port":mixed,"tag":"mixed"}],
                       "outbounds":[dict(wire, server=HOST,server_port=relay.port,tag="tunnel")],
                       "route":{"final":"tunnel"}}
        processes, logs = [], []
        for exe, cfg, side in ((srv, server_json, "server"), (cli, client_json, "client")):
            config = work / f"{side}.json"
            config.write_text(json.dumps(cfg), encoding="utf-8")
            log = work / f"{side}.log"
            logs.append(log)
            handle = stack.enter_context(log.open("wb"))
            proc = subprocess.Popen([str(exe), "run", "-c", str(config)], stdout=handle, stderr=subprocess.STDOUT)
            processes.append(proc)
            stack.callback(stop, proc)
        server, client = processes
        try:
            wait_start(client, server, mixed, udp)
            sample = sys.platform.startswith("linux") and label.endswith("go-client") and fec == (10, 3)
            idle = process_usage(server.pid) if sample else None
            test_business(mixed, origins, helper, ca)
            if sample:
                measure_linux(server.pid, mixed, origins[1], idle, label)
            check(all(n > 0 for n in relay.count), "no encrypted transport traffic on node relay")
            check(not impair or relay.dropped > 3, "impairment never exceeded parity budget")
            stop(server)
            try:
                status, body = request_http(mixed, origins[0], "ping", timeout=1)
            except (OSError, http.client.HTTPException, EOFError):
                pass
            else:
                check((status,body)!=(200,b"loopback origin"), "proxy succeeded after node stopped")
        except Exception:
            for proc in processes:
                stop(proc)
            for log in logs:
                print(log.name, log.read_text(encoding="utf-8", errors="replace")[-3000:], file=sys.stderr)
            raise
        profile = f"{data_shards}+{parity_shards}" if data_shards else "off"
        print(f"PASS {label} fec={profile} packets={relay.count} dropped={relay.dropped}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-bin", "--rust", type=Path, help="prebuilt Rust CLI")
    parser.add_argument("--go-bin", "--go", type=Path, help="prebuilt Go reference")
    parser.add_argument("--direction", choices=("both","rust-client","go-client"), default="both")
    parser.add_argument("--fec", choices=("all","both","on","off","4+1"), default="all",
                        help="on selects 10+3, both selects off and 10+3, all also exercises 4+1")
    parser.add_argument("--no-loss", action="store_true", help="disable deterministic five-packet bursts")
    parser.add_argument("--windows-gui", type=Path, help="run the Win32 package smoke test against the same reference")
    opts = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="cs2world-interop-") as tmp, contextlib.ExitStack() as stack:
        work=Path(tmp)
        if opts.rust_bin is None:
            subprocess.run(["cargo","build","--locked","-p","cs2world-server"],cwd=ROOT,check=True)
        rust=(opts.rust_bin or ROOT/"target"/"debug"/("cs2world-server"+EXE)).resolve()
        archive = work / "go-baseline.zip"
        baseline = work / "go-baseline"
        subprocess.run(["git", "archive", "--format=zip", f"--output={archive}", GO_COMMIT,
                        "go.mod", "go.sum", "cmd", "pkg"], cwd=ROOT, check=True)
        with zipfile.ZipFile(archive) as tree:
            tree.extractall(baseline)
        go = (opts.go_bin or work / ("reference" + EXE)).resolve()
        if opts.go_bin is None:
            subprocess.run(["go", "build", "-mod=readonly", "-o", str(go), "./cmd/sing-box-cs2world"],
                           cwd=baseline, check=True)
        for name, path in (("Go binary", go), ("Rust binary", rust), ("go.mod", baseline / "go.mod"),
                           ("go.sum", baseline / "go.sum")):
            with path.open("rb") as data:
                print(f"{name} {path.name} sha256={hashlib.file_digest(data, 'sha256').hexdigest()}", flush=True)
        print("Go source baseline", GO_COMMIT, flush=True)
        source=work/"fixture.go"
        helper=work/("fixture"+EXE)
        source.write_text(GO_HELPER, encoding="utf-8")
        subprocess.run(["go","build","-mod=readonly","-o",str(helper),str(source)],cwd=baseline,check=True)
        ca=work/"origin.pem"
        tls=subprocess.Popen([str(helper),"https",str(ca)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
        stack.callback(stop,tls)
        tls_addr=tls.stdout.readline().strip()
        check(tls_addr, "TLS test origin did not report a port")
        http_port=stack.enter_context(origin(http.server.ThreadingHTTPServer,HTTPOrigin))
        echo_port=stack.enter_context(origin(socketserver.ThreadingTCPServer,Echo))
        udp_ports=[stack.enter_context(origin(socketserver.UDPServer,DatagramEcho)) for _ in range(2)]
        origins=(http_port,echo_port,int(tls_addr.rsplit(":",1)[1]),udp_ports)
        for label,cli,srv in (("rust-client",rust,go),("go-client",go,rust)):
            if opts.direction not in ("both",label):
                continue
            profiles = {"off": (0, 0), "on": (10, 3), "4+1": (4, 1)}
            for name, fec in profiles.items():
                if opts.fec not in ("all", name) and not (opts.fec == "both" and name in ("off", "on")):
                    continue
                run_pair(work,cli,srv,label,fec,origins,helper,ca,not opts.no_loss)
        clients = [("rust-client", rust), ("go-client", go)]
        selected_clients = [(label, exe) for label, exe in clients if opts.direction in ("both", label)]
        runpy.run_path(str(Path(__file__).with_name("shaper.py")))["run"](work, baseline, selected_clients)
        if opts.windows_gui is not None:
            subprocess.run([sys.executable, "-B", str(ROOT/"win32-xp/tests/smoke.py"), "--exe",
                            str(opts.windows_gui.resolve()), "--go", str(go)], cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
