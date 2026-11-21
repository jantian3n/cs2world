#!/usr/bin/env python3
"""
CS2World vs Real CS2 Traffic A/B DPI Analyzer & Profile Extractor.
Streams PCAPNG via mmap so even 500MB+ captures parse in seconds.
"""
import collections
import ipaddress
import json
import math
import mmap
import os
import statistics
import struct
import sys


def is_private_v4(ip_bytes: bytes) -> bool:
    b0, b1 = ip_bytes[0], ip_bytes[1]
    if b0 == 10 or b0 == 127:
        return True
    if b0 == 172 and 16 <= b1 <= 31:
        return True
    if b0 == 192 and b1 == 168:
        return True
    if b0 == 169 and b1 == 254:
        return True
    return False


def shannon_entropy(byte_counts: list, total: int) -> float:
    if total == 0:
        return 0.0
    ent = 0.0
    for c in byte_counts:
        if c:
            p = c / total
            ent -= p * math.log2(p)
    return ent


def ks_statistic(a: list, b: list) -> float:
    """Two-sample Kolmogorov-Smirnov distance in [0, 1] (lower = more similar)."""
    if not a or not b:
        return 1.0
    # Subsample deterministically if huge
    sa = sorted(a[:: max(1, len(a) // 5000)])
    sb = sorted(b[:: max(1, len(b) // 5000)])
    na, nb = len(sa), len(sb)
    i = j = 0
    d = 0.0
    while i < na and j < nb:
        if sa[i] <= sb[j]:
            i += 1
        else:
            j += 1
        diff = abs(i / na - j / nb)
        if diff > d:
            d = diff
    return d


def pct(sorted_arr: list, p: float) -> float:
    if not sorted_arr:
        return 0.0
    idx = min(len(sorted_arr) - 1, int(len(sorted_arr) * p))
    return float(sorted_arr[idx])


def parse_pcapng(path: str, prefer_port: int = None):
    ifaces = []
    flows = collections.defaultdict(lambda: {
        'c2s_sz': [], 's2c_sz': [], 'c2s_ts': [], 's2c_ts': [],
        'c2s_pfx': collections.Counter(), 's2c_pfx': collections.Counter(),
        'c2s_ent': [0] * 256, 'c2s_ent_n': 0,
        's2c_ent': [0] * 256, 's2c_ent_n': 0,
        'a2s': 0,
    })
    total_pkts = 0
    udp_pkts = 0
    tcp_pkts = 0
    min_ts = None
    max_ts = None

    with open(path, 'rb') as f:
        mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        n = len(mm)
        pos = 0
        while pos + 12 <= n:
            btype, blen = struct.unpack_from('<II', mm, pos)
            if blen < 12 or pos + blen > n:
                break
            if btype == 0x00000001:  # IDB
                link_type = struct.unpack_from('<H', mm, pos + 8)[0]
                ts_resol = 1e-6
                opos = pos + 16
                oend = pos + blen - 4
                while opos + 4 <= oend:
                    ocode, olen = struct.unpack_from('<HH', mm, opos)
                    if ocode == 0:
                        break
                    if ocode == 9 and olen >= 1:
                        v = mm[opos + 4]
                        ts_resol = (2.0 ** (-(v & 0x7f))) if (v & 0x80) else (10.0 ** (-v))
                    opos += 4 + ((olen + 3) & ~3)
                ifaces.append((link_type, ts_resol))
            elif btype == 0x00000006:  # EPB
                iface_id, ts_hi, ts_lo, cap_len, orig_len = struct.unpack_from('<IIIII', mm, pos + 8)
                lt, resol = ifaces[iface_id] if iface_id < len(ifaces) else (1, 1e-6)
                ts = ((ts_hi << 32) | ts_lo) * resol
                if min_ts is None or ts < min_ts:
                    min_ts = ts
                if max_ts is None or ts > max_ts:
                    max_ts = ts
                total_pkts += 1
                pstart = pos + 28
                if lt == 1 and cap_len >= 34:
                    eth_type = struct.unpack_from('!H', mm, pstart + 12)[0]
                    if eth_type == 0x0800:
                        ip_off = pstart + 14
                    elif eth_type == 0x8100 and cap_len >= 38 and struct.unpack_from('!H', mm, pstart + 16)[0] == 0x0800:
                        ip_off = pstart + 18
                    else:
                        pos += blen
                        continue
                elif lt in (101, 228) and cap_len >= 20 and (mm[pstart] >> 4) == 4:
                    ip_off = pstart
                else:
                    pos += blen
                    continue

                ihl = (mm[ip_off] & 0x0f) * 4
                proto = mm[ip_off + 9]
                if proto == 6:
                    tcp_pkts += 1
                elif proto == 17 and (ip_off - pstart) + ihl + 8 <= cap_len:
                    udp_pkts += 1
                    src_ip = mm[ip_off + 12 : ip_off + 16]
                    dst_ip = mm[ip_off + 16 : ip_off + 20]
                    sport, dport, ulen = struct.unpack_from('!HHH', mm, ip_off + ihl)
                    plen = max(0, ulen - 8)
                    pay_off = ip_off + ihl + 8
                    avail = min(plen, (pstart + cap_len) - pay_off)
                    src_priv = is_private_v4(src_ip)
                    dst_priv = is_private_v4(dst_ip)
                    if src_priv and not dst_priv:
                        key = (str(ipaddress.IPv4Address(dst_ip)), dport, sport)
                        st = flows[key]
                        st['c2s_sz'].append(plen)
                        st['c2s_ts'].append(ts)
                        if avail >= 1:
                            b0 = mm[pay_off]
                            st['c2s_pfx'][b0] += 1
                            if avail >= 4 and mm[pay_off : pay_off + 4] == b'\xff\xff\xff\xff':
                                st['a2s'] += 1
                            if len(st['c2s_sz']) <= 1500 and avail > 12:
                                ent_arr = st['c2s_ent']
                                slice_end = pay_off + min(avail, 128)
                                for b in mm[pay_off + 9 : slice_end]:
                                    ent_arr[b] += 1
                                st['c2s_ent_n'] += slice_end - (pay_off + 9)
                    elif dst_priv and not src_priv:
                        key = (str(ipaddress.IPv4Address(src_ip)), sport, dport)
                        st = flows[key]
                        st['s2c_sz'].append(plen)
                        st['s2c_ts'].append(ts)
                        if avail >= 1:
                            b0 = mm[pay_off]
                            st['s2c_pfx'][b0] += 1
                            if avail >= 4 and mm[pay_off : pay_off + 4] == b'\xff\xff\xff\xff':
                                st['a2s'] += 1
                            if len(st['s2c_sz']) <= 1500 and avail > 12:
                                ent_arr = st['s2c_ent']
                                slice_end = pay_off + min(avail, 128)
                                for b in mm[pay_off + 9 : slice_end]:
                                    ent_arr[b] += 1
                                st['s2c_ent_n'] += slice_end - (pay_off + 9)
            pos += blen
        mm.close()

    # Select dominant UDP flow
    candidates = list(flows.items())
    if prefer_port is not None:
        port_matches = [kv for kv in candidates if kv[0][1] == prefer_port]
        if port_matches:
            candidates = port_matches
    candidates.sort(key=lambda kv: len(kv[1]['c2s_sz']) + len(kv[1]['s2c_sz']), reverse=True)
    return {
        'total_pkts': total_pkts,
        'tcp_pkts': tcp_pkts,
        'udp_pkts': udp_pkts,
        'duration': (max_ts - min_ts) if (min_ts and max_ts) else 0.0,
        'top_flows': candidates[:5],
    }


def summarize_dir(sizes: list, timestamps: list, pfx: collections.Counter, ent_counts: list, ent_n: int):
    if not sizes:
        return {}
    ssz = sorted(sizes)
    dur = (timestamps[-1] - timestamps[0]) if len(timestamps) > 1 else 0.0
    iats = [(timestamps[i] - timestamps[i - 1]) * 1000.0 for i in range(1, len(timestamps)) if 0 <= (timestamps[i] - timestamps[i - 1]) < 2.0]
    siat = sorted(iats)
    in_64hz = sum(1 for x in iats if 13.0 <= x <= 18.5) / len(iats) * 100.0 if iats else 0.0
    burst_1ms = sum(1 for x in iats if x <= 1.0) / len(iats) * 100.0 if iats else 0.0
    total_bytes = sum(sizes)
    return {
        'pkts': len(sizes),
        'bytes': total_bytes,
        'dur_s': round(dur, 2),
        'pps': round(len(sizes) / dur, 1) if dur > 0 else 0.0,
        'kbps': round(total_bytes * 8.0 / 1000.0 / dur, 1) if dur > 0 else 0.0,
        'sz_min': ssz[0],
        'sz_p10': int(pct(ssz, 0.10)),
        'sz_med': int(pct(ssz, 0.50)),
        'sz_mean': round(statistics.mean(sizes), 1),
        'sz_std': round(statistics.pstdev(sizes), 1),
        'sz_p90': int(pct(ssz, 0.90)),
        'sz_p99': int(pct(ssz, 0.99)),
        'sz_max': ssz[-1],
        'iat_p10_ms': round(pct(siat, 0.10), 2),
        'iat_med_ms': round(pct(siat, 0.50), 2),
        'iat_mean_ms': round(statistics.mean(iats), 2) if iats else 0.0,
        'iat_p90_ms': round(pct(siat, 0.90), 2),
        'pct_in_64hz_window': round(in_64hz, 1),
        'pct_burst_le_1ms': round(burst_1ms, 1),
        'entropy_bits': round(shannon_entropy(ent_counts, ent_n), 3),
        'top_lead_bytes': {f'0x{k:02x}': v for k, v in pfx.most_common(8)},
    }


def timeline_buckets(st: dict, bucket_s: float = 10.0):
    t0 = min(st['c2s_ts'][0] if st['c2s_ts'] else 1e18, st['s2c_ts'][0] if st['s2c_ts'] else 1e18)
    t1 = max(st['c2s_ts'][-1] if st['c2s_ts'] else 0, st['s2c_ts'][-1] if st['s2c_ts'] else 0)
    nb = max(1, int((t1 - t0) / bucket_s) + 1)
    c_b = [[] for _ in range(nb)]
    s_b = [[] for _ in range(nb)]
    for sz, ts in zip(st['c2s_sz'], st['c2s_ts']):
        idx = min(nb - 1, int((ts - t0) / bucket_s))
        c_b[idx].append(sz)
    for sz, ts in zip(st['s2c_sz'], st['s2c_ts']):
        idx = min(nb - 1, int((ts - t0) / bucket_s))
        s_b[idx].append(sz)
    rows = []
    for i in range(nb):
        cu, cd = c_b[i], s_b[i]
        rows.append((
            f'{int(i*bucket_s):3d}-{int((i+1)*bucket_s):3d}s',
            len(cu), int(statistics.mean(cu)) if cu else 0, max(cu) if cu else 0,
            len(cd), int(statistics.mean(cd)) if cd else 0, max(cd) if cd else 0,
            round(sum(cd) / max(1, sum(cu)), 2),
        ))
    return rows


def build_histogram(sizes: list, bin_width: int = 64, max_val: int = 1280):
    bins = [0] * (max_val // bin_width + 1)
    for s in sizes:
        idx = min(len(bins) - 1, s // bin_width)
        bins[idx] += 1
    tot = max(1, len(sizes))
    return [
        {'range': f'{i*bin_width}-{(i+1)*bin_width-1}', 'prob': round(c / tot, 5)}
        for i, c in enumerate(bins) if c > 0
    ]


def main():
    real_path = sys.argv[1] if len(sys.argv) > 1 else r'C:\Users\lan\Desktop\cs2world-v2rayN\cs2-real.pcapng'
    test_path = sys.argv[2] if len(sys.argv) > 2 else r'C:\Users\lan\Desktop\cs2world-v2rayN\cs2world-test.pcapng'
    profile_out = sys.argv[3] if len(sys.argv) > 3 else r'C:\Users\lan\Desktop\cs2world-v2rayN\cs2world-src\configs\cs2_profile.json'

    print(f'[1/2] Parsing Real CS2: {real_path}')
    real = parse_pcapng(real_path)
    print(f'[2/2] Parsing CS2World: {test_path}')
    test = parse_pcapng(test_path, prefer_port=27015)

    print('\n=== Top UDP Flows in cs2world-test.pcapng ===')
    for (rip, rport, lport), st in test['top_flows']:
        print(f'  {rip}:{rport} (local {lport}) | up={len(st["c2s_sz"])} dn={len(st["s2c_sz"])}')

    r_key, r_st = real['top_flows'][0]
    t_key, t_st = test['top_flows'][0]

    r_up = summarize_dir(r_st['c2s_sz'], r_st['c2s_ts'], r_st['c2s_pfx'], r_st['c2s_ent'], r_st['c2s_ent_n'])
    r_dn = summarize_dir(r_st['s2c_sz'], r_st['s2c_ts'], r_st['s2c_pfx'], r_st['s2c_ent'], r_st['s2c_ent_n'])
    t_up = summarize_dir(t_st['c2s_sz'], t_st['c2s_ts'], t_st['c2s_pfx'], t_st['c2s_ent'], t_st['c2s_ent_n'])
    t_dn = summarize_dir(t_st['s2c_sz'], t_st['s2c_ts'], t_st['s2c_pfx'], t_st['s2c_ent'], t_st['s2c_ent_n'])

    ks_up = ks_statistic(r_st['c2s_sz'], t_st['c2s_sz'])
    ks_dn = ks_statistic(r_st['s2c_sz'], t_st['s2c_sz'])

    print('\n================ A/B COMPARISON ================')
    print(f'Real CS2 Flow : port {r_key[1]} | dur={r_up.get("dur_s")}s | pkts={r_up["pkts"]}+{r_dn["pkts"]}')
    print(f'CS2World Flow : port {t_key[1]} | dur={t_up.get("dur_s")}s | pkts={t_up["pkts"]}+{t_dn["pkts"]}')
    print(f'KS Size Distance (0=identical, 1=disjoint): Up={ks_up:.3f}, Down={ks_dn:.3f}')
    print(f'Down/Up Pkt Ratio : Real={r_dn["pkts"]/max(1,r_up["pkts"]):.2f}x  vs  CS2World={t_dn["pkts"]/max(1,t_up["pkts"]):.2f}x')
    print(f'Down/Up Byte Ratio: Real={r_dn["bytes"]/max(1,r_up["bytes"]):.2f}x  vs  CS2World={t_dn["bytes"]/max(1,t_up["bytes"]):.2f}x')

    print('\n--- UPLINK (C -> S) ---')
    print('  Real CS2 :', json.dumps(r_up, ensure_ascii=False))
    print('  CS2World :', json.dumps(t_up, ensure_ascii=False))

    print('\n--- DOWNLINK (S -> C) ---')
    print('  Real CS2 :', json.dumps(r_dn, ensure_ascii=False))
    print('  CS2World :', json.dumps(t_dn, ensure_ascii=False))

    print('\n--- CS2World 10s Timeline (Up/Dn pkts, avgB, maxB, Dn/Up bytes) ---')
    for row in timeline_buckets(t_st, 10.0):
        print(f'  {row[0]} | Up: {row[1]:5d} pkts (avg {row[2]:4d}B max {row[3]:4d}B) | Dn: {row[4]:6d} pkts (avg {row[5]:4d}B max {row[6]:4d}B) | Dn/Up={row[7]:.2f}x')

    # Save sanitized real CS2 profile
    profile = {
        'schema_version': '1.0',
        'source': 'real CS2 Valve GNS/SDR match capture (sanitized statistics only)',
        'tick_rate_hz': 64,
        'gns_wire_lead_bytes': {
            'challenge_request': '0x20',
            'challenge_reply': '0x21',
            'connect_request': '0x22',
            'connect_ok': '0x23',
            'connection_closed': '0x24',
            'no_connection': '0x25',
            'data_frame_primary': '0x82',
            'data_frame_alt': '0x83',
        },
        'uplink': {
            'max_udp_payload': r_up['sz_max'],
            'recommended_mtu_cap': 1069,
            'mean_bytes': r_up['sz_mean'],
            'median_bytes': r_up['sz_med'],
            'std_bytes': r_up['sz_std'],
            'p10_bytes': r_up['sz_p10'],
            'p90_bytes': r_up['sz_p90'],
            'iat_median_ms': r_up['iat_med_ms'],
            'iat_p10_ms': r_up['iat_p10_ms'],
            'iat_p90_ms': r_up['iat_p90_ms'],
            'size_histogram_64b': build_histogram(r_st['c2s_sz'], 64, 1280),
        },
        'downlink': {
            'max_udp_payload': r_dn['sz_max'],
            'recommended_mtu_cap': 1257,
            'mean_bytes': r_dn['sz_mean'],
            'median_bytes': r_dn['sz_med'],
            'std_bytes': r_dn['sz_std'],
            'p10_bytes': r_dn['sz_p10'],
            'p90_bytes': r_dn['sz_p90'],
            'iat_median_ms': r_dn['iat_med_ms'],
            'iat_p10_ms': r_dn['iat_p10_ms'],
            'iat_p90_ms': r_dn['iat_p90_ms'],
            'burst_le_1ms_pct': r_dn['pct_burst_le_1ms'],
            'size_histogram_64b': build_histogram(r_st['s2c_sz'], 64, 1280),
        },
    }
    os.makedirs(os.path.dirname(profile_out), exist_ok=True)
    with open(profile_out, 'w', encoding='utf-8') as f:
        json.dump(profile, f, indent=2, ensure_ascii=False)
    print(f'\n[OK] Exported sanitized real CS2 profile -> {profile_out}')


if __name__ == '__main__':
    main()
