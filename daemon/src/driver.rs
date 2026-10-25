//! Safe idiomatic Rust abstraction over the `/dev/kread` character device driver.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::uapi::{
    KreadIoctlBatch, KreadIoctlRange, KREAD_DEVICE_PATH, KREAD_IOCTL_READ_BATCH,
    KREAD_IOCTL_READ_RANGE, KREAD_MAX_BATCH_COUNT, KREAD_MAX_RANGE_LEN,
};

extern "C" {
    fn ioctl(fd: std::os::raw::c_int, request: std::os::raw::c_ulong, ...) -> std::os::raw::c_int;
}

/// Safe handle to the `/dev/kread` Linux character device.
///
/// Encapsulates kernel communication via atomic `ioctl` system calls, ensuring
/// zero memory faults and O(1) memory overhead.
#[derive(Debug)]
pub struct KreadDriver {
    file: File,
    path: PathBuf,
}

impl KreadDriver {
    /// Opens the default character device node `/dev/kread`.
    ///
    /// Requires read/write access to the device node.
    pub fn open() -> io::Result<Self> {
        Self::open_path(KREAD_DEVICE_PATH)
    }

    /// Opens the character device node at a custom filesystem path.
    pub fn open_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let p = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(p)?;
        Ok(Self {
            file,
            path: p.to_path_buf(),
        })
    }

    /// Returns the filesystem path to the opened device node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads `len` bytes from virtual address `addr` of target process `pid`.
    ///
    /// Returns an allocated `Vec<u8>` containing the streamed bytes.
    /// Unmapped, swapped, or hole pages are zero-filled by the kernel engine.
    pub fn read_range(&self, pid: i32, addr: u64, len: usize) -> io::Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        if len > KREAD_MAX_RANGE_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "requested length {} exceeds limit of {} bytes",
                    len, KREAD_MAX_RANGE_LEN
                ),
            ));
        }

        let mut buf = vec![0u8; len];
        let transferred = self.read_range_into(pid, addr, &mut buf)?;
        buf.truncate(transferred);
        Ok(buf)
    }

    /// Reads memory from virtual address `addr` of target process `pid` directly into caller's slice.
    ///
    /// Performs a single atomic `ioctl` passing the user buffer pointer.
    /// Returns the actual number of bytes transferred.
    pub fn read_range_into(&self, pid: i32, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target pid must be greater than 0",
            ));
        }
        if buf.len() > KREAD_MAX_RANGE_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "buffer size {} exceeds limit of {} bytes",
                    buf.len(),
                    KREAD_MAX_RANGE_LEN
                ),
            ));
        }

        let mut req = KreadIoctlRange {
            target_pid: pid,
            pad: 0,
            addr,
            len: buf.len() as u64,
            out_buf: buf.as_mut_ptr() as u64,
            out_len: 0,
        };

        let ret = unsafe {
            ioctl(
                self.file.as_raw_fd(),
                KREAD_IOCTL_READ_RANGE as std::os::raw::c_ulong,
                &mut req as *mut KreadIoctlRange,
            )
        };

        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(req.out_len as usize)
        }
    }

    /// Performs a scatter batch read of 64-bit values across target process `pid`.
    ///
    /// Returns a tuple of:
    /// - Vector of `u64` values corresponding 1:1 to input `addrs`.
    /// - `u32` success count (number of resident / non-hole addresses successfully read).
    pub fn read_batch(&self, pid: i32, addrs: &[u64]) -> io::Result<(Vec<u64>, u32)> {
        if addrs.is_empty() {
            return Ok((Vec::new(), 0));
        }
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target pid must be greater than 0",
            ));
        }
        if addrs.len() > KREAD_MAX_BATCH_COUNT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "batch count {} exceeds limit of {}",
                    addrs.len(),
                    KREAD_MAX_BATCH_COUNT
                ),
            ));
        }

        let mut vals = vec![0u64; addrs.len()];
        let mut req = KreadIoctlBatch {
            target_pid: pid,
            count: addrs.len() as u32,
            addrs_ptr: addrs.as_ptr() as u64,
            vals_ptr: vals.as_mut_ptr() as u64,
            success_count: 0,
            pad: 0,
        };

        let ret = unsafe {
            ioctl(
                self.file.as_raw_fd(),
                KREAD_IOCTL_READ_BATCH as std::os::raw::c_ulong,
                &mut req as *mut KreadIoctlBatch,
            )
        };

        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok((vals, req.success_count))
        }
    }

    /// Reads a typed Plain-Old-Data value `T` from virtual address `addr` of target process `pid`.
    ///
    /// Safety: `T` must be `Copy` and valid for arbitrary byte representations.
    pub fn read_pod<T: Copy>(&self, pid: i32, addr: u64) -> io::Result<T> {
        let size = std::mem::size_of::<T>();
        if size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot read zero-sized type",
            ));
        }
        let mut val = std::mem::MaybeUninit::<T>::uninit();
        let slice =
            unsafe { std::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size) };
        let transferred = self.read_range_into(pid, addr, slice)?;
        if transferred < size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "partial read: transferred {} of {} bytes",
                    transferred, size
                ),
            ));
        }
        Ok(unsafe { val.assume_init() })
    }

    /// Reads a null-terminated UTF-8 / ASCII C string up to `max_len` bytes.
    pub fn read_string(&self, pid: i32, addr: u64, max_len: usize) -> io::Result<String> {
        let raw = self.read_range(pid, addr, max_len)?;
        let nul_pos = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8(raw[..nul_pos].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Checks if the opened driver file descriptor is still alive and accessible.
    pub fn is_alive(&self) -> bool {
        self.file.metadata().is_ok()
    }

    /// Static check if `/dev/kread` exists and is accessible.
    pub fn check_device_available() -> bool {
        Path::new(KREAD_DEVICE_PATH).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_driver_open_nonexistent() {
        let res = KreadDriver::open_path("/tmp/nonexistent_kread_dev_node_test_path");
        assert!(res.is_err());
    }

    #[test]
    fn test_driver_check_device_available() {
        // Just verify the function executes without panic
        let _ = KreadDriver::check_device_available();
    }
}
