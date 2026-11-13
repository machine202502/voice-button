//! The door in front of the API: who may pass, what is served without a token,
//! and what the phone sees when its link is dead.
//!
//! Kept apart from the socket on purpose — this is a pure function of a request
//! and the stored state, so every rule (a wrong token, an expired link, a file,
//! a fallback) is a unit test rather than an integration test.
//!
//! Nothing private is ever served without the token: the home network rule and
//! the setup page are the only things that answer without it, and they expose
//! no data.

use std::net::IpAddr;
use std::path::Path;

use crate::http::{self, Request, Response};
use crate::i18n::tr;

use super::api::{self, Ctx};
use super::db::Db;
use std::sync::Mutex;

pub struct Gate<'a> {
    pub db: &'a Mutex<Db>,
    pub engine: &'a dyn api::Engine,
    pub static_dir: &'a Path,
    /// Where the authority certificate lives. It is not inside `static_dir`:
    /// the page is served next to the executable, the certificate is written to
    /// the data directory, so the path is handed in instead of guessed.
    pub ca: &'a Path,
    pub now: &'a str,
}

/// What the gate answers with: a whole response, or a route the caller has to
/// keep open and write itself.
pub enum Answer {
    Response(Response),
    /// `GET /site/api/events`: the token was accepted, the stream has not
    /// started yet. The socket owner writes it.
    Events,
}

pub fn answer(request: &Request, gate: &Gate) -> Answer {
    let path = request.path.as_str();

    // The liveness probe is open: it names nothing and is used to decide whether
    // the local address is reachable from the phone.
    if path == "/site/api/ping" {
        let version = api::API_VERSION;
        return Answer::Response(
            Response::json(200, serde_json::json!({ "ok": true, "api": version }))
                .with_header("Access-Control-Allow-Origin", "*"),
        );
    }

    if path == "/pair" {
        return Answer::Response(pairing_page(request, gate));
    }

    // The certificate is public on purpose: a public key is not a secret, and
    // the phone needs it before it can trust this address.
    if path == "/ca.crt" {
        return Answer::Response(match std::fs::read(gate.ca) {
            Ok(bytes) => Response::new(200, "application/x-x509-ca-cert", bytes).with_header(
                "Content-Disposition",
                "attachment; filename=\"voice-button-ca.crt\"",
            ),
            Err(_) => Response::new(404, "text/plain; charset=utf-8", "no authority yet"),
        });
    }

    if path == "/install" {
        return Answer::Response(static_file("install.html", gate.static_dir));
    }

    if path.starts_with("/site/api/") {
        if !authorised(request, gate) {
            return Answer::Response(Response::json(
                401,
                serde_json::json!({ "ok": false, "error": "pairing required" }),
            ));
        }
        // The first request that proves the token pairs the phone. Doing it on
        // the pairing page would burn the link before the browser follows it.
        {
            let mut db = gate.db.lock().unwrap();
            if !db.data().client.paired {
                db.mark_paired("", gate.now);
                let _ = db.save();
            }
        }
        if api::is_events_path(request) {
            // The stream is GET-only: anything else gets an ordinary answer
            // and the connection closes instead of being held open.
            if request.method != "GET" {
                return Answer::Response(Response::json(
                    405,
                    serde_json::json!({ "ok": false, "error": "the event stream is GET only" }),
                ));
            }
            return Answer::Events;
        }
        let ctx = Ctx {
            db: gate.db,
            engine: gate.engine,
            now: gate.now,
        };
        return Answer::Response(api::handle(request, &ctx));
    }

    Answer::Response(static_file(path, gate.static_dir))
}

/// A token is the only key. An unused pairing link dies with its five minutes;
/// a phone that already paired keeps its token.
///
/// The header is the normal way. The event stream also accepts the token in
/// `?t=`: `EventSource` cannot send headers, and a client that opens the stream
/// that way must still be refused before any event is written.
fn authorised(request: &Request, gate: &Gate) -> bool {
    let token = match request.header("authorization") {
        Some(header) => match header.strip_prefix("Bearer ") {
            Some(token) => token.trim().to_string(),
            None => return false,
        },
        None if api::is_events_path(request) => {
            match http::query_param(&request.query, "t") {
                Some(token) => token,
                None => return false,
            }
        }
        None => return false,
    };
    if token.is_empty() {
        return false;
    }
    let db = gate.db.lock().unwrap();
    if !db.token_matches(&token) {
        return false;
    }
    db.data().client.paired || db.pairing_alive(gate.now)
}

