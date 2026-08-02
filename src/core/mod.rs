mod config;
mod logs;
mod paths;
mod supervisor;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub struct Resolution {
    pub path: Option<PathBuf>,
    pub managed: bool,
}

pub use config::{ManagedConfig, atomic_write, tun_privileges_available, validate_tun_privileges};
pub use logs::{LogBuffer, LogEntry};
pub use paths::ManagedPaths;
pub use supervisor::Supervisor;

pub fn resolve_binary(explicit: &str, external: bool, executable: &Path) -> Result<Resolution> {
    if external {
        if !explicit.is_empty() {
            bail!("-core-binary cannot be used with external mode");
        }
        return Ok(Resolution {
            path: None,
            managed: false,
        });
    }

    let path = if explicit.is_empty() {
        executable.with_file_name(executable_name())
    } else {
        std::fs::canonicalize(explicit).unwrap_or_else(|_| PathBuf::from(explicit))
    };
    if path.is_file() {
        Ok(Resolution {
            path: Some(path),
            managed: true,
        })
    } else if explicit.is_empty() {
        Ok(Resolution {
            path: None,
            managed: false,
        })
    } else {
        bail!(
            "managed Core executable was not found at {}",
            path.display()
        )
    }
}

pub fn executable_name() -> &'static str {
    if cfg!(windows) {
        "zju-portal-core.exe"
    } else {
        "zju-portal-core"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bundled_binary() {
        let directory =
            std::env::temp_dir().join(format!("sumire-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let webui = directory.join("sumire");
        let core = directory.join(executable_name());
        std::fs::write(&core, b"binary").unwrap();
        let result = resolve_binary("", false, &webui).unwrap();
        assert!(result.managed);
        assert_eq!(result.path.as_deref(), Some(core.as_path()));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
