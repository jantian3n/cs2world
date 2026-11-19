#!/usr/bin/env python3
"""
CS2World Share URI (cs2world://) <-> sing-box Outbound / Client JSON converter.

Usage:
  python cs2world-link.py encode <client.json|outbound.json> [--remark NAME]
  python cs2world-link.py decode "cs2world://psk@host:27015?ds=10&ps=3#Remark"
"""

import argparse
import json
import sys
from urllib.parse import parse_qs, quote, unquote, urlparse

DEFAULTS = {
    "ds": 10,
    "ps": 3,
    "mtu": 1180,
    "sw": 1024,
    "rw": 1024,
    "shaper": True,
    "tick": 64,
    "sleep": 600,
    "combat": 180,
    "fastdl": 1248,
}


def encode_json(data: dict, remark_override: str | None = None) -> str:
    outbound = data
    if "outbounds" in data and isinstance(data["outbounds"], list):
        matches = [o for o in data["outbounds"] if isinstance(o, dict) and o.get("type") == "cs2world"]
        if not matches:
            raise ValueError("No outbound with type='cs2world' found in JSON")
        outbound = matches[0]

    if outbound.get("type") != "cs2world":
        raise ValueError(f"Expected outbound type='cs2world', got {outbound.get('type')!r}")

    server = str(outbound.get("server", "")).strip()
    port = int(outbound.get("server_port", 27015))
    psk = str(outbound.get("psk", "")).strip()
    if not server or not psk or not (1 <= port <= 65535):
        raise ValueError("Missing valid server, server_port, or psk")

    host_fmt = f"[{server}]" if ":" in server and not server.startswith("[") else server
    kcp = outbound.get("kcp") or {}
    shaper = outbound.get("shaper") or {}

    params = []
    for qk, jk in (
        ("ds", "data_shards"),
        ("ps", "parity_shards"),
        ("mtu", "mtu"),
        ("sw", "snd_wnd"),
        ("rw", "rcv_wnd"),
    ):
        if jk in kcp and int(kcp[jk]) != DEFAULTS[qk]:
            params.append(f"{qk}={int(kcp[jk])}")

    if "enabled" in shaper and bool(shaper["enabled"]) != DEFAULTS["shaper"]:
        params.append(f"shaper={'1' if shaper['enabled'] else '0'}")

    for qk, jk in (
        ("tick", "tick_rate_hz"),
        ("sleep", "idle_sleep_seconds"),
        ("combat", "combat_mean_bytes"),
        ("fastdl", "fastdl_chunk_size"),
    ):
        if jk in shaper and int(shaper[jk]) != DEFAULTS[qk]:
            params.append(f"{qk}={int(shaper[jk])}")

    remark = remark_override if remark_override is not None else str(outbound.get("tag") or f"cs2world_{server}:{port}")
    query = ("?" + "&".join(params)) if params else ""
    frag = ("#" + quote(remark, safe="")) if remark else ""
    return f"cs2world://{quote(psk, safe='')}@{host_fmt}:{port}{query}{frag}"


def _pick_int(qs: dict, default: int, *keys: str) -> int:
    for k in keys:
        if k in qs and qs[k]:
            try:
                return int(qs[k][0])
            except ValueError:
                pass
    return default


def _pick_bool(qs: dict, default: bool, *keys: str) -> bool:
    for k in keys:
        if k in qs and qs[k]:
            v = qs[k][0].strip().lower()
            if v in ("1", "true", "yes", "on"):
                return True
            if v in ("0", "false", "no", "off"):
                return False
    return default


def decode_uri(uri: str) -> dict:
    parsed = urlparse(uri.strip())
    if parsed.scheme.lower() != "cs2world":
        raise ValueError(f"Unsupported scheme: {parsed.scheme!r}")
    psk = unquote(parsed.username or "").strip()
    server = (parsed.hostname or "").strip()
    port = parsed.port or 27015
    if not psk or not server or not (1 <= port <= 65535):
        raise ValueError("Invalid cs2world:// URI (requires psk@host:port)")

    qs = parse_qs(parsed.query)
    remark = unquote(parsed.fragment or "").strip() or f"cs2world_{server}:{port}"
    return {
        "type": "cs2world",
        "tag": remark,
        "server": server,
        "server_port": port,
        "psk": psk,
        "kcp": {
            "data_shards": _pick_int(qs, DEFAULTS["ds"], "ds", "data_shards"),
            "parity_shards": _pick_int(qs, DEFAULTS["ps"], "ps", "parity_shards"),
            "mtu": _pick_int(qs, DEFAULTS["mtu"], "mtu"),
            "snd_wnd": _pick_int(qs, DEFAULTS["sw"], "sw", "snd_wnd"),
            "rcv_wnd": _pick_int(qs, DEFAULTS["rw"], "rw", "rcv_wnd"),
        },
        "shaper": {
            "enabled": _pick_bool(qs, DEFAULTS["shaper"], "shaper", "shaper_enabled"),
            "tick_rate_hz": _pick_int(qs, DEFAULTS["tick"], "tick", "tick_hz", "tick_rate_hz"),
            "idle_sleep_seconds": _pick_int(qs, DEFAULTS["sleep"], "sleep", "idle_sleep", "idle_sleep_seconds"),
            "combat_mean_bytes": _pick_int(qs, DEFAULTS["combat"], "combat", "combat_bytes", "combat_mean_bytes"),
            "fastdl_chunk_size": _pick_int(qs, DEFAULTS["fastdl"], "fastdl", "fastdl_chunk", "fastdl_chunk_size"),
        },
    }


def main():
    parser = argparse.ArgumentParser(description="Convert between cs2world:// share URIs and sing-box JSON.")
    sub = parser.add_subparsers(dest="cmd", required=True)

    enc = sub.add_parser("encode", help="Encode client.json or outbound.json to cs2world:// URI")
    enc.add_argument("json_file", help="Path to client.json or outbound.json")
    enc.add_argument("--remark", default=None, help="Optional node remark (#fragment)")

    dec = sub.add_parser("decode", help="Decode cs2world:// URI to sing-box outbound JSON")
    dec.add_argument("uri", help="cs2world:// share URI")

    args = parser.parse_args()
    if args.cmd == "encode":
        with open(args.json_file, "r", encoding="utf-8") as f:
            data = json.load(f)
        print(encode_json(data, args.remark))
    else:
        print(json.dumps(decode_uri(args.uri), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
