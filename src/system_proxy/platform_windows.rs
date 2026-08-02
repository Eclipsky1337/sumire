use std::{ffi::c_void, ptr::null_mut};

use anyhow::{Context, Result, bail};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE},
};

use super::{Settings, SystemProxyPlatform};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const INTERNET_OPTION_REFRESH: u32 = 37;
const INTERNET_OPTION_PROXY_SETTINGS_CHANGED: u32 = 95;

#[link(name = "wininet")]
unsafe extern "system" {
    fn InternetSetOptionW(
        handle: *mut c_void,
        option: u32,
        buffer: *mut c_void,
        length: u32,
    ) -> i32;
}

#[derive(Default)]
pub struct Platform;

impl SystemProxyPlatform for Platform {
    fn supported(&self) -> bool {
        true
    }
    fn supports_socks(&self) -> bool {
        false
    }

    fn enable(&self, settings: &Settings) -> Result<()> {
        let http = settings.http.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Windows system proxy requires an active HTTP inbound")
        })?;
        let address = endpoint_address(http);
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(INTERNET_SETTINGS, KEY_SET_VALUE)
            .context("open Windows internet settings")?;
        key.set_value("ProxyServer", &format!("http={address};https={address}"))
            .context("set Windows proxy server")?;
        key.set_value("ProxyOverride", &"<local>;localhost;127.*;[::1]")
            .context("set Windows proxy bypass")?;
        key.set_value("ProxyEnable", &1_u32)
            .context("enable Windows proxy")?;
        notify_changed()
    }

    fn disable(&self) -> Result<()> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(INTERNET_SETTINGS, KEY_SET_VALUE)
            .context("open Windows internet settings")?;
        key.set_value("ProxyEnable", &0_u32)
            .context("disable Windows proxy")?;
        key.set_value("ProxyServer", &"")
            .context("clear Windows proxy server")?;
        notify_changed()
    }

    fn matches(&self, settings: &Settings) -> Result<bool> {
        let Some(http) = &settings.http else {
            return Ok(false);
        };
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(INTERNET_SETTINGS, KEY_QUERY_VALUE)
            .context("open Windows internet settings")?;
        let enabled: u32 = match key.get_value("ProxyEnable") {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let server: String = match key.get_value("ProxyServer") {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let bypass: String = match key.get_value("ProxyOverride") {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let address = endpoint_address(http);
        Ok(enabled == 1
            && server == format!("http={address};https={address}")
            && bypass == "<local>;localhost;127.*;[::1]")
    }
}

fn endpoint_address(endpoint: &super::Endpoint) -> String {
    if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    }
}

fn notify_changed() -> Result<()> {
    unsafe {
        if InternetSetOptionW(
            null_mut(),
            INTERNET_OPTION_PROXY_SETTINGS_CHANGED,
            null_mut(),
            0,
        ) == 0
        {
            bail!(
                "notify Windows proxy settings change: {}",
                std::io::Error::last_os_error()
            );
        }
        if InternetSetOptionW(null_mut(), INTERNET_OPTION_REFRESH, null_mut(), 0) == 0 {
            bail!(
                "refresh Windows internet settings: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}
