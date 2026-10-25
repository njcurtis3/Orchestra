//! `orchestrad`, the headless Orchestra host. The engine and the phone link come from
//! `orchestra-core`; these modules are what a service adds around them.

pub mod config;
pub mod control;
pub mod daemon;
pub mod log;
pub mod preflight;
pub mod qr;
