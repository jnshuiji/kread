// SPDX-License-Identifier: GPL-2.0
/*
 * adv_test.c - Adversarial Empirical Stress & Invariant Challenger for kread
 *
 * Specifically challenges:
 *   1. ARM64 TBI Tagged Pointer Handling (0xb4..., 0x8a..., 0xff...) via /dev/kread ioctls
 *   2. THP 2MB boundary traversal & 100% byte-level deterministic consistency
 *   3. Zero-Page-Fault sparse hole filling (strictly 0x00) & batch success_count correctness
 *   4. Heavy 16-thread concurrency stress & anti-exit kill race with zero deadlock
 *   5. Negative testing (kernel address rejection, invalid len, invalid count)
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <pthread.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <sys/time.h>
#include <time.h>

#include "kread_uapi.h"

#define KREAD_DEV_PATH "/dev/kread"
#define THP_SIZE       (2UL * 1024 * 1024)

static inline uint64_t compute_pattern(uint64_t addr, uint64_t index)
{
	return addr ^ (index * 0x9e3779b97f4a7c15ULL) ^ 0xFEEDFACECAFEBABEULL;
}

static double get_time_sec(void)
{
	struct timeval tv;
	gettimeofday(&tv, NULL);
	return (double)tv.tv_sec + (double)tv.tv_usec * 1e-6;
}

/* Global test parameters */
static int g_target_pid = 0;
static uint64_t g_thp_addr = 0;
static uint64_t g_thp_len = 0;
static uint64_t g_sparse_addr = 0;
static uint64_t g_sparse_len = 0;

static int g_failed_checks = 0;

