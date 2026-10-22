//! Which proxy a request goes through: the environment's, one that is given, or the one
//! the operating system is set to use ([`ProxyMode::System`]).
//!
//! The system settings that are read:
//!
//! - **Windows:** the manual proxy of the Internet Options (`ProxyEnable`, `ProxyServer` and
//!   `ProxyOverride` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`),
//!   read with `reg query`.
//! - **GNOME:** the manual proxy of `org.gnome.system.proxy` (`gsettings`).
//! - **KDE:** the manual proxy of `kioslaverc`.
//!
//! One proxy serves every request: the `https` entry of a list that has one per scheme,
//! else its `http` entry. A bypass list is honoured (`*.corp.example`, `.corp.example`,
//! `<local>`, `10.0.0.0/8`, `192.168.*`...).
//!
//! What is not read is a proxy that a script decides: running a PAC file needs a JavaScript
//! engine, which this crate does not carry. A system that is set to use a script (`auto` in
//! GNOME, `Use setup script` in Windows, a script or automatic detection chosen in KDE) is not
//! ignored, because requests would then bypass a proxy the user needs: every request fails
//! and says why, until the application sets [`ProxyMode::Url`] or the environment sets
//! `HTTPS_PROXY`. The exception is the `Automatically detect settings` box of Windows, which
//! is on by default and usually finds nothing: it is not read, so that setting alone does not
//! stop requests, and a network that needs WPAD to reach the proxy needs one of those two.
//!
//! A bypass entry that is a plain name (`corp.example`) matches that host and the hosts under
//! it, as `NO_PROXY`, KDE and GNOME read it; the entries of Windows' list match exactly, as
//! Windows reads them (`*.corp.example` for the hosts under).

use std::net::IpAddr;

use super::ProxyMode;

/// What the settings of the operating system say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum System {
    /// Connect directly (or nothing could be read, which clients treat the same way).
    Direct,
    Manual {
        url: String,
        bypass: Vec<String>,
    },
    /// A script or automatic detection decides, which is not supported.
    Script(String),
}

/// What a [`ProxyMode`] comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The agent's own default: the environment's variables, `NO_PROXY` included.
    Environment,
    Direct,
    Proxy {
        url: String,
        bypass: Vec<String>,
    },
    /// Requests must fail, with this reason.
    Unusable(String),
}

/// `environment_set`: whether `HTTPS_PROXY` and its kind are in the environment. `system`
/// reads the operating system's settings, and is called only when it is needed.
pub(crate) fn plan(
    mode: &ProxyMode,
    no_proxy: &[String],
    environment_set: bool,
    system: impl FnOnce() -> System,
) -> Plan {
    match mode {
        ProxyMode::FromEnvironment => Plan::Environment,
        ProxyMode::Direct => Plan::Direct,
        ProxyMode::Url(url) => Plan::Proxy {
            url: url.clone(),
            bypass: no_proxy.to_vec(),
        },
        // What the user set for this process wins over what the desktop says.
        ProxyMode::System if environment_set => Plan::Environment,
        ProxyMode::System => match system() {
            System::Direct => Plan::Direct,
            System::Manual { url, mut bypass } => {
                bypass.extend(no_proxy.iter().cloned());
                Plan::Proxy { url, bypass }
            }
            System::Script(what) => Plan::Unusable(format!(
                "the system proxy is set by {what}, which is not evaluated; set HTTPS_PROXY or \
                 use ProxyMode::Url"
            )),
        },
    }
}

