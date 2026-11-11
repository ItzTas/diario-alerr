use std::error::Error;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Duration;

use monitor_hub_plugin::{NOT_LAUNCHED_BY_HUB, launched_by_hub, logging};
use tokio::runtime::{Builder, Runtime};
use tracing::error;

const RUNTIME_SHUTDOWN_LIMIT: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    if !launched_as_plugin() {
        eprintln!("{NOT_LAUNCHED_BY_HUB}");
        return ExitCode::FAILURE;
    }
    if let Err(err) = logging::init("info") {
        eprintln!("cannot start logging: {err}");
        return ExitCode::FAILURE;
    }
    let Some(runtime) = runtime() else {
        return ExitCode::FAILURE;
    };
    let result = runtime.block_on(diario_alerr::app::run());
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_LIMIT);
    let Err(err) = result else {
        return ExitCode::SUCCESS;
    };
    error!(error = &*err as &dyn Error, "fatal");
    ExitCode::FAILURE
}

fn launched_as_plugin() -> bool {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    arguments == ["plugin"] && launched_by_hub()
}

fn runtime() -> Option<Runtime> {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .inspect_err(|err| error!(error = err as &dyn Error, "cannot start the async runtime"))
        .ok()
}