/// The pairing page stores the token in the phone and moves to the app.
fn pairing_page(request: &Request, gate: &Gate) -> Response {
    let token = http::query_param(&request.query, "t").unwrap_or_default();
    // Served in the language of the computer, which is the one the person
    // reading the screen has been using.
    let (title, line, keep) = {
        let db = gate.db.lock().unwrap();
        if db.data().client.paired {
            (
                tr("The link has already been used"),
                tr("This phone is already connected. Ask for a new QR code on the computer."),
                None,
            )
        } else if !db.token_matches(&token) {
            (
                tr("The link is not valid"),
                tr("Ask for a new QR code on the computer."),
                None,
            )
        } else if !db.pairing_alive(gate.now) {
            (
                tr("The link has expired"),
                tr("The link is valid for five minutes. Ask for a new QR code on the computer."),
                None,
            )
        } else {
            (
                tr("Connecting"),
                tr("The phone is connecting to the computer."),
                Some(token),
            )
        }
    };
    let script = match keep {
        Some(token) => format!(
            "<script>localStorage.setItem('vb_token', {token:?});location.replace('/');</script>"
        ),
        None => String::new(),
    };
    let lang = crate::i18n::active_code();
    let body = format!(
        "<!DOCTYPE html><html lang=\"{lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Voice Button</title><style>body{{background:#111713;color:#e7f0e0;\
         font:16px/1.5 system-ui;margin:0;display:grid;place-items:center;height:100vh}}\
         main{{max-width:340px;padding:24px;text-align:center}}h1{{font-size:1.2em}}</style>\
         </head><body><main><h1>{title}</h1><p>{line}</p></main>{script}</body></html>"
    );
    Response::new(200, "text/html; charset=utf-8", body)
}

/// Files of the phone page. Anything unknown falls back to the app itself, so a
/// deep link opened fresh still loads.
fn static_file(path: &str, dir: &Path) -> Response {
    let mut relative = path.trim_start_matches('/');
    if relative.is_empty() {
        relative = "index.html";
    }
    if relative.contains("..") {
        return Response::new(400, "text/plain; charset=utf-8", "bad path");
    }
    let file = dir.join(relative);
    let file = if file.is_file() {
        file
    } else {
        dir.join("index.html")
    };
    let Ok(bytes) = std::fs::read(&file) else {
        return Response::new(404, "text/plain; charset=utf-8", "nothing here");
    };
    let response = Response::new(200, content_type(&file), bytes);
    // The page and its service worker must never be answered from the HTTP
    // cache: an installed app that keeps an old shell shows an old screen
    // forever, and no update can reach it.
    if relative == "sw.js" || relative == "version.json" || relative.ends_with(".html") {
        return response.with_header("Cache-Control", "no-store");
    }
    response
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "crt" => "application/x-x509-ca-cert",
        _ => "application/octet-stream",
    }
}

/// Port the phone connects to when the user does not pick another one.
pub const DEFAULT_SITE_PORT: u16 = 8443;

