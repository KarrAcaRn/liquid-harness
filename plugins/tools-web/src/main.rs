use lh_plugin_sdk::mcp::{ToolServer, serve, tool};
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;
use url::Url;

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    provider: String,
    base_url: Option<String>,
    api_key_env: String,
    timeout_secs: u64,
    max_bytes: usize,
    allow_private_network: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "duckduckgo".into(),
            base_url: None,
            api_key_env: "BRAVE_SEARCH_API_KEY".into(),
            timeout_secs: 15,
            max_bytes: 1_048_576,
            allow_private_network: false,
        }
    }
}

#[derive(Default)]
struct Server {
    config: Config,
}

// Conservative global-unicast allowlist: some globally reachable special-purpose
// addresses are intentionally excluded. IPv4-mapped IPv6 is never allowed.
fn global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

fn checked_addresses(addresses: Vec<SocketAddr>, private: bool) -> io::Result<Vec<SocketAddr>> {
    if addresses.is_empty() || (!private && addresses.iter().any(|a| !global(a.ip()))) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "non-global destination denied",
        ));
    }
    Ok(addresses)
}

fn parse_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "invalid URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("only HTTP(S) URLs without credentials are supported".into());
    }
    Ok(url)
}

impl Server {
    fn get(&self, url: &Url, key: Option<&str>) -> Result<(String, String), String> {
        let private = self.config.allow_private_network;
        let timeout = Duration::from_secs(self.config.timeout_secs);
        // Validate the *actual* resolution used by the connector, not a separate
        // preflight lookup. ureq connects only to these validated SocketAddrs.
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout(timeout)
            .resolver(move |netloc: &str| {
                // System DNS is blocking and has no cancellation API. Bound the
                // caller's wait; a timed-out lookup may finish on its worker.
                let netloc = netloc.to_owned();
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let result = netloc
                        .to_socket_addrs()
                        .and_then(|iter| checked_addresses(iter.collect(), private));
                    let _ = send.send(result);
                });
                receive.recv_timeout(timeout).map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "DNS resolution timed out")
                })?
            })
            .build();
        let mut request = agent
            .get(url.as_str())
            .set("User-Agent", "liquid-harness-tools-web/0.1");
        if let Some(key) = key {
            request = request.set("X-Subscription-Token", key);
        }
        let response = request.call().map_err(|err| match err {
            ureq::Error::Status(code, _) => format!("HTTP status {code}"),
            // Never leak API keys or full URLs in diagnostics.
            ureq::Error::Transport(_) => {
                "HTTP transport failed (destination denied, DNS, TLS or timeout)".into()
            }
        })?;
        if !(200..300).contains(&response.status()) {
            return Err("redirects are disabled".into());
        }
        let content_type = response
            .header("Content-Type")
            .unwrap_or("text/plain")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if content_type != "text/html"
            && content_type != "application/xhtml+xml"
            && !content_type.starts_with("text/")
            && content_type != "application/json"
        {
            return Err("unsupported response content type".into());
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(self.config.max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "response read failed or timed out")?;
        if bytes.len() > self.config.max_bytes {
            return Err("response exceeds max_bytes".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "response is not UTF-8")?;
        Ok((text, content_type))
    }

    fn search(&self, query: &str, limit: usize) -> Result<String, String> {
        let (mut url, key) = match self.config.provider.as_str() {
            "duckduckgo" => (parse_url("https://html.duckduckgo.com/html/")?, None),
            "searxng" => {
                let mut base = parse_url(
                    self.config
                        .base_url
                        .as_deref()
                        .ok_or("searxng requires base_url")?,
                )?;
                let path = format!("{}/search", base.path().trim_end_matches('/'));
                base.set_path(&path);
                base.set_query(None);
                base.set_fragment(None);
                (base, None)
            }
            "brave" => {
                let key = std::env::var(&self.config.api_key_env)
                    .map_err(|_| "Brave API key environment variable is missing")?;
                if key.trim().is_empty() || key.chars().any(char::is_control) {
                    return Err("Brave API key is empty or invalid".into());
                }
                (
                    parse_url("https://api.search.brave.com/res/v1/web/search")?,
                    Some(key),
                )
            }
            _ => return Err("unsupported search provider".into()),
        };
        url.query_pairs_mut().append_pair("q", query);
        if self.config.provider == "searxng" {
            url.query_pairs_mut().append_pair("format", "json");
        } else if self.config.provider == "brave" {
            url.query_pairs_mut()
                .append_pair("count", &limit.to_string());
        }
        let (body, _) = self.get(&url, key.as_deref())?;
        let results = if self.config.provider == "duckduckgo" {
            duckduckgo_results(&body, limit)
        } else {
            json_results(&body, &self.config.provider, limit)?
        };
        Ok(
            json!({"query": query, "provider": self.config.provider, "results": results})
                .to_string(),
        )
    }
}

