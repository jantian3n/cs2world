use cs2world_core::service::Service;
use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::{watch, Notify},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const NAME: &str = "CS2WorldCore";
static START: OnceLock<(PathBuf, Handle)> = OnceLock::new();
define_windows_service!(service_entry, main_service);

pub(super) fn dispatch(config: PathBuf) -> io::Result<()> {
    START
        .set((config, Handle::current()))
        .map_err(|_| io::Error::other("service dispatcher already started"))?;
    service_dispatcher::start(NAME, service_entry).map_err(io::Error::other)
}

fn state(s: ServiceState, code: u32) -> ServiceStatus {
    let pending = matches!(s, ServiceState::StartPending | ServiceState::StopPending);
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: s,
        controls_accepted: if s == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::PARAM_CHANGE
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: u32::from(pending),
        wait_hint: if pending {
            Duration::from_secs(10)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}
fn main_service(_: Vec<OsString>) {
    let Some((config, runtime)) = START.get() else {
        return;
    };
    let (stop, mut stopped) = watch::channel(false);
    let changed = Arc::new(Notify::new());
    let reload = changed.clone();
    let Ok(status) = service_control_handler::register(NAME, move |control| {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop.send_replace(true);
            }
            ServiceControl::ParamChange => reload.notify_one(),
            ServiceControl::Interrogate => {}
            _ => return ServiceControlHandlerResult::NotImplemented,
        }
        ServiceControlHandlerResult::NoError
    }) else {
        return;
    };
    let _ = status.set_service_status(state(ServiceState::StartPending, 0));
    let result=runtime.block_on(async{
        let mut service=Service::start(super::load(config)?).await?;
        status.set_service_status(state(ServiceState::Running,0)).map_err(io::Error::other)?;
        loop{
            tokio::select!{
                _=stopped.changed()=>break,
                _=changed.notified()=>{if let Ok(cfg)=super::load(config){let _=service.reload(cfg).await;}}
            }
        }
        let _=status.set_service_status(state(ServiceState::StopPending,0));
        service.stop().await;
        Ok::<(),io::Error>(())
    });
    let code = result
        .err()
        .map_or(0, |e| e.raw_os_error().unwrap_or(1066) as u32);
    let _ = status.set_service_status(state(ServiceState::Stopped, code));
}
