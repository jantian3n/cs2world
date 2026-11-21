"""Measure authenticated idle records emitted on real UDP sockets by each client."""
import json
import os
import socket
import statistics
import subprocess
import time
from pathlib import Path


def observe(observer, client_bin, work, label, enabled):
    with subprocess.Popen([str(observer)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as peer:
        client = None
        connection = None
        try:
            line = peer.stdout.readline().strip()
            if not line:
                raise RuntimeError("no UDP observer port")
            with socket.socket() as free:
                free.bind(("127.0.0.1", 0))
                port = free.getsockname()[1]
            config = {
                "inbounds": [{"type":"mixed","listen":"127.0.0.1","listen_port":port}],
                "outbounds": [{"type":"cs2world","tag":"node","server":"127.0.0.1",
                    "server_port":int(line),"psk":"loopback-shaper-fixture",
                    "kcp":{"data_shards":0,"parity_shards":0},
                    "shaper":{"enabled":enabled,"tick_rate_hz":64,"idle_sleep_seconds":60}}],
                "route": {"final":"node"},
            }
            path = work / f"shaper-{label}-{int(enabled)}.json"
            path.write_text(json.dumps(config),encoding="utf-8")
            client = subprocess.Popen([str(client_bin),"run","-c",str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            end = time.monotonic() + 7
            while time.monotonic() < end and client.poll() is None:
                try:
                    connection = socket.create_connection(("127.0.0.1",port), .3)
                    break
                except OSError:
                    time.sleep(.05)
            if not connection:
                raise RuntimeError("client Mixed listener did not start")
            # This creates a stream and a real KCP payload. The observer does not
            # acknowledge KCP; authenticated retransmissions are excluded by mode.
            connection.sendall(b"CONNECT 127.0.0.1:1 HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n")
            rest, error = peer.communicate(timeout=14)
            if peer.returncode:
                raise RuntimeError(f"Go wire observer failed: {error}")
        finally:
            if connection:
                connection.close()
            if client:
                client.terminate()
                client.wait(timeout=5)
            if peer.poll() is None:
                peer.kill()
                peer.wait()
    frames=json.loads(rest)
    real=[f for f in frames if f["Mode"]!=0]
    dummy=[f for f in frames if f["Mode"]==0]
    if not real or any(f["Payload"]<=0 for f in real):
        raise AssertionError("no real KCP segment reached the authenticated observer")
    if not enabled:
        if dummy or any(f["Wire"]!=f["Payload"]+28 or f["Mode"]!=1 for f in real):
            raise AssertionError("disabled shaper added padding, dummy packets or changed the mode")
        print(f"{label} shaper OFF: {len(real)} real datagrams, no padding or idle records")
        return
    if len(dummy)<10 or any(not (54<=f["Wire"]<=108 and f["Payload"]==0) for f in dummy):
        raise AssertionError("idle count, mode, payload or size differs from the reference")
    gaps=[b["Millis"]-a["Millis"] for a,b in zip(dummy,dummy[1:])]
    median=statistics.median(gaps)
    # A real retransmission suppresses a poll. Use the median to tolerate those
    # missed slots and host scheduling jitter without accepting a 15.625 ms tick.
    if not 32<=median<=66:
        raise AssertionError(f"idle median {median:.1f} ms outside 46 ms scheduling envelope")
    sizes=[f["Wire"] for f in dummy]
    print(f"{label} shaper ON: {len(dummy)} authenticated mode-0 records, {min(sizes)}-{max(sizes)} B, median {median:.1f} ms")


def run(work, baseline, clients):
    program = Path(__file__).with_name("shaper_observer.go")
    observer = work / ("shaper-observer.exe" if os.name=="nt" else "shaper-observer")
    subprocess.run(["go","build","-mod=readonly","-o",str(observer),str(program)],cwd=baseline,check=True)
    for label, executable in clients:
        for enabled in (False,True):
            observe(observer, executable, work, label, enabled)
