// SPDX-License-Identifier: GPL-2.0
/*
 * kread.c - Modern Android GKI 6.12 cross-process zero-pagefault memory reader
 *
 * Implements standard Linux character device (/dev/kread) with atomic ioctl system calls:
 *   - KREAD_IOCTL_READ_RANGE: O(1) 64 KiB chunked direct streaming to user buffer
 *   - KREAD_IOCTL_READ_BATCH: Page-coalesced scatter read directly to user buffer
 *
 * Invariants strictly guaranteed:
 *   1. Zero page fault: pure 4-level page table walk, empty/bad entries filled with zero.
 *   2. Read only: no modification to target memory, 0 GUP, 0 access_process_vm.
 *   3. Huge page leaf before bad check: pud_leaf/pmd_leaf/trans_huge checked before *_bad.
 *   4. pfn_valid verified before dereference; linear direct map copy_from_kernel_nofault.
 *   5. Anti-exit race: mmap_read_lock_killable, per-chunk lock yield, cond_resched, fatal_signal_pending.
 *   6. Stateless atomic ioctls: fully reentrant, zero session locks, concurrent across callers.
 *   7. Stealth: zero dmesg logging in production (#ifdef KREAD_DEBUG).
 *   8. ARM64 TBI: Top-Byte Ignore untagging on all target virtual addresses.
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/uaccess.h>
#include <linux/sched.h>
#include <linux/sched/task.h>
#include <linux/sched/signal.h>
#include <linux/sched/mm.h>
#include <linux/pid.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/vmalloc.h>
#include <linux/rcupdate.h>
#include <linux/pgtable.h>
#include <linux/mmap_lock.h>
#include <linux/sort.h>
#include <linux/byteorder/generic.h>
#include <linux/compat.h>
#include <linux/version.h>
#include <asm/pgtable.h>
#include <asm/page.h>

#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 1, 0)
#define kread_pud_leaf(v) (pud_sect(v) || pud_trans_huge(v))
#define kread_pmd_leaf(v) (pmd_sect(v) || pmd_trans_huge(v))
#else
#define kread_pud_leaf(v) (pud_leaf(v) || pud_trans_huge(v))
#define kread_pmd_leaf(v) (pmd_leaf(v) || pmd_trans_huge(v))
#endif

#if __has_include("kread_uapi.h")
#include "kread_uapi.h"
#else
#include "../include/kread_uapi.h"
#endif

#define KREAD_DEV_NAME              "kread"
#define KREAD_MAX_RANGE_LEN         (64UL * 1024 * 1024)   /* 64 MiB */
#define KREAD_STREAM_CHUNK_SIZE     (64UL * 1024)          /* 64 KiB streaming bounce buffer */
#define KREAD_BATCH_MAX_ENTRIES     65536UL
#define kread_untagged_addr(addr)   ((addr) & 0x00ffffffffffffffULL)

#ifdef KREAD_DEBUG
#define kread_info(fmt, ...) pr_info("kread: " fmt, ##__VA_ARGS__)
#define kread_err(fmt, ...)  pr_err("kread: " fmt, ##__VA_ARGS__)
#else
#define kread_info(fmt, ...) do {} while (0)
#define kread_err(fmt, ...)  do {} while (0)
#endif

/* ------------------------------------------------------------------ */
/* Page Table Traversal Engine                                        */
/* ------------------------------------------------------------------ */

/**
 * kread_walk_page - Translate a 4KB aligned virtual address to direct map address
 * @mm:      Target mm_struct (caller must hold mmap_read_lock)
 * @page_va: 4KB aligned virtual address
 *
 * Invariants strictly evaluated:
 *   - pud_leaf/pud_trans_huge checked before pud_bad
 *   - pmd_leaf/pmd_trans_huge checked before pmd_bad
 *   - pfn_valid verified before pfn_to_page
 *   - Zero page faults, zero write access
 */