/// Token handed to a phone: 16 random bytes, hex. Long enough that guessing is
/// pointless, short enough to put in a QR code.
pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // Extremely unlikely; a time-based fallback still beats an empty token.
        let now = chrono::Local::now().timestamp_nanos_opt().unwrap_or_default();
        bytes[..8].copy_from_slice(&now.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Address a phone should open: the first local IPv4 plus the port.
pub fn display_url(port: u16) -> String {
    match local_ipv4().first() {
        Some(ip) => format!("https://{ip}:{port}/"),
        None => format!("https://127.0.0.1:{port}/"),
    }
}

/// Every address the certificate should cover: the loopback for local checks
/// and all IPv4 addresses the machine currently has, so the phone can reach it
/// over the local network.
pub fn address_names() -> Vec<String> {
    let mut names = vec!["127.0.0.1".to_string(), "localhost".to_string()];
    for addr in local_ipv4() {
        let text = addr.to_string();
        if !names.contains(&text) {
            names.push(text);
        }
    }
    names
}

/// IPv4 addresses of this machine that a phone could reach, best first.
///
/// A machine behind a router has only private addresses; a machine with a real
/// ("white") address has a public one, and that is what a phone outside the
/// home network must use. Benchmarking ranges that VPN clients hand out
/// (198.18.0.0/15) are skipped: they never lead to this computer.
pub fn local_ipv4() -> Vec<IpAddr> {
    let mut public = Vec::new();
    let mut private = Vec::new();
    if let Ok(list) = if_addrs::get_if_addrs() {
        for interface in list {
            let IpAddr::V4(v4) = interface.ip() else {
                continue;
            };
            if v4.is_loopback() || v4.is_link_local() || is_benchmark(v4) {
                continue;
            }
            let ip = IpAddr::V4(v4);
            if v4.is_private() {
                if !private.contains(&ip) {
                    private.push(ip);
                }
            } else if !public.contains(&ip) {
                public.push(ip);
            }
        }
    }
    public.extend(private);
    public
}

/// First private IPv4 address: what a router on the local network sees.
pub fn private_ipv4() -> Option<IpAddr> {
    local_ipv4()
        .into_iter()
        .find(|ip| matches!(ip, IpAddr::V4(v4) if v4.is_private()))
}

/// Does this machine hold a public address itself? Then there is no router to
/// forward anything on, and UPnP would be noise.
pub fn has_public_ipv4() -> bool {
    local_ipv4().into_iter().any(|ip| match ip {
        IpAddr::V4(v4) => !v4.is_private() && !v4.is_loopback() && !v4.is_link_local(),
        _ => false,
    })
}

/// 198.18.0.0/15 is reserved for benchmarking and is what some VPN clients
/// report as a local address.
fn is_benchmark(addr: std::net::Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::api::Silent;
    use crate::site::db::Db;

    const NOW: &str = "2026-11-12T15:00:00+05:00";

    struct Case {
        db: Mutex<Db>,
        dir: std::path::PathBuf,
        ca: std::path::PathBuf,
    }

    impl Case {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("vb-gate-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("static")).unwrap();
            std::fs::write(root.join("static").join("index.html"), "<!doctype html>app").unwrap();
            std::fs::write(root.join("static").join("app.js"), "console.log(1)").unwrap();
            std::fs::write(
                root.join("static").join("install.html"),
                "<!doctype html>guide",
            )
            .unwrap();
            let ca = crate::site::tls::authority_path(&root);
            std::fs::create_dir_all(ca.parent().unwrap()).unwrap();
            std::fs::write(&ca, b"authority").unwrap();
            let mut db = Db::open(&root).unwrap();
            db.start_pairing("secret", NOW);
            db.save().unwrap();
            Self {
                db: Mutex::new(db),
                dir: root.join("static"),
                ca,
            }
        }

        fn gate(&self) -> Gate<'_> {
            Gate {
                db: &self.db,
                engine: &Silent,
                static_dir: &self.dir,
                ca: &self.ca,
                now: NOW,
            }
        }

        fn ask(&self, method: &str, path: &str, query: &str, token: Option<&str>) -> (u16, String) {
            let mut request = Request {
                method: method.to_string(),
                path: path.to_string(),
                query: query.to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            };
            if let Some(token) = token {
                request
                    .headers
                    .push(("Authorization".to_string(), format!("Bearer {token}")));
            }
            match answer(&request, &self.gate()) {
                Answer::Response(response) => {
                    (response.status, String::from_utf8_lossy(&response.body).to_string())
                }
                Answer::Events => panic!("this request opens the event stream"),
            }
        }

        /// Whether this request is allowed to open the event stream.
        fn events(&self, method: &str, query: &str, token: Option<&str>) -> Option<u16> {
            let mut request = Request {
                method: method.to_string(),
                path: "/site/api/events".to_string(),
                query: query.to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            };
            if let Some(token) = token {
                request
                    .headers
                    .push(("Authorization".to_string(), format!("Bearer {token}")));
            }
            match answer(&request, &self.gate()) {
                Answer::Events => None,
                Answer::Response(response) => Some(response.status),
            }
        }
    }

    #[test]
    fn the_certificate_comes_from_the_data_directory() {
        let case = Case::new("crt");
        // The page directory holds no certificate: a 200 proves the route reads
        // the data directory, which is where the daemon writes it.
        let (status, body) = case.ask("GET", "/ca.crt", "", None);
        assert_eq!(status, 200, "no authority served");
        assert_eq!(body, "authority");
        std::fs::remove_file(&case.ca).unwrap();
        let (status, _) = case.ask("GET", "/ca.crt", "", None);
        assert_eq!(status, 404);
    }

    #[test]
    fn the_probe_answers_without_a_token() {
        let case = Case::new("ping");
        let (status, body) = case.ask("GET", "/site/api/ping", "", None);
        assert_eq!(status, 200);
        assert!(body.contains("api"));
    }

    #[test]
    fn the_api_is_closed_without_the_token() {
        let case = Case::new("closed");
        assert_eq!(case.ask("GET", "/site/api/monologues", "", None).0, 401);
        assert_eq!(case.ask("GET", "/site/api/monologues", "", Some("wrong")).0, 401);
        assert_eq!(
            case.ask("GET", "/site/api/monologues", "", Some("secret")).0,
            200,
            "the right token opens it"
        );
    }

    #[test]
    fn the_event_stream_is_refused_before_it_starts_without_the_token() {
        let case = Case::new("events");
        assert_eq!(case.events("GET", "", None), Some(401), "no token, no stream");
        assert_eq!(
            case.events("GET", "t=wrong", None),
            Some(401),
            "a wrong query token is refused"
        );
        assert_eq!(
            case.events("GET", "", Some("wrong")),
            Some(401),
            "a wrong header token is refused"
        );
        assert_eq!(
            case.events("GET", "", Some("secret")),
            None,
            "the header opens the stream"
        );
        assert_eq!(
            case.events("GET", "t=secret", None),
            None,
            "so does the query, for clients that cannot send headers"
        );
        assert_eq!(
            case.events("HEAD", "", Some("secret")),
            Some(405),
            "only GET keeps the connection open"
        );
    }

    #[test]
    fn an_expired_link_does_not_open_the_api() {
        let case = Case::new("expired");
        {
            let mut db = case.db.lock().unwrap();
            db.data_mut().client.expires = "2000-01-01T00:00:00+05:00".to_string();
        }
        assert_eq!(case.ask("GET", "/site/api/monologues", "", Some("secret")).0, 401);
        let (status, body) = case.ask("GET", "/pair", "t=secret", None);
        assert_eq!(status, 200);
        assert!(body.contains("The link has expired"));
    }

    #[test]
    fn a_paired_phone_keeps_working_after_the_deadline() {
        let case = Case::new("paired");
        {
            let mut db = case.db.lock().unwrap();
            db.mark_paired("5.79.1.2", NOW);
            db.data_mut().client.expires = "2000-01-01T00:00:00+05:00".to_string();
        }
        assert_eq!(case.ask("GET", "/site/api/monologues", "", Some("secret")).0, 200);
    }

    #[test]
    fn the_pairing_page_hands_the_token_to_the_app() {
        let case = Case::new("pair");
        let (status, body) = case.ask("GET", "/pair", "t=secret", None);
        assert_eq!(status, 200);
        assert!(body.contains("localStorage.setItem"), "the token is stored");
        assert!(body.contains("location.replace"), "and the page moves on");
    }

    #[test]
    fn a_used_link_says_so() {
        let case = Case::new("used");
        {
            let mut db = case.db.lock().unwrap();
            db.mark_paired("5.79.1.2", NOW);
        }
        let (_, body) = case.ask("GET", "/pair", "t=secret", None);
        assert!(body.contains("The link has already been used"));

        // A wrong token is reported as such while nobody is paired.
        let fresh = Case::new("used-fresh");
        let (_, body) = fresh.ask("GET", "/pair", "t=nonsense", None);
        assert!(body.contains("The link is not valid"));
    }

    #[test]
    fn files_are_served_and_unknown_paths_fall_back_to_the_app() {
        let case = Case::new("static");
        assert_eq!(case.ask("GET", "/", "", None).1, "<!doctype html>app");
        assert_eq!(case.ask("GET", "/app.js", "", None).1, "console.log(1)");
        assert_eq!(case.ask("GET", "/deep/link", "", None).1, "<!doctype html>app");
        assert_eq!(case.ask("GET", "/install", "", None).1, "<!doctype html>guide");
    }

    #[test]
    fn a_path_cannot_climb_out_of_the_folder() {
        let case = Case::new("climb");
        assert_eq!(case.ask("GET", "/../../etc/passwd", "", None).0, 400);
    }
}