#define ASSERT_TRUE(cond, msg, ...) do { \
	if (!(cond)) { \
		fprintf(stderr, "[-] CHALLENGE FAIL: " msg "\n", ##__VA_ARGS__); \
		g_failed_checks++; \
	} else { \
		printf("  [PASS] " msg "\n", ##__VA_ARGS__); \
	} \
} while (0)

/* Challenge 1: Negative & Boundary Inputs */
static void challenge_negative_inputs(int fd)
{
	printf("\n=== Challenge 1: Negative & Boundary Handling ===\n");

	/* 1. Target PID <= 0 */
	struct kread_ioctl_range r_req;
	char buf[64];
	memset(&r_req, 0, sizeof(r_req));
	r_req.target_pid = -1;
	r_req.addr = g_thp_addr;
	r_req.len = sizeof(buf);
	r_req.out_buf = (uint64_t)(uintptr_t)buf;
	int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "PID <= 0 rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 2. Len == 0 */
	r_req.target_pid = g_target_pid;
	r_req.len = 0;
	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "Len == 0 rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 3. Len > 64MB */
	r_req.len = (64UL * 1024 * 1024) + 1;
	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "Len > 64MB rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 4. Kernel address (untagged >= TASK_SIZE) */
	r_req.addr = 0xffff800012345000ULL;
	r_req.len = 64;
	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "Kernel address rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 5. NULL out_buf */
	r_req.addr = g_thp_addr;
	r_req.out_buf = 0;
	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "NULL out_buf rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 6. Batch count == 0 */
	struct kread_ioctl_batch b_req;
	uint64_t dummy_addr = g_thp_addr;
	uint64_t dummy_val = 0;
	memset(&b_req, 0, sizeof(b_req));
	b_req.target_pid = g_target_pid;
	b_req.count = 0;
	b_req.addrs_ptr = (uint64_t)(uintptr_t)&dummy_addr;
	b_req.vals_ptr = (uint64_t)(uintptr_t)&dummy_val;
	ret = ioctl(fd, KREAD_IOCTL_READ_BATCH, &b_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "Batch count == 0 rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);

	/* 7. Batch count > 65536 */
	b_req.count = 65537;
	ret = ioctl(fd, KREAD_IOCTL_READ_BATCH, &b_req);
	ASSERT_TRUE(ret < 0 && errno == EINVAL, "Batch count > 65536 rejected with -EINVAL (ret=%d, errno=%d)", ret, errno);
}

/* Challenge 2: Tagged Pointer (ARM64 TBI) Verification */
static void challenge_tagged_pointers(int fd)
{
	printf("\n=== Challenge 2: ARM64 Tagged Pointer (TBI) Invariant ===\n");

	uint64_t tags[] = {
		0xB4ULL << 56,  /* Android Scudo MTE / memory tag */
		0x8AULL << 56,  /* High tag */
		0xFFULL << 56,  /* All bits set tag */
		0x01ULL << 56,  /* Low bit tag */
		0x55ULL << 56   /* Alternating tag */
	};
	int num_tags = sizeof(tags) / sizeof(tags[0]);

	/* Read canonical reference data without tags */
	uint64_t ref_val = 0;
	struct kread_ioctl_range r_req;
	memset(&r_req, 0, sizeof(r_req));
	r_req.target_pid = g_target_pid;
	r_req.addr = g_thp_addr;
	r_req.len = sizeof(ref_val);
	r_req.out_buf = (uint64_t)(uintptr_t)&ref_val;

	int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret == 0 && r_req.out_len == sizeof(ref_val), "Untagged range read succeeded (val=0x%lx)", (unsigned long)ref_val);

	/* Test each tag on Range Read */
	for (int i = 0; i < num_tags; i++) {
		uint64_t tagged_addr = g_thp_addr | tags[i];
		uint64_t read_val = 0;
		r_req.addr = tagged_addr;
		r_req.out_buf = (uint64_t)(uintptr_t)&read_val;
		r_req.out_len = 0;

		ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
		ASSERT_TRUE(ret == 0 && r_req.out_len == sizeof(read_val) && read_val == ref_val,
			    "Range read with tagged ptr 0x%016lx succeeded and data matches ref", tagged_addr);
	}

	/* Test tagged addresses on Batch Read */
	uint64_t batch_addrs[8];
	uint64_t batch_vals[8];
	for (int i = 0; i < 5; i++) {
		batch_addrs[i] = g_thp_addr | tags[i];
	}
	batch_addrs[5] = (g_thp_addr + 8) | (0xB4ULL << 56);
	batch_addrs[6] = (g_thp_addr + 16) | (0x8AULL << 56);
	batch_addrs[7] = (g_thp_addr + 24) | (0xFFULL << 56);

	struct kread_ioctl_batch b_req;
	memset(&b_req, 0, sizeof(b_req));
	b_req.target_pid = g_target_pid;
	b_req.count = 8;
	b_req.addrs_ptr = (uint64_t)(uintptr_t)batch_addrs;
	b_req.vals_ptr = (uint64_t)(uintptr_t)batch_vals;

	ret = ioctl(fd, KREAD_IOCTL_READ_BATCH, &b_req);
	ASSERT_TRUE(ret == 0 && b_req.success_count == 8, "Batch read with tagged ptrs returned success_count=8");
	for (int i = 0; i < 5; i++) {
		ASSERT_TRUE(batch_vals[i] == ref_val, "Batch entry %d with tag 0x%02lx matches ref", i, tags[i] >> 56);
	}
}

/* Challenge 3: THP 2MB Consistency Across Huge Page Boundary */
static void challenge_thp_consistency(int fd)
{
	printf("\n=== Challenge 3: THP 2MB Boundary & 100%% Data Consistency ===\n");

	/* We verify the entire 8 MiB THP buffer against compute_pattern */
	size_t check_len = g_thp_len;
	uint8_t *buffer = malloc(check_len);
	ASSERT_TRUE(buffer != NULL, "Allocated verification buffer (%zu bytes)", check_len);

	struct kread_ioctl_range r_req;
	memset(&r_req, 0, sizeof(r_req));
	r_req.target_pid = g_target_pid;
	r_req.addr = g_thp_addr;
	r_req.len = check_len;
	r_req.out_buf = (uint64_t)(uintptr_t)buffer;

	double t0 = get_time_sec();
	int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	double t1 = get_time_sec();

	ASSERT_TRUE(ret == 0 && r_req.out_len == check_len, "Read 8 MiB THP range in %.2f ms (%.2f MB/s)",
		    (t1 - t0) * 1000.0, ((double)check_len / (1024.0 * 1024.0)) / (t1 - t0));

	/* Verify each 64-bit word */
	uint64_t *words = (uint64_t *)buffer;
	size_t num_words = check_len / sizeof(uint64_t);
	size_t mismatches = 0;

	for (size_t i = 0; i < num_words; i++) {
		uint64_t target_vaddr = g_thp_addr + (i * sizeof(uint64_t));
		uint64_t expected = compute_pattern(target_vaddr, i);
		if (words[i] != expected) {
			if (mismatches < 5) {
				fprintf(stderr, "[-] Word mismatch at index %zu (addr 0x%lx): expected 0x%016lx, got 0x%016lx\n",
					i, (unsigned long)target_vaddr, (unsigned long)expected, (unsigned long)words[i]);
			}
			mismatches++;
		}
	}

	double consistency = 100.0 * (double)(num_words - mismatches) / (double)num_words;
	ASSERT_TRUE(mismatches == 0, "THP 2MB Consistency is %.6f%% (%zu / %zu matched, %zu mismatches)",
		    consistency, num_words - mismatches, num_words, mismatches);

	/* Now specifically test straddling the 2MB boundary */
	/* 2MB boundary is at g_thp_addr + 2MB */
	uint64_t boundary_addr = g_thp_addr + THP_SIZE;
	uint64_t straddle_start = boundary_addr - 128;
	uint64_t straddle_buf[32]; /* 256 bytes */
	r_req.addr = straddle_start;
	r_req.len = sizeof(straddle_buf);
	r_req.out_buf = (uint64_t)(uintptr_t)straddle_buf;

	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret == 0 && r_req.out_len == sizeof(straddle_buf), "Straddle read across 2MB PMD boundary succeeded");

	size_t straddle_words = sizeof(straddle_buf) / sizeof(uint64_t);
	size_t straddle_mismatches = 0;
	size_t base_index = (straddle_start - g_thp_addr) / sizeof(uint64_t);
	for (size_t j = 0; j < straddle_words; j++) {
		uint64_t va = straddle_start + j * sizeof(uint64_t);
		uint64_t exp = compute_pattern(va, base_index + j);
		if (straddle_buf[j] != exp)
			straddle_mismatches++;
	}
	ASSERT_TRUE(straddle_mismatches == 0, "Straddle 2MB boundary bytes 100%% consistent (%zu / %zu)",
		    straddle_words - straddle_mismatches, straddle_words);

	free(buffer);
}