static const char *kread_walk_page(struct mm_struct *mm, unsigned long page_va)
{
	pgd_t *pgd, pgdval;
	p4d_t *p4d, p4dval;
	pud_t *pud, pudval;
	pmd_t *pmd, pmdval;
	pte_t *ptep, pte;
	unsigned long pfn;
	struct page *page;

	if (unlikely(page_va >= TASK_SIZE))
		return NULL;

	pgd = pgd_offset(mm, page_va);
	pgdval = READ_ONCE(*pgd);
	if (pgd_none(pgdval) || unlikely(pgd_bad(pgdval)))
		return NULL;

	p4d = p4d_offset(pgd, page_va);
	p4dval = READ_ONCE(*p4d);
	if (p4d_none(p4dval) || unlikely(p4d_bad(p4dval)))
		return NULL;

	pud = pud_offset(p4d, page_va);
	pudval = READ_ONCE(*pud);
	if (pud_none(pudval))
		return NULL;

	if (kread_pud_leaf(pudval)) {
		if (!pud_present(pudval))
			return NULL;
		pfn = pud_pfn(pudval) + ((page_va & ~PUD_MASK) >> PAGE_SHIFT);
		if (!pfn_valid(pfn))
			return NULL;
		page = pfn_to_page(pfn);
		return (const char *)__va(page_to_phys(page));
	}

	if (unlikely(pud_bad(pudval)))
		return NULL;

	pmd = pmd_offset(pud, page_va);
	pmdval = READ_ONCE(*pmd);
	if (pmd_none(pmdval))
		return NULL;

	if (kread_pmd_leaf(pmdval)) {
		if (!pmd_present(pmdval))
			return NULL;
		pfn = pmd_pfn(pmdval) + ((page_va & ~PMD_MASK) >> PAGE_SHIFT);
		if (!pfn_valid(pfn))
			return NULL;
		page = pfn_to_page(pfn);
		return (const char *)__va(page_to_phys(page));
	}

	if (unlikely(pmd_bad(pmdval)))
		return NULL;

	ptep = pte_offset_kernel(pmd, page_va);
	if (!ptep)
		return NULL;

	pte = ptep_get(ptep);
	if (!pte_present(pte) || pte_none(pte))
		return NULL;

	pfn = pte_pfn(pte);
	if (!pfn_valid(pfn))
		return NULL;

	page = pfn_to_page(pfn);
	return (const char *)__va(page_to_phys(page));
}

static struct mm_struct *kread_get_mm(int pid)
{
	struct task_struct *task = NULL;
	struct mm_struct *mm = NULL;

	if (pid > 0) {
		rcu_read_lock();
		task = find_task_by_vpid(pid);
		if (task)
			get_task_struct(task);
		rcu_read_unlock();
	}

	if (!task && pid == 0)
		task = get_task_struct(current);

	if (task) {
		mm = get_task_mm(task);
		put_task_struct(task);
	}
	return mm;
}

static inline bool kread_is_target_alive(int pid)
{
	struct task_struct *task;
	bool alive = false;

	rcu_read_lock();
	task = find_task_by_vpid(pid);
	if (task && !(task->flags & PF_EXITING))
		alive = true;
	rcu_read_unlock();

	return alive;
}

/* ------------------------------------------------------------------ */
/* Range Read IOCTL Handler                                           */
/* ------------------------------------------------------------------ */

static long kread_do_read_range(struct file *file, unsigned long arg)
{
	struct kread_ioctl_range req;
	struct mm_struct *mm;
	char *chunk_buf;
	size_t chunk_alloc_size;
	unsigned long base_addr;
	size_t offset = 0;
	long ret = 0;

	if (copy_from_user(&req, (void __user *)arg, sizeof(req)))
		return -EFAULT;

	if (req.target_pid <= 0 || req.len == 0 || req.len > KREAD_MAX_RANGE_LEN || !req.out_buf)
		return -EINVAL;

	base_addr = (unsigned long)kread_untagged_addr(req.addr);

	/* Check for overflow and boundary against TASK_SIZE */
	if (base_addr >= TASK_SIZE || req.len > TASK_SIZE ||
	    base_addr + req.len > TASK_SIZE || base_addr + req.len < base_addr)
		return -EINVAL;

	/* Verify destination buffer in user space */
	if (!access_ok((const void __user *)(uintptr_t)req.out_buf, req.len))
		return -EFAULT;

	mm = kread_get_mm(req.target_pid);
	if (!mm)
		return -ESRCH;

	/* Ensure requested range does not exceed target mm's task size */
	if (base_addr >= mm->task_size || base_addr + req.len > mm->task_size) {
		mmput(mm);
		return -EINVAL;
	}

	chunk_alloc_size = min_t(size_t, (size_t)req.len, (size_t)KREAD_STREAM_CHUNK_SIZE);
	chunk_buf = kvmalloc(chunk_alloc_size, GFP_KERNEL);
	if (!chunk_buf) {
		mmput(mm);
		return -ENOMEM;
	}

	while (offset < req.len) {
		size_t to_read_this_chunk = min_t(size_t, req.len - offset, (size_t)KREAD_STREAM_CHUNK_SIZE);
		size_t chunk_pos = 0;
		unsigned long cur_vaddr = base_addr + offset;

		if (fatal_signal_pending(current)) {
			ret = offset ? 0 : -ERESTARTSYS;
			break;
		}

		if (!kread_is_target_alive(req.target_pid)) {
			/* Target process died or exiting, terminate gracefully */
			ret = offset ? 0 : -ESRCH;
			break;
		}

		if (mmap_read_lock_killable(mm)) {
			ret = offset ? 0 : -ERESTARTSYS;
			break;
		}

		/* Walk pages and fill bounce buffer */
		while (chunk_pos < to_read_this_chunk) {
			size_t page_off = cur_vaddr & (PAGE_SIZE - 1);
			size_t cur_chunk = min_t(size_t, to_read_this_chunk - chunk_pos,
						 (size_t)PAGE_SIZE - page_off);
			const char *kaddr = kread_walk_page(mm, cur_vaddr & PAGE_MASK);

			if (kaddr && copy_from_kernel_nofault(chunk_buf + chunk_pos,
							      kaddr + page_off, cur_chunk) == 0) {
				/* Valid resident memory copied */
			} else {
				/* Hole / unmapped / zero page */
				memset(chunk_buf + chunk_pos, 0, cur_chunk);
			}

			cur_vaddr += cur_chunk;
			chunk_pos += cur_chunk;
		}

		mmap_read_unlock(mm);

		/* Cooperative yield before copying to user space */
		cond_resched();

		if (copy_to_user((void __user *)(uintptr_t)(req.out_buf + offset),
				 chunk_buf, to_read_this_chunk)) {
			ret = offset ? 0 : -EFAULT;
			break;
		}

		offset += to_read_this_chunk;
	}

	kvfree(chunk_buf);
	mmput(mm);

	if (ret < 0)
		return ret;

	req.out_len = offset;
	if (copy_to_user((void __user *)arg, &req, sizeof(req)))
		return -EFAULT;

	return 0;
}

