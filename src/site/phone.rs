//! The phone server: a socket, TLS, and the three parts that do the work.
//!
//! This is the only file in the new layer that knows about networking. It takes
//! a connection, wraps it in TLS, and hands the request loop a `Gate`. Anything
//! that can be decided without a socket lives elsewhere and is tested there.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use super::api::Engine;
use super::db::Db;
use super::gate::Gate;
use super::listener;
use super::tls;
use std::sync::Mutex;

pub struct PhoneServer {
    addr: SocketAddr,
    running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PhoneServer {
    /// Bind and start answering. Binding happens before the thread starts, so a
    /// busy port is reported to the caller instead of vanishing into a log.
    pub fn bind(
        root: &Path,
        port: u16,
        db: Arc<Mutex<Db>>,
        engine: Arc<dyn Engine + Send + Sync>,
        static_dir: PathBuf,
    ) -> Result<Self> {
        // Windows keeps the port in TIME_WAIT for minutes after the phone's
        // connections close. Binding without SO_REUSEADDR then fails with
        // WSAEADDRINUSE even though nothing listens, so closing and reopening
        // the app (or flipping the switch) looked like a broken server.
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .context("cannot create the listening socket")?;
        socket
            .set_reuse_address(true)
            .context("cannot make the phone port reusable")?;
        let address = SocketAddr::from(([0, 0, 0, 0], port));
        socket
            .bind(&address.into())
            .with_context(|| format!("cannot listen on port {port}"))?;
        socket.listen(128).context("cannot listen for connections")?;
        let listener: TcpListener = socket.into();
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        // The certificate must cover every address this machine has right now.
        let names = super::gate::address_names();
        let certificate = tls::ensure(root, &names)?;
        // The page lives next to the executable, the certificate in the data
        // directory: the gate gets the path, it does not guess it from the page.
        let ca = tls::authority_path(root);
        let config = Arc::new(certificate.server_config()?);
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let thread = std::thread::spawn(move || {
            while flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let config = config.clone();
                        let db = db.clone();
                        let engine = engine.clone();
                        let static_dir = static_dir.clone();
                        let ca = ca.clone();
                        std::thread::spawn(move || {
                            answer_one(stream, config, db, engine, static_dir, ca);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        crate::log::log(&format!("phone: listening on {addr}"));
        Ok(Self {
            addr,
            running,
            thread: Some(thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(mut self) {
        self.wait();
    }

    /// Ask the accept loop to finish and wait until the socket is released.
    ///
    /// Switching the phone server off and on again must free the port before
    /// the next binder asks for it: `halt` alone returns while the listener is
    /// still open, and the rebind fails with "address already in use".
    pub fn wait(&mut self) {
        self.halt();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Ask the accept loop to finish without waiting for it.
    pub fn halt(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for PhoneServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

fn answer_one(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
    db: Arc<Mutex<Db>>,
    engine: Arc<dyn Engine + Send + Sync>,
    static_dir: PathBuf,
    ca: PathBuf,
) {
    // Windows hands accepted sockets the listener's non-blocking flag: without
    // this the first read fails with 10035 and the connection dies silently.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
    let Ok(mut connection) = rustls::ServerConnection::new(config) else {
        return;
    };
    let mut stream = stream;
    let mut tls = rustls::Stream::new(&mut connection, &mut stream);
    let now = chrono::Local::now().to_rfc3339();
    let gate = Gate {
        db: &db,
        engine: engine.as_ref(),
        static_dir: &static_dir,
        ca: &ca,
        now: &now,
    };
    listener::serve_connection(&mut tls, &gate);
}

/// Where the phone page lives: next to the executable, like every other asset.
pub fn static_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("site-app")))
        .unwrap_or_else(|| PathBuf::from("site-app"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpStream as Client;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vb-phone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    struct Nothing;
    impl Engine for Nothing {
        fn audio(&self, _: u64, _: &[u8]) {}
        fn finish(&self, _: u64) {}
        fn retry(&self, _: u64) {}
        fn cancel(&self, _: u64) {}
    }

    #[test]
    fn the_server_binds_a_free_port_and_answers_a_connection() {
        let root = temp_root("bind");
        let db = Arc::new(Mutex::new(Db::open(&root).unwrap()));
        let server = PhoneServer::bind(
            &root,
            0,
            db,
            Arc::new(Nothing),
            root.join("site-app"),
        )
        .expect("bind");
        let port = server.addr().port();
        assert_ne!(port, 0, "the system chose a port");
        assert_ne!(
            std::net::TcpListener::bind(("0.0.0.0", port)).is_ok(),
            true,
            "the port is taken by the server"
        );

        // A raw connection is accepted: the TLS handshake fails, but the server
        // must survive it and keep listening.
        {
            let mut client = Client::connect(("127.0.0.1", port)).expect("connect");
            let mut buffer = [0u8; 16];
            let _ = client.read(&mut buffer);
        }
        let again = Client::connect(("127.0.0.1", port)).expect("the server is still there");
        drop(again);
        server.stop();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_busy_port_is_reported_instead_of_ignored() {
        let root = temp_root("busy");
        let taken = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let db = Arc::new(Mutex::new(Db::open(&root).unwrap()));
        let error = PhoneServer::bind(&root, port, db, Arc::new(Nothing), root.join("site-app"));
        assert!(error.is_err(), "binding a taken port must fail loudly");
        drop(taken);
        let _ = std::fs::remove_dir_all(root);
    }
}