/* Challenge 4: Sparse Unmapped Holes & Zero Page Invariant */
static void challenge_sparse_holes(int fd)
{
	printf("\n=== Challenge 4: Sparse Memory Holes & Zero-Fill Invariant ===\n");

	/* Even 4KB pages in sparse_buf are mapped, odd 4KB pages are unmapped holes */
	size_t test_len = 64 * 1024; /* 64 KB = 16 pages */
	uint8_t *buf = malloc(test_len);
	ASSERT_TRUE(buf != NULL, "Allocated sparse test buffer");

	struct kread_ioctl_range r_req;
	memset(&r_req, 0, sizeof(r_req));
	r_req.target_pid = g_target_pid;
	r_req.addr = g_sparse_addr;
	r_req.len = test_len;
	r_req.out_buf = (uint64_t)(uintptr_t)buf;

	int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	ASSERT_TRUE(ret == 0 && r_req.out_len == test_len, "Read 64 KB sparse range across alternating pages");

	/* Check pages: even pages must have non-zero pattern, odd pages must be strictly 0x00 */
	size_t hole_errors = 0;
	size_t mapped_errors = 0;
	for (size_t p = 0; p < 16; p++) {
		uint8_t *page_ptr = buf + (p * 4096);
		bool is_hole = (p % 2 == 1);
		if (is_hole) {
			for (size_t b = 0; b < 4096; b++) {
				if (page_ptr[b] != 0)
					hole_errors++;
			}
		} else {
			uint64_t first_word = *(uint64_t *)page_ptr;
			uint64_t expected = compute_pattern(g_sparse_addr + p * 4096, p * 4096);
			if (first_word != expected)
				mapped_errors++;
		}
	}

	ASSERT_TRUE(hole_errors == 0, "Unmapped hole pages strictly 0x00-filled (0 byte errors out of 32768 hole bytes)");
	ASSERT_TRUE(mapped_errors == 0, "Mapped pages contain correct pattern data (0 page errors out of 8 mapped pages)");

	/* Batch read on truly unmapped addresses (hole between VMAs, e.g. 0x10000000) */
	uint64_t unmapped_addrs[8];
	uint64_t unmapped_vals[8];
	for (int i = 0; i < 8; i++) {
		unmapped_addrs[i] = 0x10000000ULL + (i * 4096);
		unmapped_vals[i] = 0xDEADBEEFCAFEBABEULL;
	}
	struct kread_ioctl_batch b_req;
	memset(&b_req, 0, sizeof(b_req));
	b_req.target_pid = g_target_pid;
	b_req.count = 8;
	b_req.addrs_ptr = (uint64_t)(uintptr_t)unmapped_addrs;
	b_req.vals_ptr = (uint64_t)(uintptr_t)unmapped_vals;

	ret = ioctl(fd, KREAD_IOCTL_READ_BATCH, &b_req);
	ASSERT_TRUE(ret == 0 && b_req.success_count == 0,
		    "Batch read on truly unmapped addresses returned success_count=0 (got %u)", b_req.success_count);
	bool all_zero = true;
	for (int i = 0; i < 8; i++) {
		if (unmapped_vals[i] != 0)
			all_zero = false;
	}
	ASSERT_TRUE(all_zero, "Batch values for unmapped addresses are strictly 0");

	/* Also range read across unmapped hole 0x10000000 */
	uint8_t unmapped_buf[4096];
	memset(unmapped_buf, 0xFF, sizeof(unmapped_buf));
	r_req.addr = 0x10000000ULL;
	r_req.len = sizeof(unmapped_buf);
	r_req.out_buf = (uint64_t)(uintptr_t)unmapped_buf;

	ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &r_req);
	bool unmapped_zero_filled = true;
	for (size_t b = 0; b < sizeof(unmapped_buf); b++) {
		if (unmapped_buf[b] != 0)
			unmapped_zero_filled = false;
	}
	ASSERT_TRUE(ret == 0 && r_req.out_len == sizeof(unmapped_buf) && unmapped_zero_filled,
		    "Range read across unmapped hole returns all 0x00 without page fault");

	free(buf);
}