/* ------------------------------------------------------------------ */
/* Batch Read IOCTL Handler                                           */
/* ------------------------------------------------------------------ */

struct kread_batch_item {
	u64 addr;
	u32 orig_idx;
};

static int batch_item_cmp(const void *a, const void *b)
{
	const struct kread_batch_item *ia = a;
	const struct kread_batch_item *ib = b;

	if (ia->addr < ib->addr)
		return -1;
	if (ia->addr > ib->addr)
		return 1;
	if (ia->orig_idx < ib->orig_idx)
		return -1;
	if (ia->orig_idx > ib->orig_idx)
		return 1;
	return 0;
}

static u64 batch_sample_one(struct mm_struct *mm, unsigned long addr,
			    const char *page_kaddr, bool *hit)
{
	u64 val = 0;
	size_t page_off = addr & (PAGE_SIZE - 1);

	*hit = false;
	if (likely(page_off + sizeof(u64) <= PAGE_SIZE)) {
		if (copy_from_kernel_nofault(&val, page_kaddr + page_off, sizeof(u64)) == 0)
			*hit = true;
		else
			val = 0;
	} else {
		/* Straddle across 4KB page boundary */
		size_t part1 = PAGE_SIZE - page_off;
		size_t part2 = sizeof(u64) - part1;
		char buf[sizeof(u64)] = {0};
		const char *next = kread_walk_page(mm, (addr + part1) & PAGE_MASK);

		if (next && copy_from_kernel_nofault(buf, page_kaddr + page_off, part1) == 0 &&
		    copy_from_kernel_nofault(buf + part1, next, part2) == 0) {
			memcpy(&val, buf, sizeof(u64));
			*hit = true;
		}
	}
	return val;
}

