//! Standard UAPI definitions for /dev/kread ioctl interface.
//!
//! Binary layout matches `include/kread_uapi.h`:
//! - `KREAD_IOCTL_READ_RANGE`: Atomic cross-process range read directly streaming into caller buffer.
//! - `KREAD_IOCTL_READ_BATCH`: Atomic cross-process scatter batch read into caller value array.

/// Magic number for kread ioctl requests ('k' = 0x6b).
pub const KREAD_IOCTL_MAGIC: u8 = b'k';

/// Number of bits allocated for the ioctl sequence number.
pub const IOC_NRBITS: u32 = 8;
/// Number of bits allocated for the ioctl magic type field.
pub const IOC_TYPEBITS: u32 = 8;
/// Number of bits allocated for the ioctl data size field.
pub const IOC_SIZEBITS: u32 = 14;
/// Number of bits allocated for the ioctl data transfer direction field.
pub const IOC_DIRBITS: u32 = 2;

/// Bit shift for ioctl sequence number.
pub const IOC_NRSHIFT: u32 = 0;
/// Bit shift for ioctl magic type field.
pub const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
/// Bit shift for ioctl data size field.
pub const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
/// Bit shift for ioctl data direction field.
pub const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

/// Direction bitmask representing no data transfer.
pub const IOC_NONE: u32 = 0;
/// Direction bitmask representing data written by user to driver.
pub const IOC_WRITE: u32 = 1;
/// Direction bitmask representing data read by user from driver.
pub const IOC_READ: u32 = 2;

/// Computes Linux ioctl request code.
#[inline]
pub const fn _ioc(dir: u32, type_: u8, nr: u32, size: usize) -> u64 {
    ((dir as u64) << IOC_DIRSHIFT)
        | ((type_ as u64) << IOC_TYPESHIFT)
        | ((nr as u64) << IOC_NRSHIFT)
        | ((size as u64) << IOC_SIZESHIFT)
}

/// Computes Linux read-write ioctl request code (_IOWR).
#[inline]
pub const fn _iowr(type_: u8, nr: u32, size: usize) -> u64 {
    _ioc(IOC_READ | IOC_WRITE, type_, nr, size)
}

/// Request structure for `KREAD_IOCTL_READ_RANGE`.
///
/// Matches C definition in `kread_uapi.h`:
/// ```c
/// struct kread_ioctl_range {
///     __s32 target_pid;   /* Target process ID (> 0) */
///     __u32 pad;          /* 8-byte alignment padding */
///     __u64 addr;         /* Target virtual address (TBI untagged in kernel) */
///     __u64 len;          /* Number of bytes to read (<= 64 MiB) */
///     __u64 out_buf;      /* User-space pointer to destination buffer */
///     __u64 out_len;      /* [OUT] Actual bytes transferred */
/// };
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KreadIoctlRange {
    /// Target process ID (> 0)
    pub target_pid: i32,
    /// 8-byte alignment padding
    pub pad: u32,
    /// Target virtual address (ARM64 TBI untagged in kernel)
    pub addr: u64,
    /// Number of bytes to read (<= 64 MiB)
    pub len: u64,
    /// User-space pointer to destination buffer (cast to u64)
    pub out_buf: u64,
    /// [OUT] Actual bytes transferred by kernel streaming engine
    pub out_len: u64,
}

/// Request structure for `KREAD_IOCTL_READ_BATCH`.
///
/// Matches C definition in `kread_uapi.h`:
/// ```c
/// struct kread_ioctl_batch {
///     __s32 target_pid;       /* Target process ID (> 0) */
///     __u32 count;            /* Number of entries (<= 65536) */
///     __u64 addrs_ptr;        /* User-space pointer to array of uint64_t target VAs */
///     __u64 vals_ptr;         /* User-space pointer to array of uint64_t destination values */
///     __u32 success_count;    /* [OUT] Count of resident/non-hole pages read */
///     __u32 pad;              /* 8-byte alignment padding */
/// };
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KreadIoctlBatch {
    /// Target process ID (> 0)
    pub target_pid: i32,
    /// Number of entries (<= 65536)
    pub count: u32,
    /// User-space pointer to array of uint64_t target VAs
    pub addrs_ptr: u64,
    /// User-space pointer to array of uint64_t destination values
    pub vals_ptr: u64,
    /// [OUT] Count of resident / non-hole pages successfully read
    pub success_count: u32,
    /// 8-byte alignment padding
    pub pad: u32,
}

/// Range read ioctl command number (_IOWR('k', 1, struct kread_ioctl_range)).
pub const KREAD_IOCTL_READ_RANGE: u64 =
    _iowr(KREAD_IOCTL_MAGIC, 1, std::mem::size_of::<KreadIoctlRange>());

/// Batch scatter read ioctl command number (_IOWR('k', 2, struct kread_ioctl_batch)).
pub const KREAD_IOCTL_READ_BATCH: u64 =
    _iowr(KREAD_IOCTL_MAGIC, 2, std::mem::size_of::<KreadIoctlBatch>());

/// Maximum supported range read length (64 MiB).
pub const KREAD_MAX_RANGE_LEN: usize = 64 * 1024 * 1024;

/// Maximum supported batch count (65536 entries).
pub const KREAD_MAX_BATCH_COUNT: usize = 65536;

/// Default character device path.
pub const KREAD_DEVICE_PATH: &str = "/dev/kread";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ioctl_range_layout() {
        assert_eq!(std::mem::size_of::<KreadIoctlRange>(), 40);
        assert_eq!(std::mem::align_of::<KreadIoctlRange>(), 8);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, target_pid), 0);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, pad), 4);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, addr), 8);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, len), 16);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, out_buf), 24);
        assert_eq!(std::mem::offset_of!(KreadIoctlRange, out_len), 32);
    }

    #[test]
    fn test_ioctl_batch_layout() {
        assert_eq!(std::mem::size_of::<KreadIoctlBatch>(), 32);
        assert_eq!(std::mem::align_of::<KreadIoctlBatch>(), 8);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, target_pid), 0);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, count), 4);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, addrs_ptr), 8);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, vals_ptr), 16);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, success_count), 24);
        assert_eq!(std::mem::offset_of!(KreadIoctlBatch, pad), 28);
    }

    #[test]
    fn test_ioctl_numbers() {
        assert_eq!(KREAD_IOCTL_READ_RANGE, 0xc0286b01);
        assert_eq!(KREAD_IOCTL_READ_BATCH, 0xc0206b02);
    }
}
