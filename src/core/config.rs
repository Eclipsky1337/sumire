use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde_json::Value as JsonValue;
use yaml_edit::{Mapping, MappingBuilder, YamlFile};

use super::ManagedPaths;

const DEFAULT_CONFIG: &[u8] = include_bytes!("../../assets/default-config.yaml");

#[derive(Debug, Clone)]
pub struct ManagedConfig {
    pub paths: ManagedPaths,
    pub core_listen: String,
}

impl ManagedConfig {
    pub fn prepare(&self) -> Result<()> {
        for path in [
            &self.paths.data,
            parent(&self.paths.config)?,
            parent(&self.paths.resume)?,
            parent(&self.paths.token)?,
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("create managed data directory {}", path.display()))?;
            set_directory_mode(path)?;
            restore_path_owner(path)?;
        }
        ensure_token(&self.paths.token)?;
        let input = match fs::read(&self.paths.config) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_vec(),
            Err(error) => return Err(error).context("read managed configuration"),
        };
        let normalized = self.normalize_yaml(&input)?;
        atomic_write(&self.paths.config, &normalized, 0o600)?;
        self.restore_ownership()?;
        Ok(())
    }

    pub fn restore_ownership(&self) -> Result<()> {
        for path in [
            &self.paths.data,
            &self.paths.config,
            &self.paths.resume,
            &self.paths.token,
        ] {
            if path.exists() {
                restore_path_owner(path)?;
            }
        }
        Ok(())
    }

    pub fn token(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.paths.token)
            .context("read managed Core token")?
            .trim()
            .to_owned())
    }

    pub fn normalize_yaml(&self, input: &[u8]) -> Result<Vec<u8>> {
        let source = std::str::from_utf8(input).context("configuration is not UTF-8")?;
        let source = if source.trim().is_empty() {
            "{}\n"
        } else {
            source
        };
        let file = YamlFile::from_str(source).context("decode managed configuration")?;
        let document = file
            .document()
            .ok_or_else(|| anyhow::anyhow!("configuration is empty"))?;
        let root = document
            .as_mapping()
            .ok_or_else(|| anyhow::anyhow!("configuration must be a mapping"))?;
        set_path(&root, &["version"], ManagedValue::Integer(1));
        set_path(
            &root,
            &["control", "rest", "enabled"],
            ManagedValue::Boolean(true),
        );
        set_path(
            &root,
            &["control", "rest", "listen"],
            ManagedValue::String(&self.core_listen),
        );
        set_path(
            &root,
            &["control", "rest", "secret"],
            ManagedValue::String(""),
        );
        set_path(
            &root,
            &["control", "rest", "secret-file"],
            ManagedValue::String(path_text(&self.paths.token)?),
        );
        set_path(
            &root,
            &["state", "resume-file"],
            ManagedValue::String(path_text(&self.paths.resume)?),
        );
        let output = file.to_string();
        let _: serde_yaml::Value =
            serde_yaml::from_str(&output).context("validate normalized configuration")?;
        Ok(output.into_bytes())
    }

    pub fn normalize_json(&self, input: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut config: JsonValue =
            serde_json::from_slice(input).context("decode configuration")?;
        let root = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("configuration must be an object"))?;
        root.insert("version".into(), 1.into());
        let control = object_child(root, "control");
        let rest = object_child(control, "rest");
        rest.insert("enabled".into(), true.into());
        rest.insert("listen".into(), self.core_listen.clone().into());
        rest.remove("secret");
        rest.insert("secret-file".into(), path_text(&self.paths.token)?.into());
        let state = object_child(root, "state");
        state.insert("resume-file".into(), path_text(&self.paths.resume)?.into());
        Ok((
            serde_json::to_vec(&config)?,
            serde_yaml::to_string(&config)?.into_bytes(),
        ))
    }

    pub fn update_routing_yaml(&self, input: &[u8], mode: &str) -> Result<Vec<u8>> {
        if !matches!(mode, "rule" | "global" | "direct") {
            bail!("invalid routing mode {mode:?}");
        }
        self.update_yaml(input, &["routing", "mode"], ManagedValue::String(mode))
    }

    pub fn update_tun_yaml(&self, input: &[u8], enabled: bool) -> Result<Vec<u8>> {
        self.update_yaml(
            input,
            &["inbounds", "tun", "enabled"],
            ManagedValue::Boolean(enabled),
        )
    }

    fn update_yaml(&self, input: &[u8], path: &[&str], value: ManagedValue<'_>) -> Result<Vec<u8>> {
        let normalized = self.normalize_yaml(input)?;
        let file = YamlFile::from_str(std::str::from_utf8(&normalized)?)?;
        let root = file
            .document()
            .and_then(|document| document.as_mapping())
            .ok_or_else(|| anyhow::anyhow!("configuration must be a mapping"))?;
        set_path(&root, path, value);
        let output = file.to_string();
        let _: serde_yaml::Value = serde_yaml::from_str(&output)?;
        Ok(output.into_bytes())
    }

    pub fn tun_enabled(&self) -> Result<bool> {
        let value: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&self.paths.config)?)?;
        Ok(value
            .get("inbounds")
            .and_then(|value| value.get("tun"))
            .and_then(|value| value.get("enabled"))
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false))
    }
}

