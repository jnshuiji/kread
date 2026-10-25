/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
#ifndef _UAPI_KREAD_H
#define _UAPI_KREAD_H

#include <linux/types.h>
#include <linux/ioctl.h>

#define KREAD_IOCTL_MAGIC 'k'

struct kread_ioctl_range {
	__s32 target_pid;
	__u32 pad;
	__u64 addr;
	__u64 len;
	__u64 out_buf;
	__u64 out_len;
};

struct kread_ioctl_batch {
	__s32 target_pid;
	__u32 count;
	__u64 addrs_ptr;
	__u64 vals_ptr;
	__u32 success_count;
	__u32 pad;
};

#define KREAD_IOCTL_READ_RANGE _IOWR(KREAD_IOCTL_MAGIC, 1, struct kread_ioctl_range)
#define KREAD_IOCTL_READ_BATCH _IOWR(KREAD_IOCTL_MAGIC, 2, struct kread_ioctl_batch)

#endif /* _UAPI_KREAD_H */
