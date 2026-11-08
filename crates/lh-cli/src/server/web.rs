//! Immutable same-origin shell. No filesystem paths or user content are served.
use super::{Reply, bytes_response};
use hyper::{Method, StatusCode};

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

pub(super) fn asset(method: &Method, path: &str) -> Option<Reply> {
    let (content, mime) = match path {
        "/" | "/index.html" => (
            include_str!("../../web/index.html"),
            "text/html; charset=utf-8",
        ),
        "/app.mjs" => (
            include_str!("../../web/app.mjs"),
            "text/javascript; charset=utf-8",
        ),
        "/markdown.mjs" => (
            include_str!("../../web/markdown.mjs"),
            "text/javascript; charset=utf-8",
        ),
        "/stats.mjs" => (
            include_str!("../../web/stats.mjs"),
            "text/javascript; charset=utf-8",
        ),
        "/style.css" => (
            include_str!("../../web/style.css"),
            "text/css; charset=utf-8",
        ),
        "/layout.css" => (
            include_str!("../../web/layout.css"),
            "text/css; charset=utf-8",
        ),
        _ => return None,
    };
    let allowed = method == Method::GET || method == Method::HEAD;
    let mut reply = bytes_response(if method == Method::GET { content } else { "" }, mime);
    let headers = reply.headers_mut();
    headers.insert("content-security-policy", CSP.parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    if method == Method::HEAD {
        headers.insert("content-length", content.len().into());
    }
    if !allowed {
        headers.insert("allow", "GET, HEAD".parse().unwrap());
        *reply.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    }
    Some(reply)
}
