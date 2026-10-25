//! Safe idiomatic Rust abstraction over the `/dev/kread` character device driver.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::uapi::{
    KreadIoctlBatch, KreadIoctlRange, KREAD_DEVICE_PATH, KREAD_IOCTL_READ_BATCH,
    KREAD_IOCTL_READ_RANGE, KREAD_MAX_BATCH_COUNT, KREAD_MAX_RANGE_LEN,
};

unsafe extern "C" {
    fn ioctl(fd: std::os::raw::c_int, request: std::os::raw::c_ulong, ...) -> std::os::raw::c_int;
}

/// Marker trait for Plain-Old-Data types that can be safely read from arbitrary raw memory.
///
/// # Safety
/// Implementors guarantee that:
/// 1. The type implements `Copy`.
/// 2. Any arbitrary byte sequence of length `std::mem::size_of::<Self>()` represents a
///    valid, initialized instance of `Self`.
/// 3. The type has no invalid bit patterns (e.g. `bool`, `char`, enums with unused discriminants,
///    references, `NonNull`, or `NonZero*`).
/// 4. The type has no uninitialized padding bytes that would produce Undefined Behavior if read.
pub unsafe trait Pod: Copy {}

macro_rules! impl_pod {
    ($($t:ty),*) => {
        $( unsafe impl Pod for $t {} )*
    };
}

impl_pod!(
    u8, u16, u32, u64, u128, usize,
    i8, i16, i32, i64, i128, isize,
    f32, f64, ()
);

unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}
unsafe impl Pod for KreadIoctlRange {}
unsafe impl Pod for KreadIoctlBatch {}