/// A list of hosts that do not go through the proxy.
#[derive(Debug, Default, Clone)]
pub(crate) struct Bypass {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
enum Rule {
    /// `*`: everything.
    All,
    /// `<local>`: a name with no dot.
    Local,
    /// A pattern with `*` in it, or (written `=name`) a name that must match exactly.
    Glob(String),
    /// A plain name: that host and the hosts under it.
    Name(String),
    /// `.example.com`: the domain and what is under it.
    Domain(String),
    Network(IpAddr, u8),
}

impl Bypass {
    /// Rules separated by commas, semicolons or spaces.
    pub fn parse(entries: &[String]) -> Self {
        let mut rules = Vec::new();
        for entry in entries {
            for token in entry.split([',', ';', ' ']) {
                let token = token.trim().to_ascii_lowercase();
                if token.is_empty() {
                    continue;
                }
                rules.push(if token == "*" {
                    Rule::All
                } else if token == "<local>" {
                    Rule::Local
                } else if let Some((address, bits)) = cidr(&token) {
                    Rule::Network(address, bits)
                } else if let Some(domain) = token.strip_prefix('.') {
                    Rule::Domain(domain.to_owned())
                } else if let Some(exact) = token.strip_prefix('=') {
                    Rule::Glob(exact.to_owned())
                } else if let Some((inside, after)) = token
                    .strip_prefix('[')
                    .and_then(|rest| rest.split_once(']'))
                {
                    // `[::1]`, `[::1]:8080` and `[2001:db8::]/32`.
                    match cidr(&format!("{inside}{after}")) {
                        Some((address, bits)) if after.starts_with('/') => {
                            Rule::Network(address, bits)
                        }
                        // A port after the address is not part of it.
                        _ => Rule::Glob(inside.to_owned()),
                    }
                } else {
                    // Any `:port` at the end is not part of the name.
                    let name = match token.rsplit_once(':') {
                        Some((name, port))
                            if !name.contains(':') && port.chars().all(|c| c.is_ascii_digit()) =>
                        {
                            name
                        }
                        _ => token.as_str(),
                    };
                    if name.contains('*') {
                        Rule::Glob(name.to_owned())
                    } else {
                        Rule::Name(name.to_owned())
                    }
                });
            }
        }
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether a request to `host` goes straight to it.
    pub fn matches(&self, host: &str) -> bool {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        let address = host.parse::<IpAddr>().ok();
        self.rules.iter().any(|rule| match rule {
            Rule::All => true,
            Rule::Local => !host.contains('.') && address.is_none(),
            Rule::Glob(pattern) => glob(pattern, &host),
            Rule::Name(name) => host == *name || host.ends_with(&format!(".{name}")),
            Rule::Domain(domain) => host == *domain || host.ends_with(&format!(".{domain}")),
            Rule::Network(network, bits) => {
                address.is_some_and(|address| in_network(address, *network, *bits))
            }
        })
    }
}

/// `10.0.0.0/8`, `::1/128`.
fn cidr(token: &str) -> Option<(IpAddr, u8)> {
    let (address, bits) = token.split_once('/')?;
    let address: IpAddr = address.parse().ok()?;
    let bits: u8 = bits.parse().ok()?;
    let most = if address.is_ipv4() { 32 } else { 128 };
    (bits <= most).then_some((address, bits))
}

fn in_network(address: IpAddr, network: IpAddr, bits: u8) -> bool {
    let (a, n) = match (address, network) {
        (IpAddr::V4(a), IpAddr::V4(n)) => (
            u128::from(u32::from(a)) << 96,
            u128::from(u32::from(n)) << 96,
        ),
        (IpAddr::V6(a), IpAddr::V6(n)) => (u128::from(a), u128::from(n)),
        _ => return false,
    };
    if bits == 0 {
        return true;
    }
    let mask = u128::MAX << (128 - u32::from(bits));
    a & mask == n & mask
}

/// `*` matches any run of characters, including none.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut at = first.len();
    let end = text.len() - last.len();
    for middle in &parts[1..parts.len() - 1] {
        match text[at..end].find(middle) {
            Some(found) => at += found + middle.len(),
            None => return false,
        }
    }
    true
}

/// A proxy address as the client takes it: a bare `host:port` is an HTTP proxy.
fn proxy_url(address: &str, default_scheme: &str) -> String {
    if address.contains("://") {
        address.to_owned()
    } else {
        format!("{default_scheme}://{address}")
    }
}

// --- Windows ---------------------------------------------------------------------------

/// What `reg query` prints for the Internet Settings key, as the settings it holds.
#[cfg(any(windows, test))]
pub(crate) fn parse_windows(output: &str) -> System {
    let value = |name: &str| -> Option<&str> {
        output.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == name).then(|| {
                // The type, then the data, which may hold spaces.
                let rest = line
                    .trim_start()
                    .strip_prefix(name)
                    .unwrap_or("")
                    .trim_start();
                rest.split_once(char::is_whitespace)
                    .map_or("", |(_, data)| data.trim())
            })
        })
    };
    let enabled = value("ProxyEnable").is_some_and(|data| {
        let data = data.trim_start_matches("0x");
        data.parse::<u32>().is_ok_and(|n| n != 0)
            || u32::from_str_radix(data, 16).is_ok_and(|n| n != 0)
    });
    if enabled && let Some(server) = value("ProxyServer").filter(|s| !s.is_empty()) {
        let Some(url) = pick_windows_server(server) else {
            return System::Direct;
        };
        let bypass = value("ProxyOverride")
            .map(|list| vec![windows_exact(list)])
            .unwrap_or_default();
        return System::Manual { url, bypass };
    }
    if value("AutoConfigURL").is_some_and(|url| !url.is_empty()) {
        return System::Script("a proxy auto-configuration script".to_owned());
    }
    System::Direct
}