enum ManagedValue<'a> {
    String(&'a str),
    Boolean(bool),
    Integer(i64),
}

fn set_path(mapping: &Mapping, path: &[&str], value: ManagedValue<'_>) {
    let mut current = mapping.clone();
    for key in &path[..path.len() - 1] {
        if current.get_mapping(*key).is_none() {
            let empty = MappingBuilder::new()
                .build_document()
                .as_mapping()
                .expect("mapping builder");
            current.set(*key, &empty);
        }
        current = current
            .get_mapping(*key)
            .expect("nested mapping was inserted");
    }
    let key = path[path.len() - 1];
    match value {
        ManagedValue::String(value) => current.set(key, value),
        ManagedValue::Boolean(value) => current.set(key, value),
        ManagedValue::Integer(value) => current.set(key, value),
    }
}

fn object_child<'a>(
    parent: &'a mut serde_json::Map<String, JsonValue>,
    key: &str,
) -> &'a mut serde_json::Map<String, JsonValue> {
    if !parent.get(key).is_some_and(JsonValue::is_object) {
        parent.insert(key.into(), JsonValue::Object(Default::default()));
    }
    parent
        .get_mut(key)
        .and_then(JsonValue::as_object_mut)
        .expect("object child")
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("managed path is not valid UTF-8: {}", path.display()))
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| anyhow::anyhow!("managed path has no parent: {}", path.display()))
}

fn ensure_token(path: &Path) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|token| !token.trim().is_empty()) {
        return Ok(());
    }
    let mut random = [0_u8; 32];
    rand::rng().fill_bytes(&mut random);
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    atomic_write(path, token.as_bytes(), 0o600)
}