/* Challenge 5: 16-Thread Concurrency & Anti-Exit Deadlock Stress */
struct thread_arg {
	int thread_id;
	int fd;
	volatile bool *stop;
	unsigned long range_ops;
	unsigned long batch_ops;
};

static void *concur_worker(void *data)
{
	struct thread_arg *arg = (struct thread_arg *)data;
	uint8_t range_buf[16 * 1024];
	uint64_t batch_addrs[32];
	uint64_t batch_vals[32];

	for (int i = 0; i < 32; i++) {
		batch_addrs[i] = g_thp_addr + (i * 64);
	}

	struct kread_ioctl_range r_req;
	memset(&r_req, 0, sizeof(r_req));
	r_req.target_pid = g_target_pid;
	r_req.addr = g_thp_addr;
	r_req.len = sizeof(range_buf);
	r_req.out_buf = (uint64_t)(uintptr_t)range_buf;

	struct kread_ioctl_batch b_req;
	memset(&b_req, 0, sizeof(b_req));
	b_req.target_pid = g_target_pid;
	b_req.count = 32;
	b_req.addrs_ptr = (uint64_t)(uintptr_t)batch_addrs;
	b_req.vals_ptr = (uint64_t)(uintptr_t)batch_vals;

	while (!*arg->stop) {
		if (arg->thread_id % 2 == 0) {
			/* Range read */
			if (ioctl(arg->fd, KREAD_IOCTL_READ_RANGE, &r_req) == 0)
				arg->range_ops++;
		} else {
			/* Batch read */
			if (ioctl(arg->fd, KREAD_IOCTL_READ_BATCH, &b_req) == 0)
				arg->batch_ops++;
		}
	}
	return NULL;
}

