use std::{ffi::OsString, net::IpAddr};

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub listen: String,
    pub core_address: String,
    pub core_binary: String,
    pub external_core: bool,
    pub data_directory: String,
    pub config_file: String,
    pub resume_file: String,
    pub core_listen: String,
    pub core_log_console: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:9080".into(),
            core_address: "http://127.0.0.1:9090".into(),
            core_binary: String::new(),
            external_core: false,
            data_directory: String::new(),
            config_file: String::new(),
            resume_file: String::new(),
            core_listen: "127.0.0.1:9090".into(),
            core_log_console: false,
        }
    }
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut args = args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .peekable();
        let mut options = Self::default();
        if args.peek().is_some_and(|value| value == "external") {
            options.external_core = true;
            args.next();
        }

        let mut positional = Vec::new();
        while let Some(argument) = args.next() {
            if argument == "-h" || argument == "--help" {
                print_usage();
                std::process::exit(0);
            }
            if !argument.starts_with('-') {
                positional.push(argument);
                positional.extend(args);
                break;
            }
            let (name, inline_value) = argument.split_once('=').unwrap_or((&argument, ""));
            match name {
                "-listen" | "--listen" => options.listen = value(inline_value, &mut args, name)?,
                "-core-binary" | "--core-binary" => {
                    options.core_binary = value(inline_value, &mut args, name)?
                }
                "-data-dir" | "--data-dir" => {
                    options.data_directory = value(inline_value, &mut args, name)?
                }
                "-config" | "--config" => {
                    options.config_file = value(inline_value, &mut args, name)?
                }
                "-resume-state" | "--resume-state" => {
                    options.resume_file = value(inline_value, &mut args, name)?
                }
                "-core-listen" | "--core-listen" => {
                    options.core_listen = value(inline_value, &mut args, name)?
                }
                "-core-log-console" | "--core-log-console" if inline_value.is_empty() => {
                    options.core_log_console = true
                }
                "-core-log-console" | "--core-log-console" => {
                    options.core_log_console = inline_value.parse()?
                }
                _ => bail!("unknown option {argument:?}"),
            }
        }

        match positional.as_slice() {
            [] => {}
            [address] if options.external_core => {
                options.core_address = normalize_core_address(address)
            }
            [_] if !options.external_core => {
                bail!("unexpected argument; use 'sumire external [core-address]'")
            }
            _ => bail!("too many arguments for external mode"),
        }
        options.core_address = normalize_core_address(&options.core_address);
        Ok(options)
    }
}

fn value(inline: &str, args: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    if !inline.is_empty() {
        return Ok(inline.to_owned());
    }
    args.next()
        .ok_or_else(|| anyhow::anyhow!("missing value for {option}"))
}

pub fn normalize_core_address(address: &str) -> String {
    let address = address.trim();
    if !address.is_empty() && !address.contains("://") {
        format!("http://{address}")
    } else {
        address.to_owned()
    }
}

pub fn managed_listen_warning(address: &str) -> Option<String> {
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    let host = host.trim_matches(['[', ']']);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if loopback {
        None
    } else {
        Some(format!(
            "WebUI is listening on non-loopback address {address}; anyone who can reach it may obtain the managed Core token"
        ))
    }
}

fn print_usage() {
    eprintln!("Usage:\n  sumire [options]\n  sumire external [options] [core-address]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options> {
        Options::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_default_and_external_modes() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        let options = parse(&["external", "192.0.2.1:9090"]).unwrap();
        assert!(options.external_core);
        assert_eq!(options.core_address, "http://192.0.2.1:9090");
    }

    #[test]
    fn rejects_removed_and_extra_arguments() {
        assert!(parse(&["-external-core"]).is_err());
        assert!(parse(&["-core", "127.0.0.1:9090"]).is_err());
        assert!(parse(&["external", "one", "two"]).is_err());
    }

    #[test]
    fn warns_for_non_loopback_managed_listen() {
        assert!(managed_listen_warning("127.0.0.1:9080").is_none());
        assert!(
            managed_listen_warning(":9080")
                .unwrap()
                .contains("managed Core token")
        );
    }
}
