//! # kread_daemon
//!
//! Production Rust daemon and safe driver bindings for `/dev/kread`.
//!
//! This crate provides:
//! - [`uapi`]: Standard UAPI definitions matching `kread_uapi.h`.
//! - [`driver`]: Safe, idiomatic Rust API [`driver::KreadDriver`] and [`driver::MemoryReader`] trait.
//! - [`status`]: RFC 8259 compliant JSON serialization and daemon health status reporting.
//! - [`bench`]: High-precision benchmark engine with CPU governor warmup and inner batching.
//! - [`examples`]: Diagnostics, hexdump formatters, and status helpers.

pub mod bench;
pub mod driver;
pub mod examples;
pub mod status;
pub mod uapi;

pub use driver::{KreadDriver, MemoryReader, Pod};
pub use status::{escape_json, DaemonStatusReport};
pub use uapi::{
    KreadIoctlBatch, KreadIoctlRange, KREAD_DEVICE_PATH, KREAD_IOCTL_MAGIC,
    KREAD_IOCTL_READ_BATCH, KREAD_IOCTL_READ_RANGE, KREAD_MAX_BATCH_COUNT,
    KREAD_MAX_RANGE_LEN,
};