static long kread_do_read_batch(struct file *file, unsigned long arg)
{
	struct kread_ioctl_batch req;
	struct mm_struct *mm;
	struct kread_batch_item *items;
	u64 *user_addrs;
	u64 *vals;
	size_t i;
	u32 success_count = 0;
	long ret = 0;
	bool locked = false;

	if (copy_from_user(&req, (void __user *)arg, sizeof(req)))
		return -EFAULT;

	if (req.target_pid <= 0 || req.count == 0 || req.count > KREAD_BATCH_MAX_ENTRIES ||
	    !req.addrs_ptr || !req.vals_ptr)
		return -EINVAL;

	if (!access_ok((const void __user *)(uintptr_t)req.addrs_ptr, req.count * sizeof(u64)) ||
	    !access_ok((const void __user *)(uintptr_t)req.vals_ptr, req.count * sizeof(u64)))
		return -EFAULT;

	user_addrs = kvmalloc_array(req.count, sizeof(u64), GFP_KERNEL);
	if (!user_addrs)
		return -ENOMEM;

	if (copy_from_user(user_addrs, (const void __user *)(uintptr_t)req.addrs_ptr,
			   req.count * sizeof(u64))) {
		kvfree(user_addrs);
		return -EFAULT;
	}

	items = kvmalloc_array(req.count, sizeof(struct kread_batch_item), GFP_KERNEL);
	if (!items) {
		kvfree(user_addrs);
		return -ENOMEM;
	}

	vals = kvzalloc(req.count * sizeof(u64), GFP_KERNEL);
	if (!vals) {
		kvfree(user_addrs);
		kvfree(items);
		return -ENOMEM;
	}

	for (i = 0; i < req.count; i++) {
		items[i].addr = kread_untagged_addr(le64_to_cpu(user_addrs[i]));
		items[i].orig_idx = (u32)i;
	}
	kvfree(user_addrs);

	/* Sort by address to coalesce lookups on shared 4KB pages */
	sort(items, req.count, sizeof(struct kread_batch_item), batch_item_cmp, NULL);

	mm = kread_get_mm(req.target_pid);
	if (!mm) {
		kvfree(items);
		kvfree(vals);
		return -ESRCH;
	}

	if (mmap_read_lock_killable(mm)) {
		mmput(mm);
		kvfree(items);
		kvfree(vals);
		return -ERESTARTSYS;
	}
	locked = true;

	i = 0;
	while (i < req.count) {
		unsigned long page_va = items[i].addr & PAGE_MASK;
		const char *kaddr = NULL;

		if (fatal_signal_pending(current)) {
			ret = -ERESTARTSYS;
			break;
		}

		if (!kread_is_target_alive(req.target_pid)) {
			ret = -ESRCH;
			break;
		}

		if (likely(items[i].addr < TASK_SIZE && items[i].addr < mm->task_size))
			kaddr = kread_walk_page(mm, page_va);

		/* Process all sorted entries residing on the same 4KB page */
		while (i < req.count && (items[i].addr & PAGE_MASK) == page_va) {
			u64 val = 0;
			bool hit = false;

			if (kaddr)
				val = batch_sample_one(mm, items[i].addr, kaddr, &hit);

			vals[items[i].orig_idx] = cpu_to_le64(val);
			if (hit)
				success_count++;
			i++;
		}

		/* Yield lock periodically (every 1024 entries) to avoid monopolizing mmap_lock */
		if ((i & 0x3FF) == 0 && i < req.count) {
			mmap_read_unlock(mm);
			locked = false;
			cond_resched();

			if (fatal_signal_pending(current)) {
				ret = -ERESTARTSYS;
				break;
			}
			if (!kread_is_target_alive(req.target_pid)) {
				ret = -ESRCH;
				break;
			}
			if (mmap_read_lock_killable(mm)) {
				ret = -ERESTARTSYS;
				break;
			}
			locked = true;
		}
	}

	if (locked)
		mmap_read_unlock(mm);

	mmput(mm);
	kvfree(items);

	if (ret < 0) {
		kvfree(vals);
		return ret;
	}

	if (copy_to_user((void __user *)(uintptr_t)req.vals_ptr, vals,
			 req.count * sizeof(u64))) {
		kvfree(vals);
		return -EFAULT;
	}
	kvfree(vals);

	req.success_count = success_count;
	if (copy_to_user((void __user *)arg, &req, sizeof(req)))
		return -EFAULT;

	return 0;
}

/* ------------------------------------------------------------------ */
/* File Operations & Device Registration                              */
/* ------------------------------------------------------------------ */

static long kread_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	switch (cmd) {
	case KREAD_IOCTL_READ_RANGE:
		return kread_do_read_range(file, arg);
	case KREAD_IOCTL_READ_BATCH:
		return kread_do_read_batch(file, arg);
	default:
		return -ENOTTY;
	}
}

#ifdef CONFIG_COMPAT
static long kread_compat_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	return kread_ioctl(file, cmd, (unsigned long)compat_ptr(arg));
}
#else
#define kread_compat_ioctl NULL
#endif

static int kread_open(struct inode *inode, struct file *file)
{
	return nonseekable_open(inode, file);
}

static int kread_release(struct inode *inode, struct file *file)
{
	return 0;
}

static const struct file_operations kread_fops = {
	.owner          = THIS_MODULE,
	.open           = kread_open,
	.release        = kread_release,
	.unlocked_ioctl = kread_ioctl,
	.compat_ioctl   = kread_compat_ioctl,
	.llseek         = noop_llseek,
};

static struct miscdevice kread_miscdev = {
	.minor = MISC_DYNAMIC_MINOR,
	.name  = KREAD_DEV_NAME,
	.fops  = &kread_fops,
	.mode  = 0666,
};

static int __init kread_init(void)
{
	int ret;

	ret = misc_register(&kread_miscdev);
	if (ret) {
		kread_err("failed to register miscdevice (err=%d)\n", ret);
		return ret;
	}

	kread_info("ready (/dev/%s miscdevice, mode 0666)\n", kread_miscdev.name);
	return 0;
}

static void __exit kread_exit(void)
{
	misc_deregister(&kread_miscdev);
	kread_info("unloaded\n");
}

module_init(kread_init);
module_exit(kread_exit);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("kread: modern zero-pagefault cross-process memory character device");
MODULE_AUTHOR("kread");
