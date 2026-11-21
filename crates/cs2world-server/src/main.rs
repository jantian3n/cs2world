use cs2world_core::{config::Config, service::Service};
use std::{env, fs, io, path::Path};

#[cfg(all(windows, feature = "windows-service"))]
mod windows_service;

fn load(path: &Path) -> io::Result<Config> {
    Config::from_json(&fs::read_to_string(path)?)
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("cs2world-server: {err}");
        std::process::exit(1);
    }
}

async fn run() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        #[cfg(all(windows, feature = "windows-service"))]
        [cmd, flag, file] if cmd == "service" && flag == "-c" => {
            windows_service::dispatch(Path::new(file).canonicalize()?)?
        }
        [cmd] if cmd == "version" || cmd == "--version" => {
            println!("cs2world-server {} (wire 0.0.1)", env!("CARGO_PKG_VERSION"));
        }
        [cmd, flag, file] if (cmd == "check" || cmd == "run") && flag == "-c" => {
            let path = Path::new(file);
            let cfg = load(path)?;
            if cmd == "check" {
                println!("Configuration is valid.");
            } else {
                let mut server = Service::start(cfg).await?;
                println!("Listening on {}", server.local_addr());
                signals(&mut server, path).await?;
                server.stop().await;
            }
        }
        [cmd] if cmd == "--help" || cmd == "-h" => {
            println!("cs2world-server check -c FILE | run -c FILE | version");
            #[cfg(all(windows, feature = "windows-service"))]
            println!("cs2world-server service -c FILE (Windows service CS2WorldCore)");
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: cs2world-server check -c FILE | run -c FILE | version",
            ))
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn signals(s: &mut Service, path: &Path) -> io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut hup = signal(SignalKind::hangup())?;
    loop {
        tokio::select! {
            _=term.recv()=>return Ok(()),
            res=tokio::signal::ctrl_c()=>return res,
            _=hup.recv()=>{
                let result=match load(path){Ok(c)=>s.reload(c).await,Err(e)=>Err(e)};
                match result{Ok(())=>eprintln!("Configuration reloaded."),Err(e)=>eprintln!("Reload failed: {e}")}
            }
        }
    }
}

#[cfg(not(unix))]
async fn signals(_s: &mut Service, _path: &Path) -> io::Result<()> {
    tokio::signal::ctrl_c().await
}
