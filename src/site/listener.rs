//! The socket part: read requests from one connection, answer them, keep the
//! connection alive while the client wants it.
//!
//! The loop is generic over the stream, so the transport (TLS in the app, an
//! in-memory pipe in tests) never touches the logic. The previous server read a
//! single request per connection and closed it, which lost the second request a
//! browser sends without waiting — and the page waited forever. That failure has
//! its own test here.

use std::io::{Read, Write};

use crate::http::{self, Response};

use super::gate::{self, Gate};

/// How many requests one connection may carry before it is closed anyway.
const MAX_REQUESTS: usize = 64;

/// Serve every request on the connection. Returns how many were answered.
pub fn serve_connection<S: Read + Write>(stream: &mut S, gate: &Gate) -> usize {
    let mut answered = 0;
    // Buffered, so a second request that arrived in the same packet is not
    // thrown away.
    let mut reader = std::io::BufReader::new(stream);
    for _ in 0..MAX_REQUESTS {
        let Some(request) = http::read_request_buffered(&mut reader) else {
            break;
        };
        let wants_close = request
            .header("connection")
            .map(|value| value.eq_ignore_ascii_case("close"))
            .unwrap_or(false);
        let response: Response = gate::answer(&request, gate);
        if http::write_response_keep_alive(reader.get_mut(), &response, !wants_close).is_err() {
            break;
        }
        let _ = reader.get_mut().flush();
        answered += 1;
        if wants_close {
            break;
        }
    }
    answered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::api::Silent;
    use crate::site::db::Db;
    use std::sync::Mutex;

    /// A stream that hands out pre-written requests and records the answers.
    struct Pipe {
        input: Vec<u8>,
        position: usize,
        output: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let left = self.input.len() - self.position;
            if left == 0 {
                // No more data: a real socket would block, a test file ends.
                return Ok(0);
            }
            let take = left.min(buffer.len());
            buffer[..take].copy_from_slice(&self.input[self.position..self.position + take]);
            self.position += take;
            Ok(take)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn gate_with_db() -> (Mutex<Db>, std::path::PathBuf) {
        // Tests run in parallel: each needs its own directory.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let case = NEXT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("vb-listener-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("static")).unwrap();
        std::fs::write(root.join("static").join("index.html"), "app").unwrap();
        let mut db = Db::open(&root).unwrap();
        db.start_pairing("secret", "2026-11-12T15:00:00+05:00");
        db.save().unwrap();
        (Mutex::new(db), root.join("static"))
    }

    fn response_text(output: &[u8]) -> String {
        String::from_utf8_lossy(output).to_string()
    }

    #[test]
    fn two_requests_on_one_connection_get_two_answers() {
        let (db, dir) = gate_with_db();
        let first = concat!(
            "GET /site/api/ping HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        let second = concat!(
            "GET /app.js HTTP/1.1\r\nHost: x\r\n\r\n"
        );
        let mut pipe = Pipe {
            input: format!("{first}{second}").into_bytes(),
            position: 0,
            output: Vec::new(),
        };
        let gate = Gate {
            db: &db,
            engine: &Silent,
            static_dir: &dir,
            ca: &dir,
            now: "2026-11-12T15:00:00+05:00",
        };
        let answered = serve_connection(&mut pipe, &gate);
        let text = response_text(&pipe.output);
        assert_eq!(answered, 2, "the second request must not be lost");
        assert_eq!(text.matches("HTTP/1.1 200").count(), 2);
        assert!(text.contains("\"api\""), "the probe answered");
        assert!(text.contains("app"), "the file answered");
        assert!(
            text.contains("Connection: keep-alive"),
            "the browser may reuse the connection: {text}"
        );
    }

    #[test]
    fn a_client_that_asks_to_close_is_obeyed() {
        let (db, dir) = gate_with_db();
        let request = "GET /site/api/ping HTTP/1.1\r\nConnection: close\r\n\r\n";
        let mut pipe = Pipe {
            input: request.as_bytes().to_vec(),
            position: 0,
            output: Vec::new(),
        };
        let gate = Gate {
            db: &db,
            engine: &Silent,
            static_dir: &dir,
            ca: &dir,
            now: "2026-11-12T15:00:00+05:00",
        };
        let answered = serve_connection(&mut pipe, &gate);
        assert_eq!(answered, 1);
        let text = response_text(&pipe.output);
        assert!(text.contains("Connection: close"), "{text}");
    }

    #[test]
    fn an_empty_connection_answers_nothing() {
        let (db, dir) = gate_with_db();
        let mut pipe = Pipe {
            input: Vec::new(),
            position: 0,
            output: Vec::new(),
        };
        let gate = Gate {
            db: &db,
            engine: &Silent,
            static_dir: &dir,
            ca: &dir,
            now: "2026-11-12T15:00:00+05:00",
        };
        assert_eq!(serve_connection(&mut pipe, &gate), 0);
        assert!(pipe.output.is_empty());
    }

    #[test]
    fn a_refused_request_still_gets_a_framed_answer() {
        let (db, dir) = gate_with_db();
        let request = "GET /site/api/monologues HTTP/1.1\r\n\r\n";
        let mut pipe = Pipe {
            input: request.as_bytes().to_vec(),
            position: 0,
            output: Vec::new(),
        };
        let gate = Gate {
            db: &db,
            engine: &Silent,
            static_dir: &dir,
            ca: &dir,
            now: "2026-11-12T15:00:00+05:00",
        };
        serve_connection(&mut pipe, &gate);
        let text = response_text(&pipe.output);
        assert!(text.starts_with("HTTP/1.1 401"), "{text}");
        assert!(text.contains("Content-Length"), "the answer is framed: {text}");
    }
}