/// The Windows list with its plain names marked as exact, which is how Windows matches them.
#[cfg(any(windows, test))]
fn windows_exact(list: &str) -> String {
    list.split([';', ',', ' '])
        .filter(|token| !token.is_empty())
        .map(|token| {
            let plain = !token.contains('*')
                && !token.starts_with('.')
                && !token.starts_with('<')
                && !token.starts_with('[')
                && cidr(token).is_none();
            if plain {
                format!("={token}")
            } else {
                token.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// `host:port` for every scheme, or `http=h:p;https=h:p;socks=h:p`.
#[cfg(any(windows, test))]
fn pick_windows_server(server: &str) -> Option<String> {
    if !server.contains('=') {
        return Some(proxy_url(server, "http"));
    }
    let entry = |scheme: &str| {
        server.split(';').find_map(|part| {
            let (name, address) = part.split_once('=')?;
            (name.trim().eq_ignore_ascii_case(scheme) && !address.trim().is_empty())
                .then(|| address.trim().to_owned())
        })
    };
    entry("https")
        .map(|a| proxy_url(&a, "http"))
        .or_else(|| entry("http").map(|a| proxy_url(&a, "http")))
        .or_else(|| entry("socks").map(|a| proxy_url(&a, "socks5")))
}

#[cfg(windows)]
pub(crate) fn detect() -> System {
    let output = run(
        "reg",
        &[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
        ],
    );
    output.map_or(System::Direct, |output| parse_windows(&output))
}

// --- Linux -----------------------------------------------------------------------------

/// What `gsettings list-recursively org.gnome.system.proxy` prints.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_gnome(output: &str) -> Option<System> {
    let get = |schema: &str, key: &str| -> Option<&str> {
        output.lines().find_map(|line| {
            let rest = line.strip_prefix(schema)?.strip_prefix(' ')?;
            let rest = rest.strip_prefix(key)?.strip_prefix(' ')?;
            Some(rest.trim())
        })
    };
    let text = |value: &str| value.trim_matches('\'').to_owned();
    match text(get("org.gnome.system.proxy", "mode")?).as_str() {
        "manual" => {}
        "auto" => {
            return Some(System::Script(
                "an automatic configuration (PAC) setting".to_owned(),
            ));
        }
        _ => return Some(System::Direct),
    }
    let server = |scheme: &str| {
        let schema = format!("org.gnome.system.proxy.{scheme}");
        let host = text(get(&schema, "host")?);
        let port: u16 = get(&schema, "port")?
            .parse()
            .ok()
            .filter(|port| *port != 0)?;
        (!host.is_empty()).then(|| format!("{host}:{port}"))
    };
    let url = server("https")
        .map(|a| proxy_url(&a, "http"))
        .or_else(|| server("http").map(|a| proxy_url(&a, "http")))
        .or_else(|| server("socks").map(|a| proxy_url(&a, "socks5")));
    let Some(url) = url else {
        return Some(System::Direct);
    };
    // `['localhost', '127.0.0.0/8', '::1']`
    let bypass = get("org.gnome.system.proxy", "ignore-hosts")
        .map(|list| {
            // An empty list is printed with its type: `@as []`.
            list.trim_start_matches("@as")
                .trim_start()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|host| text(host.trim()))
                .filter(|host| !host.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(System::Manual { url, bypass })
}

/// The `[Proxy Settings]` group of `kioslaverc`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_kde(contents: &str) -> System {
    let mut in_group = false;
    let mut settings = std::collections::HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_group = group == "Proxy Settings";
        } else if in_group && let Some((key, value)) = line.split_once('=') {
            settings.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    match settings.get("ProxyType").map(String::as_str) {
        Some("1") => {}
        Some("2") => return System::Script("an automatic configuration (PAC) setting".to_owned()),
        Some("3") => return System::Script("automatic proxy detection (WPAD)".to_owned()),
        // None, or "use the environment", which was already looked at.
        _ => return System::Direct,
    }
    // KDE writes `http://host:port`, and older versions `host port`.
    let server = |key: &str| {
        let value = settings.get(key)?.replace(' ', ":");
        (!value.is_empty()).then(|| proxy_url(&value, "http"))
    };
    let Some(url) = server("httpsProxy").or_else(|| server("httpProxy")) else {
        return System::Direct;
    };
    // `ReversedException=true` turns the list around: the proxy is for those hosts only.
    if settings.get("ReversedException").map(String::as_str) == Some("true") {
        return System::Script("a list of the only hosts that use the proxy".to_owned());
    }
    let bypass = settings
        .get("NoProxyFor")
        .map(|list| vec![list.clone()])
        .unwrap_or_default();
    System::Manual { url, bypass }
}

#[cfg(target_os = "linux")]
pub(crate) fn detect() -> System {
    if let Some(output) = run("gsettings", &["list-recursively", "org.gnome.system.proxy"])
        && let Some(system) = parse_gnome(&output)
        && system != System::Direct
    {
        return system;
    }
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        });
    config_home
        .and_then(|home| std::fs::read_to_string(home.join("kioslaverc")).ok())
        .map_or(System::Direct, |contents| parse_kde(&contents))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn detect() -> System {
    System::Direct
}

/// Runs a program that reads settings, giving up after a couple of seconds so that a
/// session with no settings daemon does not hold the first request back for long. `None`
/// if it is missing, fails or is slow.
#[cfg(any(windows, target_os = "linux"))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    // Read while it runs: a program that prints more than a pipe holds waits for a reader
    // before it can exit.
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let read = std::io::Read::read_to_string(&mut stdout, &mut output);
        let _ = sender.send(read.ok().map(|_| output));
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            // Too slow, or not to be asked: either way it must not outlive this.
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !succeeded {
        // The reader ends when the pipe closes; a child of the program that was killed can
        // hold it open, and waiting for that would be waiting for it.
        return None;
    }
    // The program is done, but something it started can still hold the pipe open and keep
    // the reader from seeing the end, so the wait for the output has a limit too.
    receiver
        .recv_timeout(Duration::from_millis(500))
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bypass(entries: &[&str]) -> Bypass {
        Bypass::parse(&entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn a_bypass_list_matches_names_domains_patterns_and_networks() {
        let list = bypass(&[
            "localhost;*.corp.example;.intra.example;<local>",
            "10.0.0.0/8, 192.168.*, 2001:db8::/32 printer.example:8080 [::1]:8080",
            "[fd00::]/8",
        ]);
        for host in [
            "localhost",
            "LOCALHOST",
            "build.corp.example",
            "intra.example",
            "a.b.intra.example",
            "nas",
            "10.20.30.40",
            "192.168.1.9",
            "2001:db8::7",
            "[2001:db8::7]",
            "printer.example",
            "::1",
            "fd12:3456::1",
        ] {
            assert!(list.matches(host), "{host} goes direct");
        }
        for host in [
            "example.com",
            "corp.example",
            "notintra.example",
            "11.0.0.1",
            "193.168.1.1",
            "2001:db9::1",
            "cdn.example.com",
        ] {
            assert!(!list.matches(host), "{host} uses the proxy");
        }
    }

    #[test]
    fn local_means_a_name_without_a_dot_and_not_an_address() {
        let list = bypass(&["<local>"]);
        assert!(list.matches("intranet"));
        assert!(!list.matches("intranet.example"));
        assert!(!list.matches("127.0.0.1"));
    }

    #[test]
    fn a_star_bypasses_everything_and_an_empty_list_nothing() {
        assert!(bypass(&["*"]).matches("example.com"));
        let none = bypass(&[]);
        assert!(none.is_empty() && !none.matches("example.com"));
    }

    #[test]
    fn a_pattern_with_stars_in_the_middle_matches_across_it() {
        let list = bypass(&["a*b*c"]);
        assert!(list.matches("abc") && list.matches("axxbyyc") && list.matches("abbc"));
        assert!(!list.matches("ab") && !list.matches("acb") && !list.matches("xabc"));
    }

    #[test]
    fn a_network_of_zero_bits_is_everything_of_its_family() {
        let list = bypass(&["0.0.0.0/0"]);
        assert!(list.matches("8.8.8.8"));
        assert!(!list.matches("2001:db8::1"));
    }

    #[test]
    fn the_environment_wins_over_the_desktop_and_nothing_reads_the_desktop_for_other_modes() {
        let never = || -> System { unreachable!("the system settings were read") };
        assert_eq!(
            plan(&ProxyMode::System, &[], true, never),
            Plan::Environment
        );
        assert_eq!(
            plan(&ProxyMode::FromEnvironment, &[], false, never),
            Plan::Environment
        );
        assert_eq!(plan(&ProxyMode::Direct, &[], true, never), Plan::Direct);
        assert_eq!(
            plan(
                &ProxyMode::Url("http://p:1".to_owned()),
                &["a.example".to_owned()],
                true,
                never
            ),
            Plan::Proxy {
                url: "http://p:1".to_owned(),
                bypass: vec!["a.example".to_owned()]
            }
        );
    }

    #[test]
    fn the_desktops_proxy_is_used_with_its_bypass_list_and_the_configured_one() {
        let system = || System::Manual {
            url: "http://proxy.corp:3128".to_owned(),
            bypass: vec!["*.corp".to_owned()],
        };
        assert_eq!(
            plan(
                &ProxyMode::System,
                &["extra.example".to_owned()],
                false,
                system
            ),
            Plan::Proxy {
                url: "http://proxy.corp:3128".to_owned(),
                bypass: vec!["*.corp".to_owned(), "extra.example".to_owned()]
            }
        );
        assert_eq!(
            plan(&ProxyMode::System, &[], false, || System::Direct),
            Plan::Direct
        );
    }

    #[test]
    fn a_proxy_chosen_by_a_script_is_refused_not_bypassed() {
        let plan = plan(&ProxyMode::System, &[], false, || {
            System::Script("a proxy auto-configuration script".to_owned())
        });
        assert!(
            matches!(&plan, Plan::Unusable(why) if why.contains("PAC") || why.contains("script"))
        );
    }

    #[test]
    fn windows_settings_with_one_proxy_for_every_scheme() {
        let output = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    proxy.corp:8080\r\n    ProxyOverride    REG_SZ    *.corp;192.168.*;<local>\r\n";
        assert_eq!(
            parse_windows(output),
            System::Manual {
                url: "http://proxy.corp:8080".to_owned(),
                bypass: vec!["*.corp;192.168.*;<local>".to_owned()],
            }
        );
    }

    #[test]
    fn windows_settings_with_a_proxy_for_each_scheme_use_the_https_one() {
        let output = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    http=a:1;https=b:2;socks=c:3\n";
        assert_eq!(
            parse_windows(output),
            System::Manual {
                url: "http://b:2".to_owned(),
                bypass: vec![]
            }
        );
        let only_socks =
            "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    socks=c:3\n";
        assert!(
            matches!(parse_windows(only_socks), System::Manual { url, .. } if url == "socks5://c:3")
        );
    }

    #[test]
    fn windows_settings_with_the_proxy_off_or_a_script() {
        let off = "    ProxyEnable    REG_DWORD    0x0\n    ProxyServer    REG_SZ    proxy:1\n";
        assert_eq!(parse_windows(off), System::Direct);
        let script = "    ProxyEnable    REG_DWORD    0x0\n    AutoConfigURL    REG_SZ    http://wpad/proxy.pac\n";
        assert!(matches!(parse_windows(script), System::Script(_)));
        assert_eq!(parse_windows(""), System::Direct);
    }

    const GNOME_MANUAL: &str = "org.gnome.system.proxy autoconfig-url ''\norg.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.0/8', '::1', '*.corp']\norg.gnome.system.proxy mode 'manual'\norg.gnome.system.proxy use-same-proxy true\norg.gnome.system.proxy.ftp host ''\norg.gnome.system.proxy.ftp port 0\norg.gnome.system.proxy.http authentication-password ''\norg.gnome.system.proxy.http host 'plain.corp'\norg.gnome.system.proxy.http port 3128\norg.gnome.system.proxy.https host 'secure.corp'\norg.gnome.system.proxy.https port 3129\norg.gnome.system.proxy.socks host ''\norg.gnome.system.proxy.socks port 0\n";

    #[test]
    fn gnome_manual_settings() {
        assert_eq!(
            parse_gnome(GNOME_MANUAL),
            Some(System::Manual {
                url: "http://secure.corp:3129".to_owned(),
                bypass: ["localhost", "127.0.0.0/8", "::1", "*.corp"]
                    .map(str::to_owned)
                    .to_vec(),
            })
        );
    }

    #[test]
    fn gnome_with_no_proxy_a_script_or_a_manual_mode_with_no_server() {
        let none = GNOME_MANUAL.replace("mode 'manual'", "mode 'none'");
        assert_eq!(parse_gnome(&none), Some(System::Direct));
        let auto = GNOME_MANUAL.replace("mode 'manual'", "mode 'auto'");
        assert!(matches!(parse_gnome(&auto), Some(System::Script(_))));
        let empty = "org.gnome.system.proxy mode 'manual'\norg.gnome.system.proxy.http host ''\norg.gnome.system.proxy.http port 0\n";
        assert_eq!(parse_gnome(empty), Some(System::Direct));
        // An empty list of hosts is printed with its type.
        let none = GNOME_MANUAL.replace("['localhost', '127.0.0.0/8', '::1', '*.corp']", "@as []");
        assert!(
            matches!(parse_gnome(&none), Some(System::Manual { bypass, .. }) if bypass.is_empty())
        );
        // Not GNOME's output at all.
        assert_eq!(parse_gnome("nothing here"), None);
    }

    #[test]
    fn kde_manual_settings_and_the_ones_that_are_not_supported() {
        let manual = "[General]\nfoo=bar\n\n[Proxy Settings]\nProxyType=1\nhttpProxy=http://plain.corp:3128\nhttpsProxy=http://secure.corp:3129\nNoProxyFor=localhost,.corp\n";
        assert_eq!(
            parse_kde(manual),
            System::Manual {
                url: "http://secure.corp:3129".to_owned(),
                bypass: vec!["localhost,.corp".to_owned()],
            }
        );
        let older = "[Proxy Settings]\nProxyType=1\nhttpProxy=plain.corp 3128\n";
        assert!(
            matches!(parse_kde(older), System::Manual { url, .. } if url == "http://plain.corp:3128")
        );
        assert!(matches!(
            parse_kde("[Proxy Settings]\nProxyType=2\n"),
            System::Script(_)
        ));
        assert!(matches!(
            parse_kde("[Proxy Settings]\nProxyType=3\n"),
            System::Script(_)
        ));
        assert_eq!(parse_kde("[Proxy Settings]\nProxyType=0\n"), System::Direct);
        let reversed = "[Proxy Settings]\nProxyType=1\nhttpProxy=http://p:1\nReversedException=true\nNoProxyFor=only.corp\n";
        assert!(matches!(parse_kde(reversed), System::Script(_)));
        assert_eq!(parse_kde("[Other]\nProxyType=1\n"), System::Direct);
        assert_eq!(parse_kde(""), System::Direct);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn output_larger_than_a_pipe_holds_is_read_in_full() {
        // 300 000 bytes, well over the 64 KiB a pipe takes before the writer has to wait.
        let output = run("sh", &["-c", "yes | head -c 300000"]).expect("the output");
        assert_eq!(output.len(), 300_000);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_program_that_is_missing_slow_or_failing_gives_nothing() {
        assert_eq!(run("florui-no-such-program", &[]), None);
        assert_eq!(run("sh", &["-c", "exit 3"]), None);
        let started = std::time::Instant::now();
        assert_eq!(run("sh", &["-c", "sleep 30"]), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_plain_name_matches_the_host_and_the_hosts_under_it_unless_it_is_marked_exact() {
        let list = bypass(&["corp.example"]);
        assert!(list.matches("corp.example") && list.matches("img.corp.example"));
        assert!(!list.matches("notcorp.example") && !list.matches("example"));
        let exact = bypass(&["=corp.example"]);
        assert!(exact.matches("corp.example") && !exact.matches("img.corp.example"));
    }

    #[test]
    fn the_windows_list_matches_plain_names_exactly() {
        let output = "    ProxyEnable    REG_DWORD    0x1\n    ProxyServer    REG_SZ    p:1\n    ProxyOverride    REG_SZ    intranet.corp;*.corp;10.0.0.0/8;<local>\n";
        let System::Manual {
            bypass: entries, ..
        } = parse_windows(output)
        else {
            panic!("a manual proxy")
        };
        let list = Bypass::parse(&entries);
        assert!(list.matches("intranet.corp") && list.matches("x.corp"));
        assert!(!list.matches("a.intranet.corp.example"));
        assert!(list.matches("10.1.1.1") && list.matches("nas"));
        // Marked exact: the host itself, not what is under it.
        let plain = Bypass::parse(&[windows_exact("host.example")]);
        assert!(plain.matches("host.example") && !plain.matches("a.host.example"));
    }

    #[test]
    fn a_bracketed_ipv6_network_with_a_prefix_length_is_a_network() {
        let list = bypass(&["[2001:db8::]/32"]);
        assert!(list.matches("2001:db8:1::5") && !list.matches("2001:db9::1"));
    }
}
