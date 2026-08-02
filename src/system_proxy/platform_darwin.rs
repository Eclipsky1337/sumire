use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{Endpoint, Settings, SystemProxyPlatform};

#[derive(Default)]
pub struct Platform;

impl SystemProxyPlatform for Platform {
    fn supported(&self) -> bool {
        true
    }
    fn supports_socks(&self) -> bool {
        true
    }

    fn enable(&self, settings: &Settings) -> Result<()> {
        for service in services()? {
            if let Some(http) = &settings.http {
                run(&[
                    "-setwebproxy",
                    &service,
                    &http.host,
                    &http.port.to_string(),
                    "off",
                ])?;
                run(&["-setwebproxystate", &service, "on"])?;
                run(&[
                    "-setsecurewebproxy",
                    &service,
                    &http.host,
                    &http.port.to_string(),
                    "off",
                ])?;
                run(&["-setsecurewebproxystate", &service, "on"])?;
            } else {
                run(&["-setwebproxystate", &service, "off"])?;
                run(&["-setsecurewebproxystate", &service, "off"])?;
            }
            if let Some(socks) = &settings.socks {
                run(&[
                    "-setsocksfirewallproxy",
                    &service,
                    &socks.host,
                    &socks.port.to_string(),
                    "off",
                ])?;
                run(&["-setsocksfirewallproxystate", &service, "on"])?;
            } else {
                run(&["-setsocksfirewallproxystate", &service, "off"])?;
            }
            run(&[
                "-setproxybypassdomains",
                &service,
                "localhost",
                "127.0.0.1",
                "::1",
            ])?;
        }
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        for service in services()? {
            run(&["-setwebproxystate", &service, "off"])?;
            run(&["-setsecurewebproxystate", &service, "off"])?;
            run(&["-setsocksfirewallproxystate", &service, "off"])?;
        }
        Ok(())
    }

    fn matches(&self, settings: &Settings) -> Result<bool> {
        for service in services()? {
            if !proxy_matches(&service, "-getwebproxy", settings.http.as_ref())?
                || !proxy_matches(&service, "-getsecurewebproxy", settings.http.as_ref())?
                || !proxy_matches(&service, "-getsocksfirewallproxy", settings.socks.as_ref())?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn services() -> Result<Vec<String>> {
    let output = output(&["-listallnetworkservices"])?;
    let services: Vec<_> = output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("An asterisk") && !line.starts_with('*')
        })
        .map(str::to_owned)
        .collect();
    if services.is_empty() {
        bail!("no enabled macOS network services found");
    }
    Ok(services)
}

fn proxy_matches(service: &str, command: &str, expected: Option<&Endpoint>) -> Result<bool> {
    let output = output(&[command, service])?;
    let value = |key: &str| proxy_value(&output, key);
    let enabled = value("Enabled").is_some_and(|value| value.eq_ignore_ascii_case("yes"));
    let Some(expected) = expected else {
        return Ok(!enabled);
    };
    Ok(enabled
        && value("Server").is_some_and(|host| host.eq_ignore_ascii_case(&expected.host))
        && value("Port").and_then(|port| port.parse::<u16>().ok()) == Some(expected.port))
}

fn proxy_value<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim() == key).then(|| value.trim())
    })
}

fn run(arguments: &[&str]) -> Result<()> {
    output(arguments).map(|_| ())
}

fn output(arguments: &[&str]) -> Result<String> {
    let result = Command::new("/usr/sbin/networksetup")
        .args(arguments)
        .output()
        .context("run networksetup")?;
    if !result.status.success() {
        bail!(
            "networksetup {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_networksetup_field() {
        let output =
            "Enabled: Yes\nServer: 127.0.0.1\nPort: 1081\nAuthenticated Proxy Enabled: 0\n";
        assert_eq!(proxy_value(output, "Enabled"), Some("Yes"));
        assert_eq!(proxy_value(output, "Server"), Some("127.0.0.1"));
        assert_eq!(proxy_value(output, "Port"), Some("1081"));
    }
}