static void challenge_concurrency_stress(int fd)
{
	printf("\n=== Challenge 5: 16-Thread Concurrency & Anti-Exit Deadlock Stress ===\n");

	const int num_threads = 16;
	pthread_t threads[num_threads];
	struct thread_arg args[num_threads];
	volatile bool stop = false;

	for (int i = 0; i < num_threads; i++) {
		args[i].thread_id = i;
		args[i].fd = fd;
		args[i].stop = &stop;
		args[i].range_ops = 0;
		args[i].batch_ops = 0;
		pthread_create(&threads[i], NULL, concur_worker, &args[i]);
	}

	/* Concurrently, spawn short-lived processes and hammer them with reads, then kill them */
	printf("  [+] Concurrently testing anti-exit race against terminating processes...\n");
	for (int cycle = 0; cycle < 15; cycle++) {
		pid_t child = fork();
		if (child == 0) {
			void *mem = mmap(NULL, 1024 * 1024, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
			if (mem != MAP_FAILED) {
				memset(mem, 0x42, 1024 * 1024);
			}
			while (1) usleep(1000);
			_exit(0);
		}

		/* Rapidly try reading from this child */
		char cbuf[4096];
		struct kread_ioctl_range cr_req;
		memset(&cr_req, 0, sizeof(cr_req));
		cr_req.target_pid = child;
		cr_req.addr = 0x10000;
		cr_req.len = sizeof(cbuf);
		cr_req.out_buf = (uint64_t)(uintptr_t)cbuf;
		ioctl(fd, KREAD_IOCTL_READ_RANGE, &cr_req);

		/* Terminate child abruptly */
		kill(child, SIGKILL);
		waitpid(child, NULL, 0);

		/* Immediately read dead child PID to challenge kread_is_target_alive & mm handling */
		int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &cr_req);
		(void)ret;
		usleep(20000);
	}

	/* Let threads run for 10 seconds total */
	sleep(10);
	stop = true;

	unsigned long total_range = 0;
	unsigned long total_batch = 0;
	for (int i = 0; i < num_threads; i++) {
		pthread_join(threads[i], NULL);
		total_range += args[i].range_ops;
		total_batch += args[i].batch_ops;
	}

	printf("  Total Range Ops (8 threads): %lu (%.1f ops/s)\n", total_range, (double)total_range / 10.0);
	printf("  Total Batch Ops (8 threads): %lu (%.1f ops/s)\n", total_batch, (double)total_batch / 10.0);
	printf("  Combined Throughput        : %.1f ops/s\n", (double)(total_range + total_batch) / 10.0);

	ASSERT_TRUE((total_range + total_batch) > 100000,
		    "16-thread concurrency achieved %lu total ops (>100k) with ZERO deadlock", total_range + total_batch);
}

int main(int argc, char **argv)
{
	if (argc < 6) {
		fprintf(stderr, "Usage: %s <target_pid> <thp_addr_hex> <thp_len> <sparse_addr_hex> <sparse_len>\n", argv[0]);
		return 1;
	}

	g_target_pid = atoi(argv[1]);
	g_thp_addr = strtoull(argv[2], NULL, 0);
	g_thp_len = strtoull(argv[3], NULL, 0);
	g_sparse_addr = strtoull(argv[4], NULL, 0);
	g_sparse_len = strtoull(argv[5], NULL, 0);

	printf("================================================================================\n");
	printf("       ⚡ kread ADVERSARIAL STRESS & EMPIRICAL CHALLENGE HARNESS\n");
	printf("================================================================================\n");
	printf("Target PID    : %d\n", g_target_pid);
	printf("THP Address   : 0x%lx (Len: %lu)\n", (unsigned long)g_thp_addr, (unsigned long)g_thp_len);
	printf("Sparse Address: 0x%lx (Len: %lu)\n", (unsigned long)g_sparse_addr, (unsigned long)g_sparse_len);

	int fd = open(KREAD_DEV_PATH, O_RDWR);
	if (fd < 0) {
		perror("open " KREAD_DEV_PATH);
		return 1;
	}
	printf("✓ Successfully opened %s (fd=%d)\n", KREAD_DEV_PATH, fd);

	challenge_negative_inputs(fd);
	challenge_tagged_pointers(fd);
	challenge_thp_consistency(fd);
	challenge_sparse_holes(fd);
	challenge_concurrency_stress(fd);

	close(fd);

	printf("\n================================================================================\n");
	printf("Adversarial Verification Complete: %d Failed Checks\n", g_failed_checks);
	printf("================================================================================\n");

	return g_failed_checks == 0 ? 0 : 2;
}
