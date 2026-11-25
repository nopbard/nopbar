//! Windows proxy settings are read only while constructing a worker HTTP client.
#[derive(Clone, Default, Debug, PartialEq)]
struct Rules {
    http: Option<String>,
    https: Option<String>,
    bypass: Vec<String>,
}
impl Rules {
    fn parse(proxy: &str, bypass: &str) -> Option<Self> {
        let mut rules = Self {
            bypass: bypass
                .split([';', ','])
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
            ..Default::default()
        };
        for entry in proxy.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (scheme, value) = entry.split_once('=').unwrap_or(("all", entry));
            let value = if value.contains("://") {
                value.to_string()
            } else {
                format!("http://{value}")
            };
            let url = reqwest::Url::parse(&value).ok()?;
            if url.host_str().is_none() || !["http", "https"].contains(&url.scheme()) {
                return None;
            }
            match scheme.trim().to_ascii_lowercase().as_str() {
                "http" => rules.http = Some(value),
                "https" => rules.https = Some(value),
                "all" => {
                    rules.http = Some(value.clone());
                    rules.https = Some(value);
                }
                _ => {}
            }
        }
        Some(rules)
    }
    fn route(&self, url: &reqwest::Url) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        if self.bypass.iter().any(|b| {
            b == "*"
                || b == &host
                || b == "<local>" && !host.contains('.')
                || b.strip_prefix("*.")
                    .is_some_and(|suffix| host == suffix || host.ends_with(&format!(".{suffix}")))
        }) {
            return None;
        }
        if url.scheme() == "https" {
            self.https.clone()
        } else {
            self.http.clone()
        }
    }
}
pub fn configure(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match system() {
        Some(routes) => builder.no_proxy().proxy(reqwest::Proxy::custom(move |url| {
            routes
                .iter()
                .find(|(host, _)| host.is_empty() || url.host_str() == Some(host.as_str()))
                .and_then(|(_, rules)| rules.route(url))
        })),
        None => builder, // reqwest's normal HTTP(S)_PROXY / NO_PROXY fallback.
    }
}
fn system() -> Option<Vec<(String, Rules)>> {
    use windows::{
        core::{w, HSTRING, PCWSTR, PWSTR},
        Win32::{
            Foundation::{GlobalFree, HGLOBAL},
            Networking::WinHttp::*,
        },
    };
    unsafe fn take(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let value = p.to_string().unwrap_or_default();
        let _ = GlobalFree(Some(HGLOBAL(p.0.cast())));
        value
    }
    unsafe {
        let mut ie = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        if WinHttpGetIEProxyConfigForCurrentUser(&mut ie).is_ok() {
            let proxy = take(ie.lpszProxy);
            let bypass = take(ie.lpszProxyBypass);
            let pac = take(ie.lpszAutoConfigUrl);
            if !pac.is_empty() || ie.fAutoDetect.as_bool() {
                let session = WinHttpOpen(
                    w!("NopBar"),
                    WINHTTP_ACCESS_TYPE_NO_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    0,
                );
                if !session.is_null() {
                    let _ = WinHttpSetTimeouts(session, 15000, 15000, 15000, 15000);
                    let pac = HSTRING::from(pac);
                    let mut routes = Vec::new();
                    for host in ["auth.openai.com", "api.openai.com"] {
                        let mut options = WINHTTP_AUTOPROXY_OPTIONS {
                            dwFlags: if pac.is_empty() {
                                WINHTTP_AUTOPROXY_AUTO_DETECT
                            } else {
                                WINHTTP_AUTOPROXY_CONFIG_URL
                            },
                            dwAutoDetectFlags: WINHTTP_AUTO_DETECT_TYPE_DHCP
                                | WINHTTP_AUTO_DETECT_TYPE_DNS_A,
                            lpszAutoConfigUrl: PCWSTR(pac.as_ptr()),
                            ..Default::default()
                        };
                        let mut info = WINHTTP_PROXY_INFO::default();
                        let ok = WinHttpGetProxyForUrl(
                            session,
                            &HSTRING::from(format!("https://{host}/")),
                            &mut options,
                            &mut info,
                        )
                        .is_ok();
                        let p = take(info.lpszProxy);
                        let b = take(info.lpszProxyBypass);
                        if ok {
                            if let Some(r) = Rules::parse(&p, &b) {
                                routes.push((host.into(), r));
                            }
                        }
                    }
                    let _ = WinHttpCloseHandle(session);
                    if routes.len() == 2 {
                        return Some(routes);
                    }
                }
                if proxy.is_empty() {
                    return None;
                }
            }
            if !proxy.is_empty() {
                return Rules::parse(&proxy, &bypass).map(|r| vec![(String::new(), r)]);
            }
        }
        let mut info = WINHTTP_PROXY_INFO::default();
        let ok = WinHttpGetDefaultProxyConfiguration(&mut info).is_ok();
        let proxy = take(info.lpszProxy);
        let bypass = take(info.lpszProxyBypass);
        if ok {
            Rules::parse(&proxy, &bypass).map(|r| vec![(String::new(), r)])
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_scheme_specific_proxies_and_windows_bypass_without_os_calls() {
        let rules = Rules::parse(
            "http=plain:80;https=secure:8080",
            "<local>;*.internal;127.0.0.1",
        )
        .unwrap();
        assert_eq!(
            rules
                .route(&reqwest::Url::parse("https://api.openai.com/v1").unwrap())
                .as_deref(),
            Some("http://secure:8080")
        );
        for url in ["http://printer", "https://a.internal", "http://127.0.0.1"] {
            assert!(rules.route(&reqwest::Url::parse(url).unwrap()).is_none());
        }
        assert_eq!(
            Rules::parse("proxy:3128", "").unwrap().https.as_deref(),
            Some("http://proxy:3128")
        );
        assert!(Rules::parse("https=bad proxy", "").is_none());
    }
}