fn readable(html: &str) -> String {
    let document = Html::parse_document(html);
    let mut text = String::new();
    for node in document.tree.nodes() {
        if let Some(value) = node.value().as_text() {
            let hidden = node.ancestors().filter_map(ElementRef::wrap).any(|e| {
                matches!(
                    e.value().name(),
                    "script" | "style" | "head" | "noscript" | "template" | "svg"
                ) || e.value().attr("hidden").is_some()
                    || e.value().attr("aria-hidden") == Some("true")
            });
            if !hidden {
                for word in value.split_whitespace() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(word);
                }
            }
        }
    }
    text
}

fn duckduckgo_results(body: &str, limit: usize) -> Vec<Value> {
    let document = Html::parse_document(body);
    let results = Selector::parse(".result").unwrap();
    let links = Selector::parse(".result__a").unwrap();
    let snippets = Selector::parse(".result__snippet").unwrap();
    document
        .select(&results)
        .filter_map(|result| {
            let link = result.select(&links).next()?;
            let href = link.value().attr("href")?;
            let base = Url::parse("https://html.duckduckgo.com/").ok()?;
            let mut url = base.join(href).ok()?;
            if url
                .host_str()
                .is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com"))
                && let Some(target) = url
                    .query_pairs()
                    .find(|(k, _)| k == "uddg")
                    .map(|(_, v)| v.into_owned())
            {
                url = parse_url(&target).ok()?;
            }
            parse_url(url.as_str()).ok()?;
            let snippet = result
                .select(&snippets)
                .next()
                .map(|s| readable(&s.html()))
                .unwrap_or_default();
            Some(json!({"title": readable(&link.html()), "url": url.as_str(), "snippet": snippet}))
        })
        .take(limit)
        .collect()
}

fn json_results(body: &str, provider: &str, limit: usize) -> Result<Vec<Value>, String> {
    let data: Value = serde_json::from_str(body).map_err(|_| "invalid search JSON")?;
    let results = if provider == "brave" {
        &data["web"]["results"]
    } else {
        &data["results"]
    };
    let entries = results
        .as_array()
        .ok_or("search response lacks results array")?;
    Ok(entries.iter().filter_map(|item| {
        let url = parse_url(item["url"].as_str()?).ok()?;
        Some(json!({"title": item["title"].as_str().unwrap_or(""), "url": url.as_str(),
            "snippet": readable(item[if provider == "brave" { "description" } else { "content" }].as_str().unwrap_or(""))}))
    }).take(limit).collect())
}

impl ToolServer for Server {
    fn name(&self) -> &str {
        "tools-web"
    }

    fn configure(&mut self, config: Value) -> Result<(), String> {
        let config: Config =
            serde_json::from_value(config).map_err(|e| format!("invalid web config: {e}"))?;
        if !matches!(config.provider.as_str(), "duckduckgo" | "searxng" | "brave") {
            return Err("provider must be duckduckgo, searxng or brave".into());
        }
        if !(1..=120).contains(&config.timeout_secs) || !(1..=4_194_304).contains(&config.max_bytes)
        {
            return Err("timeout_secs must be 1..120 and max_bytes 1..4194304".into());
        }
        if config.provider == "searxng" {
            parse_url(
                config
                    .base_url
                    .as_deref()
                    .ok_or("searxng requires base_url")?,
            )?;
        } else if config.base_url.is_some() {
            return Err("base_url is only supported for searxng".into());
        }
        if config.api_key_env.is_empty() || config.api_key_env.contains(['=', '\0']) {
            return Err("invalid api_key_env name".into());
        }
        self.config = config;
        Ok(())
    }

    fn tools(&self) -> Value {
        json!([
            tool(
                "web_search",
                "Search the web; returns JSON results with title, URL and snippet.",
                json!({"query": {"type": "string", "minLength": 1, "maxLength": 4096}, "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 5}}),
                &["query"],
                false
            ),
            tool(
                "web_fetch",
                "Fetch HTTP(S) readable text. Redirects, oversized responses and private networks are denied by default.",
                json!({"url": {"type": "string"}}),
                &["url"],
                false
            )
        ])
    }

    fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
        match name {
            "web_fetch" => {
                let url = parse_url(args["url"].as_str().ok_or("url must be a string")?)?;
                let (body, kind) = self.get(&url, None)?;
                Ok(if kind == "text/html" || kind == "application/xhtml+xml" {
                    readable(&body)
                } else {
                    body
                })
            }
            "web_search" => {
                let query = args["query"].as_str().ok_or("query must be a string")?;
                if query.trim().is_empty() || query.len() > 4096 {
                    return Err("query must contain 1..4096 bytes".into());
                }
                let limit = match args.get("limit") {
                    None => 5,
                    Some(value) => value
                        .as_u64()
                        .filter(|n| (1..=20).contains(n))
                        .ok_or("limit must be an integer in 1..20")?
                        as usize,
                };
                self.search(query, limit)
            }
            _ => Err(format!("unknown tool: {name}")),
        }
    }
}

fn main() -> io::Result<()> {
    serve(Server::default())
}

#[cfg(test)]
#[path = "../tests/web.rs"]
mod tests;
