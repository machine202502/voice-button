//! The phone layer: monologs, messages, stages, the queue, the HTTPS API and
//! the pairing gate.
//!
//! Each part has one job and its own tests: `db` stores, `monolog` owns the
//! stage machine, `queue` decides what to decode, `api` is the HTTP contract,
//! `gate` is the door, `phone`/`listener` are the socket, `engine` runs the
//! recognizer and `runtime` starts everything together. `network` is the
//! firewall/UPnP helper the window drives, `tls` the local certificate.

pub mod api;
pub mod cut;
pub mod db;
pub mod engine;
pub mod gate;
pub mod hub;
pub mod listener;
pub mod monolog;
pub mod network;
pub mod phone;
pub mod queue;
pub mod runtime;
pub mod tls;
