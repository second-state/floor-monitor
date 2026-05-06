//! Floor Monitor — Camera Client library.
//!
//! Holds the protocol-side code (config parsing, WebSocket command handling,
//! PTZ trait + impls) that is independent of the webcam-capture stack.
//! The binary in `main.rs` adds the `nokhwa` capture loop on top.

pub mod commands;
pub mod config;
pub mod ptz;
