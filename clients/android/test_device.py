"""Exercise VpnService on a dedicated emulator, using only host loopback endpoints."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import secrets
import socket
import socketserver
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
RUNNER = "org.cs2world.client.test/androidx.test.runner.AndroidJUnitRunner"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--adb", default="adb")
    p.add_argument("--serial", required=True)
    p.add_argument("--go", required=True, type=Path, help="native cs2world Go sing-box reference executable")
    p.add_argument("--artifacts", type=Path, default=ROOT / "dist/rust/android")
    p.add_argument("--report", type=Path, help="Write verified artifact hashes and device results")
    args = p.parse_args()

    def adb(*commands):
        return subprocess.check_output([args.adb, "-s", args.serial, *commands], text=True,
                                       encoding="utf-8", errors="replace", timeout=100).strip()

    if adb("shell", "getprop", "ro.kernel.qemu") != "1" and adb("shell","getprop","ro.boot.qemu") != "1":
        p.error("This fixture requires an Android emulator")
    for apk in ("CS2World-debug.apk", "CS2World-androidTest.apk"):
        print(adb("install","-r","-t",str(args.artifacts / apk)),flush=True)
    app = "org.cs2world.client"
    state = adb("shell","appops","get",app,"ACTIVATE_VPN")
    match = re.search(r"ACTIVATE_VPN:\s*(allow|deny|ignore|default|foreground)",state)
    original = match.group(1) if match else "default"
    token = secrets.token_hex(12)
    payload = ("probe-" + token).encode()
    observed = set()
    passed = {}

    class Tcp(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(5)
            data=b""
            while len(data)<len(payload):
                chunk=self.request.recv(len(payload)-len(data))
                if not chunk: return
                data+=chunk
            if data==payload:
                observed.add("TCP")
                self.request.sendall(data)

    class Udp(socketserver.BaseRequestHandler):
        def handle(self):
            data, sock=self.request
            if data==payload:
                observed.add("UDP")
                sock.sendto(data,self.client_address)

    def instrument(name, extra):
        cmd=["shell","am","instrument","-w","-e","class","org.cs2world.client."+name]
        for k,v in extra.items():
            cmd.extend(["-e",k,str(v)])
        result=adb(*cmd,RUNNER)
        print(result,flush=True)
        if "OK (1 test)" not in result:
            raise RuntimeError(f"{name} failed")
        duration=re.search(r"Time:\s*([\d.]+)",result)
        passed[name]={"passed":True,"seconds":float(duration.group(1)) if duration else None}

    with socketserver.ThreadingTCPServer(("127.0.0.1",0),Tcp) as tcp, socketserver.UDPServer(("127.0.0.1",0),Udp) as udp, tempfile.TemporaryDirectory() as temp:
        for s in (tcp,udp):
            threading.Thread(target=s.serve_forever,daemon=True).start()
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as port:
            port.bind(("127.0.0.1",0));node_port=port.getsockname()[1]
        # A synthetic destination prevents direct-path success. The reference server
        # sends traffic received for that address to test-only loopback echo services.
        kcp={"data_shards":10,"parity_shards":3}
        server={"inbounds":[{"type":"cs2world","listen":"127.0.0.1","listen_port":node_port,"psk":token,"kcp":kcp}],
                "outbounds":[{"type":"direct","tag":"echo"},{"type":"block","tag":"deny"}],
                "route":{"rules":[{"ip_cidr":["198.18.0.42/32"],"action":"route","outbound":"echo",
                                   "override_address":"127.0.0.1","udp_connect":True}],"final":"deny"}}
        client={"inbounds":[{"type":"mixed","listen":"127.0.0.1","listen_port":0}],
                "outbounds":[{"type":"cs2world","tag":"node","server":"10.0.2.2","server_port":node_port,"psk":token,"kcp":kcp}],"route":{"final":"node"}}
        conf=Path(temp)/"server.json";conf.write_text(json.dumps(server))
        subprocess.run([args.go,"check","-c",conf],check=True)
        log=Path(temp)/"server.log"
        with log.open("wb") as output:
            proc=subprocess.Popen([args.go,"run","-c",conf],stdout=output,stderr=output)
            try:
                with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as probe:
                    probe.settimeout(.3)
                    for _ in range(25):
                        probe.sendto(b"\xff\xff\xff\xffTSource Engine Query\0",("127.0.0.1",node_port))
                        try:
                            if probe.recv(2048).startswith(b"\xff\xff\xff\xffI"):break
                        except (TimeoutError,ConnectionResetError):time.sleep(.05)
                    else:raise RuntimeError("Go reference server did not become ready")
                adb("shell","appops","set",app,"ACTIVATE_VPN","allow")
                instrument("NativeBridgeTest",{})
                instrument("VpnDeviceTest",{"config64":base64.b64encode(json.dumps(client).encode()).decode(),
                           "token":token,"tcp":tcp.server_address[1],"udp":udp.server_address[1]})
                assert observed=={"TCP","UDP"},observed
                print("Verified host TCP + UDP origins and Android shell UID 2000 through VpnService.",flush=True)
                if args.report:
                    def digest(path):
                        with path.open("rb") as f:
                            return hashlib.file_digest(f,"sha256").hexdigest()
                    report={"device":args.serial,"api":adb("shell","getprop","ro.build.version.sdk"),
                            "abi":adb("shell","getprop","ro.product.cpu.abi"),
                            "fingerprint":adb("shell","getprop","ro.build.fingerprint"),
                            "go_sha256":digest(args.go),"artifacts":{},"instrumentation":passed,
                            "external_uid":2000,"host_echo":sorted(observed)}
                    for filename in ("CS2World-debug.apk","CS2World-androidTest.apk","CS2World-android-libs.zip"):
                        f=args.artifacts / filename
                        report["artifacts"][filename]={"bytes":f.stat().st_size,"sha256":digest(f)}
                    args.report.parent.mkdir(parents=True,exist_ok=True)
                    args.report.write_text(json.dumps(report,indent=2)+"\n",encoding="utf-8")
            except Exception:
                print(log.read_text(errors="replace")[-8000:],flush=True)
                raise
            finally:
                adb("shell","appops","set",app,"ACTIVATE_VPN",original)
                proc.terminate();proc.wait(timeout=10)
                tcp.shutdown();udp.shutdown()

if __name__ == "__main__":
    main()
