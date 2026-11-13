//! The little bit of HTTP both servers need: read one request, write one
//! response.
//!
//! `api` (plain HTTP on the loopback for scripts) and `site` (HTTPS for the
//! phone) speak the same shape, so the parsing lives here once. One request per
//! connection: the response closes it.

use std::io::{BufRead, Read, Write};

/// Largest request the servers accept: headers plus a JSON body. Audio chunks
/// are bounded separately by the route that reads them.
pub const MAX_REQUEST: usize = 1024 * 1024;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// First header with this name, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Extra headers, in order, written after the standard ones.
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn new(status: u16, content_type: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            body: body.into(),
            headers: Vec::new(),
        }
    }

    pub fn json(status: u16, value: serde_json::Value) -> Self {
        Self::new(
            status,
            "application/json",
            serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()).into_bytes(),
        )
    }

    pub fn empty(status: u16) -> Self {
        Self::new(status, "text/plain", Vec::new())
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Read one HTTP request (headers + `Content-Length` body). Works over a plain
/// socket and over a TLS stream alike.
/// Read one request from a buffered reader.
///
/// The unbuffered parser below reads a whole chunk and throws away whatever came
/// after the body — which is exactly what a browser does when it pipelines a
/// second request. A `BufReader` keeps the leftovers, so both requests survive.
pub fn read_request_buffered<R: BufRead>(reader: &mut R) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.trim_end().split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    if method.is_empty() || target.is_empty() {
        return None;
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_REQUEST {
        return None;
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

pub fn read_request<R: Read>(stream: &mut R) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    };
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_REQUEST {
        return None;
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[body_start..body_start + content_length].to_vec();
    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

pub fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn write_response<W: Write>(stream: &mut W, response: &Response) -> std::io::Result<()> {
    write_response_keep_alive(stream, response, false)
}

/// `keep_alive` leaves the connection open for the next request. Browsers send
/// a second request on the same socket without waiting, so a server that closes
/// after every answer loses it and the page waits forever.
pub fn write_response_keep_alive<W: Write>(
    stream: &mut W,
    response: &Response,
    keep_alive: bool,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(if keep_alive {
        "Connection: keep-alive\r\n\r\n"
    } else {
        "Connection: close\r\n\r\n"
    });
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// Status line text. Anything unusual is still answered; only the label is
/// generic.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

/// Value of one `?name=value` pair, percent-decoded enough for our tokens.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(percent_decode(value));
        }
    }
    None
}

/// Static files and query strings are ASCII URLs; decode the escapes a token
/// could contain and leave everything else as it is.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_parsed_with_headers_and_a_body() {
        let raw = b"POST /site/api/messages HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer abc\r\nContent-Length: 3\r\n\r\nxyz";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let request = read_request(&mut cursor).expect("parsed");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/site/api/messages");
        assert_eq!(request.header("authorization"), Some("Bearer abc"));
        assert_eq!(request.body, b"xyz");
    }

    #[test]
    fn a_response_carries_its_extra_headers() {
        let response = Response::json(200, serde_json::json!({"ok": true}))
            .with_header("Cache-Control", "no-store");
        let mut out = Vec::new();
        write_response(&mut out, &response).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Cache-Control: no-store\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
    }

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(query_param("t=abc&lang=ru", "t").as_deref(), Some("abc"));
        assert_eq!(query_param("t=a%2Fb", "t").as_deref(), Some("a/b"));
        assert_eq!(query_param("t=", "t").as_deref(), Some(""));
        assert_eq!(query_param("", "t"), None);
    }
}
