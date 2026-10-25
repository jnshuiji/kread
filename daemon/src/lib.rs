//! # kread_daemon
//!
//! Production Rust daemon and safe driver bindings for `/dev/kread`.
//!
//! This crate provides:
//! - [`uapi`]: Standard UAPI definitions matching `kread_uapi.h`.
//! - [`driver`]: Safe, idiomatic Rust API [`driver::KreadDriver`] for `/dev/kread`.
//! - [`examples`]: Diagnostics, hexdump formatters, and status helpers.

pub mod driver;
pub mod examples;
pub mod uapi;

pub use driver::KreadDriver;
pub use uapi::{
    KreadIoctlBatch, KreadIoctlRange, KREAD_DEVICE_PATH, KREAD_IOCTL_MAGIC,
    KREAD_IOCTL_READ_BATCH, KREAD_IOCTL_READ_RANGE, KREAD_MAX_BATCH_COUNT,
    KREAD_MAX_RANGE_LEN,
};
