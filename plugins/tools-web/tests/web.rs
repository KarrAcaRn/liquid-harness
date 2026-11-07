use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

fn fixture(
    status: &str,
    headers: &str,
    body: &str,
    delay_ms: u64,
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            request.push_str(&line);
        }
        thread::sleep(Duration::from_millis(delay_ms));
        let _ = stream.write_all(response.as_bytes());
        request
    });
    (url, handle)
}

fn private_server() -> Server {
    let mut server = Server::default();
    server
        .configure(json!({"allow_private_network": true}))
        .unwrap();
    server
}

#[test]
fn deny_non_global_addresses_and_mixed_dns() {
    for address in [
        "0.0.0.0",
        "10.1.2.3",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.31.1.1",
        "192.168.1.1",
        "192.0.0.8",
        "192.0.2.1",
        "192.88.99.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:8.8.8.8",
        "::ffff:127.0.0.1",
        "64:ff9b::808:808",
        "100::1",
        "2001::1",
        "2001:db8::1",
        "2002:0808:0808::1",
        "3fff::1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
    ] {
        assert!(!global(address.parse().unwrap()), "{address}");
    }
    for address in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700:4700::1111",
        "2001:4860:4860::8888",
    ] {
        assert!(global(address.parse().unwrap()), "{address}");
    }
    let addresses = vec![
        "8.8.8.8:80".parse().unwrap(),
        "127.0.0.1:80".parse().unwrap(),
    ];
    assert!(checked_addresses(addresses.clone(), false).is_err());
    assert_eq!(checked_addresses(addresses, true).unwrap().len(), 2);
    assert!(checked_addresses(vec![], true).is_err());
}

#[test]
fn default_policy_denies_actual_loopback_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut server = Server::default();
    for host in ["127.0.0.1", "localhost", "2130706433", "0x7f000001"] {
        let result = server.call(
            "web_fetch",
            json!({"url": format!("http://{host}:{}/", listener.local_addr().unwrap().port())}),
        );
        assert!(result.is_err(), "{host}");
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn environment_proxies_are_ignored() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::fetch_html_and_entities", "--nocapture"])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn fetch_html_and_entities() {
    let (url, handle) = fixture(
        "200 OK",
        "Content-Type: text/html; charset=utf-8\r\n",
        "<head><title>Hidden</title></head><body><h1>Hello &amp; goodbye</h1><script>bad()</script><style>bad</style><p hidden>secret</p><p>Readable <b>text</b>.</p></body>",
        0,
    );
    let output = private_server()
        .call("web_fetch", json!({"url":url}))
        .unwrap();
    assert_eq!(output, "Hello & goodbye Readable text .");
    assert!(handle.join().unwrap().starts_with("GET / HTTP/1.1"));
}

#[test]
fn size_cap_redirect_and_content_type() {
    for (status, headers, body, expected) in [
        (
            "200 OK",
            "Content-Type: text/plain\r\n",
            "123456",
            "max_bytes",
        ),
        (
            "302 Found",
            "Location: http://127.0.0.1:1/\r\n",
            "",
            "redirects",
        ),
        ("200 OK", "Content-Type: image/png\r\n", "", "content type"),
        ("503 Unavailable", "", "", "503"),
    ] {
        let (url, handle) = fixture(status, headers, body, 0);
        let mut server = private_server();
        server.config.max_bytes = 5;
        let error = server.call("web_fetch", json!({"url":url})).unwrap_err();
        assert!(error.contains(expected), "{error}");
        handle.join().unwrap();
    }
}

#[test]
fn http_timeout() {
    let (url, handle) = fixture("200 OK", "", "hello", 1300);
    let mut server = private_server();
    server.config.timeout_secs = 1;
    assert!(server.call("web_fetch", json!({"url":url})).is_err());
    handle.join().unwrap();
}

#[test]
fn searxng_fake_http_and_limit() {
    let body = json!({"results":[{"title":"One", "url":"https://example.com/", "content":"<b>A &amp; B</b>"}, {"title":"Two", "url":"https://example.org/"}]}).to_string();
    let (url, handle) = fixture("200 OK", "Content-Type: application/json\r\n", &body, 0);
    let mut server = Server::default();
    server.configure(json!({"provider":"searxng", "base_url":format!("{url}/prefix/"), "allow_private_network":true})).unwrap();
    let output: Value = serde_json::from_str(
        &server
            .call("web_search", json!({"query":"a & b", "limit":1}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output["results"].as_array().unwrap().len(), 1);
    assert_eq!(output["results"][0]["snippet"], "A & B");
    assert!(
        handle
            .join()
            .unwrap()
            .starts_with("GET /prefix/search?q=a+%26+b&format=json HTTP/1.1")
    );
}

#[test]
fn provider_parsers() {
    let html = r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F">Example &amp; title</a><div class="result__snippet">A <b>snippet</b></div></div><div class="result"><a class="result__a" href="javascript:alert(1)">Bad</a></div>"#;
    let results = duckduckgo_results(html, 5);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["url"], "https://example.com/");
    assert_eq!(results[0]["title"], "Example & title");
    let results = json_results(r#"{"web":{"results":[{"title":"Brave", "url":"https://example.com/", "description":"A &amp; B"}]}}"#, "brave", 1).unwrap();
    assert_eq!(results[0]["snippet"], "A & B");
    assert!(json_results("{}", "brave", 1).is_err());
}

#[test]
fn validate_config_arguments_and_urls() {
    let mut server = Server::default();
    for config in [
        json!({"provider":"unknown"}),
        json!({"provider":"searxng"}),
        json!({"timeout_secs":0}),
        json!({"max_bytes":4194305}),
        json!({"base_url":"https://example.com"}),
        json!({"api_key_env":"x=y"}),
        json!({"typo":true}),
    ] {
        assert!(server.configure(config).is_err());
    }
    for url in [
        "file:///etc/passwd",
        "ftp://example.com",
        "http://user:pass@example.com",
        "invalid",
    ] {
        assert!(server.call("web_fetch", json!({"url":url})).is_err());
    }
    for args in [
        json!({"query":""}),
        json!({"query":"x","limit":0}),
        json!({"query":"x","limit":21}),
        json!({"query":"x","limit":1.5}),
        json!({"query":42}),
    ] {
        assert!(server.call("web_search", args).is_err());
    }
    assert!(server.call("unknown", json!({})).is_err());
    server
        .configure(json!({"provider":"brave", "api_key_env":"LH_WEB_TEST_MISSING_KEY_73920"}))
        .unwrap();
    assert!(
        server
            .call("web_search", json!({"query":"test"}))
            .unwrap_err()
            .contains("missing")
    );
}
