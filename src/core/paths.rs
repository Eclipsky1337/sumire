use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct ManagedPaths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub resume: PathBuf,
    pub token: PathBuf,
}

impl ManagedPaths {
    pub fn resolve(data: &Path, config: Option<&Path>, resume: Option<&Path>) -> Result<Self> {
        let data = absolute(data).context("resolve data directory")?;
        let config = absolute(config.unwrap_or(&data.join("config.yaml")))
            .context("resolve configuration path")?;
        let resume = absolute(resume.unwrap_or(&data.join("resume-state.json")))
            .context("resolve Resume State path")?;
        let token = data.join("control.token");
        Ok(Self {
            data,
            config,
            resume,
            token,
        })
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
