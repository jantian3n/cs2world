use super::*;
fn wire(name: &str, reply: bool) -> Vec<u8> {
    let mut p = vec![
        0,
        9,
        if reply { 0x81 } else { 1 },
        0,
        0,
        1,
        0,
        u8::from(reply),
        0,
        0,
        0,
        0,
    ];
    for l in name.split('.') {
        p.push(l.len() as u8);
        p.extend_from_slice(l.as_bytes());
    }
    p.extend_from_slice(&[0, 0, 1, 0, 1]);
    if reply {
        p.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 90, 0, 4, 192, 0, 2, 99]);
    }
    p
}
#[test]
fn scope_accepts_only_matching_dns_and_discards_shared_ips() {
    let resolver = "192.0.2.53:53".parse().unwrap();
    let src = "10.0.0.2".parse().unwrap();
    let local = "127.0.0.1:1080".parse().unwrap();
    let peer = "127.0.0.1:31234".parse().unwrap();
    let cache = Cache::default();
    let mut requests = Pending::new();
    let lease = Scope {
        src,
        cache: cache.clone(),
        name: None,
    }
    .register((local, peer));
    query(&mut requests, resolver, &wire("one.cn", false));
    answer(&cache, &mut requests, src, resolver, &wire("two.cn", true));
    assert!(lookup(local, peer, "192.0.2.99").is_none());
    answer(&cache, &mut requests, src, resolver, &wire("one.cn", true));
    assert_eq!(lookup(local, peer, "192.0.2.99").as_deref(), Some("one.cn"));
    query(&mut requests, resolver, &wire("two.cn", false));
    answer(&cache, &mut requests, src, resolver, &wire("two.cn", true));
    assert!(lookup(local, peer, "192.0.2.99").is_none());
    drop(lease);
    assert!(lookup(local, peer, "192.0.2.99").is_none());
}
#[test]
fn plaintext_host_and_tls_server_name() {
    assert_eq!(
        hostname(b"GET / HTTP/1.1\r\nHost: MAIL.example.CN:80\r\n\r\n", 80).as_deref(),
        Some("mail.example.cn")
    );
    let name = b"shop.example.cn";
    let mut names = vec![0, 0, name.len() as u8];
    names.extend_from_slice(name);
    let mut ext = vec![0, 0, 0, (names.len() + 2) as u8, 0, names.len() as u8];
    ext.extend(names);
    let mut hello = vec![3, 3];
    hello.extend([7; 32]);
    hello.extend([0, 0, 2, 0x13, 1, 1, 0, 0, ext.len() as u8]);
    hello.extend(ext);
    let mut record = vec![
        22,
        3,
        1,
        0,
        (hello.len() + 4) as u8,
        1,
        0,
        0,
        hello.len() as u8,
    ];
    record.extend(hello);
    assert_eq!(hostname(&record, 443).as_deref(), Some("shop.example.cn"));
    assert!(hostname(&record[..8], 443).is_none());
}
