pub mod app;
pub mod config;
pub mod domain;
pub mod protocol;
pub mod sys;

pub fn run() -> Result<(), app::error::DaemonError> {
    let module_dir = config::loader::resolve_module_dir(
        std::env::args(),
        std::env::var(config::loader::MODULE_DIR_ENV)
            .ok()
            .as_deref(),
    )?;
    app::controller::run_with_paths(&config::loader::DaemonPaths::from_module_dir(module_dir))
}