/// Abstract trait for cross-process memory reading operations.
///
/// Enables mock implementations in CI and unit tests without requiring a physical
/// Android device or the `/dev/kread` kernel module.
pub trait MemoryReader: Send + Sync {
    /// Reads memory from virtual address `addr` of target process `pid` directly into `buf`.
    ///
    /// Returns the number of bytes transferred.
    fn read_range(&self, pid: i32, addr: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Performs a scatter batch read of 64-bit values for `addrs` into `vals`.
    ///
    /// Returns the number of resident / non-hole addresses successfully read.
    fn read_batch(&self, pid: i32, addrs: &[u64], vals: &mut [u64]) -> io::Result<u32>;
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
    /// Validates that the kernel-reported `out_len` does not exceed the caller buffer capacity.
    pub(crate) fn validate_range_out_len(out_len: u64, buf_len: usize) -> io::Result<usize> {
        let actual = out_len as usize;
        if actual > buf_len {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "kernel returned out_len ({}) exceeding buffer capacity ({})",
                    out_len, buf_len
                ),
            ))
        } else {
            Ok(actual)
        }
    }

    /// Validates that the kernel-reported `success_count` does not exceed the requested batch count.
    pub(crate) fn validate_batch_success_count(
        success_count: u32,
        requested_count: u32,
    ) -> io::Result<u32> {
        if success_count > requested_count {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "kernel returned success_count ({}) exceeding batch count ({})",
                    success_count, requested_count
                ),
            ))
        } else {
            Ok(success_count)
        }
    }

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
    /// Returns an empty `Vec<u8>` if `len == 0` or target memory contains 0 bytes.
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
    ///
    /// Clamps `req.out_len` against `buf.len()`, returning `InvalidData` if the kernel
    /// reports an out_len exceeding buffer capacity. A 0-byte return represents an empty
    /// range or end of accessible virtual address space (EOF).
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

        // SAFETY:
        // 1. AAPCS64 Calling Convention: On aarch64 Linux, variadic arguments follow the exact same
        //    register assignment sequence as named parameters. The first argument (fd) is placed in w0/x0,
        //    the request code in x1, and the third argument (&mut req as *mut KreadIoctlRange) is passed
        //    in register x2. In Musl, Bionic, and Glibc, ioctl retrieves the pointer via va_arg(ap, void*),
        //    loading x2, and passes it directly to sys_ioctl(fd, cmd, arg).
        // 2. Struct Validity & Lifetime: `req` is a repr(C) structure matching `struct kread_ioctl_range`
        //    in `kread_uapi.h` (40 bytes, 8-byte aligned). It is allocated on the local stack frame and
        //    guaranteed valid for the duration of the synchronous ioctl system call.
        // 3. Buffer Invariant: `buf.as_mut_ptr()` points to a valid, exclusive mutable memory slice
        //    with length `buf.len()`. The kernel module page walker verifies user access and writes
        //    at most `req.len` (buf.len()) bytes into `out_buf`.
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
            Self::validate_range_out_len(req.out_len, buf.len())
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
        let mut vals = vec![0u64; addrs.len()];
        let success = self.read_batch_into(pid, addrs, &mut vals)?;
        Ok((vals, success))
    }

    /// Performs a scatter batch read of 64-bit values directly into caller's slice `vals`.
    ///
    /// Avoids heap allocations during repeated scatter read operations.
    pub fn read_batch_into(&self, pid: i32, addrs: &[u64], vals: &mut [u64]) -> io::Result<u32> {
        if addrs.is_empty() || vals.is_empty() {
            return Ok(0);
        }
        if pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target pid must be greater than 0",
            ));
        }
        let count = addrs.len().min(vals.len());
        if count > KREAD_MAX_BATCH_COUNT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "batch count {} exceeds limit of {}",
                    count,
                    KREAD_MAX_BATCH_COUNT
                ),
            ));
        }

        let mut req = KreadIoctlBatch {
            target_pid: pid,
            count: count as u32,
            addrs_ptr: addrs.as_ptr() as u64,
            vals_ptr: vals.as_mut_ptr() as u64,
            success_count: 0,
            pad: 0,
        };

        // SAFETY:
        // 1. AAPCS64 Calling Convention: On aarch64 Linux, the third argument (&mut req as *mut KreadIoctlBatch)
        //    is loaded into register x2. Musl/Bionic/Glibc va_arg(ap, void*) extracts x2 directly into the syscall.
        // 2. Struct Validity & Lifetime: `req` is repr(C) (32 bytes, 8-byte aligned) matching `struct kread_ioctl_batch`
        //    in `kread_uapi.h`. It is stack-pinned and outlives the ioctl syscall.
        // 3. Memory Pointers: `addrs_ptr` points to an immutable slice of `count` u64 virtual addresses.
        //    `vals_ptr` points to a mutable slice of `count` u64 destination values. Both slices are valid
        //    and properly aligned for 64-bit access.
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
            Self::validate_batch_success_count(req.success_count, count as u32)
        }
    }

    /// Reads a typed Plain-Old-Data value `T` from virtual address `addr` of target process `pid`.
    ///
    /// This method is safe for all types implementing the [`Pod`] trait.
    pub fn read_pod<T: Pod>(&self, pid: i32, addr: u64) -> io::Result<T> {
        let size = std::mem::size_of::<T>();
        if size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot read zero-sized type",
            ));
        }
        let mut val = std::mem::MaybeUninit::<T>::zeroed();

        // SAFETY:
        // 1. `val` is initialized with zeroes, ensuring all `size` bytes are valid initialized
        //    `u8` values prior to constructing the mutable slice.
        // 2. `val.as_mut_ptr() as *mut u8` is aligned for u8 (alignment 1) and valid for writes of `size` bytes.
        // 3. The slice lifetime is constrained to this call frame and does not alias.
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

        // SAFETY:
        // 1. `read_range_into` returned `transferred == size`, confirming all `size` bytes were populated.
        // 2. `T` implements the unsafe trait `Pod`, guaranteeing every possible bit pattern of length `size`
        //    is a valid, sound value of type `T`.
        Ok(unsafe { val.assume_init() })
    }

    /// Reads a typed value `T` from virtual address `addr` of target process `pid` without
    /// validating that arbitrary bit patterns represent valid values of `T`.
    ///
    /// # Safety
    /// Caller must guarantee that the bit pattern read from remote process memory constitutes
    /// a valid representation of type `T`.
    pub unsafe fn read_pod_unchecked<T: Copy>(&self, pid: i32, addr: u64) -> io::Result<T> {
        let size = std::mem::size_of::<T>();
        if size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot read zero-sized type",
            ));
        }
        let mut val = std::mem::MaybeUninit::<T>::zeroed();

        // SAFETY:
        // 1. `val` is zero-initialized, ensuring all `size` bytes are valid initialized `u8`.
        // 2. `val.as_mut_ptr() as *mut u8` is aligned for u8 and valid for writes of `size` bytes.
        // 3. The slice lifetime is constrained to this call frame and does not alias.
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

        // SAFETY:
        // Caller has guaranteed that arbitrary bit patterns are valid for `T`.
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