pub fn atomic_write(path: &Path, data: &[u8], default_mode: u32) -> Result<()> {
    let directory = parent(path)?;
    fs::create_dir_all(directory)?;
    set_directory_mode(directory)?;
    restore_path_owner(directory)?;
    let mode = file_mode(path).unwrap_or(default_mode);
    let mut suffix = [0_u8; 8];
    rand::rng().fill_bytes(&mut suffix);
    let temporary = directory.join(format!(
        ".{}.tmp-{:x}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("managed"),
        u64::from_ne_bytes(suffix)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        set_file_mode(&file, mode)?;
        restore_file_owner(&file, path)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn set_directory_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_directory_mode(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode(file: &fs::File, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode(_: &fs::File, _: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn invoking_owner() -> Option<(u32, u32)> {
    sudo_invoking_owner(
        unsafe { libc::geteuid() },
        std::env::var("SUDO_UID").ok().as_deref(),
        std::env::var("SUDO_GID").ok().as_deref(),
    )
}

#[cfg(unix)]
fn sudo_invoking_owner(
    effective_uid: u32,
    sudo_uid: Option<&str>,
    sudo_gid: Option<&str>,
) -> Option<(u32, u32)> {
    if effective_uid != 0 {
        return None;
    }
    Some((sudo_uid?.parse().ok()?, sudo_gid?.parse().ok()?))
}

#[cfg(unix)]
fn restore_path_owner(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let Some((uid, gid)) = invoking_owner() else {
        return Ok(());
    };
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        bail!(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_path_owner(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restore_file_owner(file: &fs::File, existing: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let owner = invoking_owner().or_else(|| {
        fs::metadata(existing)
            .ok()
            .map(|metadata| (metadata.uid(), metadata.gid()))
    });
    let Some((uid, gid)) = owner else {
        return Ok(());
    };
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
        bail!(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_file_owner(_: &fs::File, _: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn tun_privileges_available() -> bool {
    (unsafe { libc::geteuid() }) == 0
}

#[cfg(windows)]
pub fn tun_privileges_available() -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0;
        let success = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        ) != 0;
        CloseHandle(token);
        success && elevation.TokenIsElevated != 0
    }
}

pub fn validate_tun_privileges() -> Result<()> {
    if tun_privileges_available() {
        return Ok(());
    }
    #[cfg(windows)]
    bail!("TUN is enabled but Sumire is not running as administrator");
    #[cfg(not(windows))]
    bail!("TUN is enabled but Sumire is not running as root; restart with sudo ./sumire")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (ManagedConfig, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "sumire-config-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let paths = ManagedPaths::resolve(&root.join("data"), None, None).unwrap();
        (
            ManagedConfig {
                paths,
                core_listen: "127.0.0.1:9090".into(),
            },
            root,
        )
    }

    #[test]
    fn normalization_preserves_comments() {
        let root = std::env::temp_dir().join(format!("sumire-config-test-{}", std::process::id()));
        let paths = ManagedPaths::resolve(&root, None, None).unwrap();
        let config = ManagedConfig {
            paths,
            core_listen: "127.0.0.1:9090".into(),
        };
        let output = String::from_utf8(
            config
                .normalize_yaml(b"# keep me\nrouting:\n  mode: rule # inline\n")
                .unwrap(),
        )
        .unwrap();
        assert!(output.contains("# keep me"));
        assert!(output.contains("# inline"));
        assert!(output.contains("secret-file:"));
    }

    #[test]
    fn prepares_embedded_configuration_and_secure_files() {
        let (config, root) = fixture("prepare");
        config.prepare().unwrap();
        let text = std::fs::read_to_string(&config.paths.config).unwrap();
        assert!(text.contains("vpn.zju.edu.cn"));
        assert!(text.contains("# 日志级别 info/debug"));
        assert!(text.contains("# 是否创建系统 TUN 设备"));
        assert!(text.find("log:") < text.find("control:"));
        assert!(text.find("control:") < text.find("session:"));
        assert_eq!(config.token().unwrap().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&config.paths.config)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&config.paths.data)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_json_managed_fields() {
        let (config, root) = fixture("json");
        let (json, yaml) = config
            .normalize_json(
                br#"{"version":2,"atrust":{"port":443},"control":{"rest":{"enabled":false,"secret":"old"}},"state":{}}"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["control"]["rest"]["enabled"], true);
        assert_eq!(value["control"]["rest"]["listen"], "127.0.0.1:9090");
        assert!(value["control"]["rest"].get("secret").is_none());
        assert_eq!(value["atrust"]["port"], 443);
        let yaml_value: serde_yaml::Value = serde_yaml::from_slice(&yaml).unwrap();
        assert_eq!(yaml_value["version"].as_i64(), Some(1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn updates_routing_and_tun_values() {
        let (config, root) = fixture("updates");
        let routing = config
            .update_routing_yaml(b"routing:\n  mode: rule\n", "global")
            .unwrap();
        assert!(String::from_utf8(routing).unwrap().contains("mode: global"));
        let tun = config
            .update_tun_yaml(b"inbounds:\n  tun:\n    enabled: false\n", true)
            .unwrap();
        assert!(String::from_utf8(tun).unwrap().contains("enabled: true"));
        assert!(config.update_routing_yaml(b"{}\n", "invalid").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn parses_sudo_invoking_owner() {
        assert_eq!(
            sudo_invoking_owner(0, Some("501"), Some("20")),
            Some((501, 20))
        );
        assert_eq!(sudo_invoking_owner(501, Some("501"), Some("20")), None);
        assert_eq!(sudo_invoking_owner(0, Some("invalid"), Some("20")), None);
        assert_eq!(sudo_invoking_owner(0, None, Some("20")), None);
    }
}
