//! Runs the packaged GUI's embedded client in isolation for repeatable interoperability checks.
use crate::{
    core_manager::{read_state, selected},
    models::PersistedState,
    singbox_config::core_config,
};
use cs2world_core::{
    config::Config,
    route::{Action, RouteTable},
    service::Service,
};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
};

pub fn run_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.to_str() != Some("--test-node") {
        return None;
    }
    let result = (|| {
        let state = PathBuf::from(
            args.next()
                .ok_or_else(|| io::Error::other("expected state file"))?,
        );
        let port = args
            .next()
            .map(|p| p.to_string_lossy().parse::<u16>())
            .transpose()
            .map_err(|_| io::Error::other("invalid local port"))?
            .unwrap_or(0);
        if args.next().is_some() {
            return Err(io::Error::other("unexpected argument"));
        }
        let state = read_state(&state)?;
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?
            .block_on(serve(&state, port))
    })();
    if let Err(e) = result {
        eprintln!("Node verification failed ({:?})", e.kind());
        return Some(1);
    }
    Some(0)
}
async fn serve(state: &PersistedState, port: u16) -> io::Result<()> {
    let node =
        selected(state).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no active node"))?;
    let mut settings = state.settings.clone();
    settings.proxy_mode = "manual".into();
    settings.mixed_port = port;
    let mut cfg = core_config(node, &settings)?;
    if let Config::Client(c) = &mut cfg {
        // Direct/private bypass rules would mask a broken tunnel in a localhost echo test.
        c.routes = RouteTable {
            rules: vec![],
            fallback: Action::Proxy,
        };
    }
    let probe = cs2world_core::probe::probe(&node.server, node.server_port).await;
    match probe {
        Ok(p) => println!("A2S app_id={} elapsed_ms={}", p.info.app_id, p.rtt_ms),
        Err(e) => println!("A2S unavailable ({:?})", e.kind()),
    }
    let mut service = Service::start(cfg).await?;
    if let Err(e) = service.preconnect().await {
        service.stop().await;
        return Err(e);
    }
    println!("READY {}", service.local_addr().port());
    io::stdout().flush()?;
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = io::stdin().lock().read_line(&mut line);
    })
    .await?;
    let stats = service.snapshot();
    println!(
        "STOP up_bytes={} down_bytes={}",
        stats.up_total, stats.down_total
    );
    service.stop().await;
    Ok(())
}