impl MemoryReader for KreadDriver {
    fn read_range(&self, pid: i32, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.read_range_into(pid, addr, buf)
    }

    fn read_batch(&self, pid: i32, addrs: &[u64], vals: &mut [u64]) -> io::Result<u32> {
        self.read_batch_into(pid, addrs, vals)
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

    struct MockReaderImpl;

    impl MemoryReader for MockReaderImpl {
        fn read_range(&self, _pid: i32, _addr: u64, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(0x42);
            Ok(buf.len())
        }

        fn read_batch(&self, _pid: i32, addrs: &[u64], vals: &mut [u64]) -> io::Result<u32> {
            for v in vals.iter_mut() {
                *v = 0x12345678;
            }
            Ok(addrs.len() as u32)
        }
    }

    #[test]
    fn test_memory_reader_trait_mockability() {
        let mock = MockReaderImpl;
        let mut buf = [0u8; 16];
        let n = mock.read_range(100, 0x1000, &mut buf).unwrap();
        assert_eq!(n, 16);
        assert_eq!(buf, [0x42; 16]);

        let addrs = [0x1000u64, 0x1008u64];
        let mut vals = [0u64; 2];
        let count = mock.read_batch(100, &addrs, &mut vals).unwrap();
        assert_eq!(count, 2);
        assert_eq!(vals, [0x12345678, 0x12345678]);
    }

    #[test]
    fn test_read_range_clamp_logic() {
        // Exercise the actual production validation logic on KreadDriver
        let res_over = KreadDriver::validate_range_out_len(32, 16);
        assert!(res_over.is_err());
        assert_eq!(res_over.unwrap_err().kind(), io::ErrorKind::InvalidData);

        let res_exact = KreadDriver::validate_range_out_len(16, 16);
        assert_eq!(res_exact.unwrap(), 16);

        let res_partial = KreadDriver::validate_range_out_len(8, 16);
        assert_eq!(res_partial.unwrap(), 8);

        let res_zero = KreadDriver::validate_range_out_len(0, 16);
        assert_eq!(res_zero.unwrap(), 0);

        assert!(KreadDriver::validate_batch_success_count(10, 5).is_err());
        assert_eq!(KreadDriver::validate_batch_success_count(5, 5).unwrap(), 5);
        assert_eq!(KreadDriver::validate_batch_success_count(0, 5).unwrap(), 0);
    }

    #[test]
    fn test_empty_buffer_and_slice_handling() {
        // Opening a dummy file to test input validation without requiring /dev/kread
        let temp_file = std::env::temp_dir().join("kread_dummy_unit_test.tmp");
        let _ = std::fs::File::create(&temp_file).unwrap();
        let driver = KreadDriver::open_path(&temp_file).unwrap();

        let mut empty_buf = [0u8; 0];
        assert_eq!(driver.read_range_into(1234, 0x1000, &mut empty_buf).unwrap(), 0);
        assert_eq!(driver.read_range(1234, 0x1000, 0).unwrap(), Vec::<u8>::new());

        let mut vals = [0u64; 0];
        let addrs = [0u64; 0];
        assert_eq!(driver.read_batch_into(1234, &addrs, &mut vals).unwrap(), 0);
        assert_eq!(driver.read_batch(1234, &addrs).unwrap(), (Vec::new(), 0));

        // Invalid PID (< 0) validation
        let mut buf = [0u8; 8];
        assert_eq!(
            driver.read_range_into(-1, 0x1000, &mut buf).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let mut vals_nonempty = [0u64; 1];
        let addrs_nonempty = [0x1000u64];
        assert_eq!(
            driver.read_batch_into(-1, &addrs_nonempty, &mut vals_nonempty).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        // Zero-sized POD type validation
        assert_eq!(
            driver.read_pod::<()>(1234, 0x1000).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            driver.read_pod::<[u8; 0]>(1234, 0x1000).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let _ = std::fs::remove_file(&temp_file);
    }
}
