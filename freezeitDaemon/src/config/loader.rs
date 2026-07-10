use std::{fs, path::Path};

use crate::app::error::DaemonError;

pub const DEFAULT_MODULE_DIR: &str = "/data/adb/modules/freezeit";
pub const MODULE_DIR_ENV: &str = "FREEZEIT_MODULE_DIR";

pub fn resolve_module_dir<I, S>(
    args: I,
    env_module_dir: Option<&str>,
) -> Result<String, DaemonError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    while let Some(argument) = args.next() {
        if argument.as_ref() == "--module-dir" {
            let value = args
                .next()
                .ok_or_else(|| DaemonError::config("--module-dir requires a non-empty path"))?;
            let value = value.as_ref().trim();
            if value.is_empty() {
                return Err(DaemonError::config(
                    "--module-dir requires a non-empty path",
                ));
            }
            return Ok(value.to_owned());
        }
    }
    Ok(env_module_dir
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_MODULE_DIR)
        .to_owned())
}

pub fn load_initial_config() -> Result<(), DaemonError> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    pub module_dir: String,
    pub app_config: String,
    pub app_label: String,
    pub settings_db: String,
    pub rom_baseline: String,
    pub verified_targets: String,
}

impl DaemonPaths {
    pub fn from_module_dir(module_dir: impl Into<String>) -> Self {
        let module_dir = module_dir.into();
        Self {
            app_config: format!("{module_dir}/appcfg.txt"),
            app_label: format!("{module_dir}/applabel.txt"),
            settings_db: format!("{module_dir}/settings.db"),
            rom_baseline: format!("{module_dir}/rom_baseline.prop"),
            verified_targets: format!("{module_dir}/verified_targets.txt"),
            module_dir,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPolicyFiles {
    pub app_config: Option<String>,
    pub app_label: Option<String>,
    pub settings: Option<Vec<u8>>,
}

impl LoadedPolicyFiles {
    pub fn is_available(&self) -> bool {
        self.app_config.is_some() || self.app_label.is_some() || self.settings.is_some()
    }
}

pub fn load_policy_files(paths: &DaemonPaths) -> Result<LoadedPolicyFiles, DaemonError> {
    Ok(LoadedPolicyFiles {
        app_config: read_optional_text(&paths.app_config)?,
        app_label: read_optional_text(&paths.app_label)?,
        settings: read_optional_bytes(&paths.settings_db)?,
    })
}

pub fn load_policy_files_recovering(paths: &DaemonPaths) -> LoadedPolicyFiles {
    LoadedPolicyFiles {
        app_config: read_optional_text(&paths.app_config).ok().flatten(),
        app_label: read_optional_text(&paths.app_label).ok().flatten(),
        settings: read_optional_bytes(&paths.settings_db).ok().flatten(),
    }
}

pub fn serialize_manager_app_config(lines: &[String]) -> Vec<u8> {
    lines.join("\n").into_bytes()
}

pub fn parse_manager_app_config(payload: &[u8]) -> Result<Vec<String>, DaemonError> {
    let text = std::str::from_utf8(payload)
        .map_err(|error| DaemonError::config(format!("app config is not utf-8: {error}")))?;
    Ok(text
        .lines()
        .filter_map(crate::config::migration::normalize_legacy_line)
        .collect())
}

fn read_optional_text(path: impl AsRef<Path>) -> Result<Option<String>, DaemonError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }

    Ok(Some(fs::read_to_string(path)?))
}

fn read_optional_bytes(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>, DaemonError> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(None);
    }

    Ok(Some(fs::read(path)?))
}
