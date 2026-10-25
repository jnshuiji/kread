// SPDX-License-Identifier: GPL-2.0
/*
 * kread_test_client.c - Multi-mode verification client for kread test suite (/dev/kread ioctl)
 *
 * Supported modes:
 *   1. zero-fault   : Asserts delta_minflt == 0 && delta_majflt == 0 over 1,000,000 reads via KREAD_IOCTL_READ_BATCH
 *   2. thp          : Verifies 100% byte-for-byte consistency on THP 2MB allocations via KREAD_IOCTL_READ_RANGE
 *   3. concur       : 12-thread concurrency stress test (4 range + 8 batch)
 *   4. stream-read  : High-volume direct chunk streaming test (e.g., 64 MiB) via KREAD_IOCTL_READ_RANGE
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <pthread.h>
#include <time.h>
#include <errno.h>
#include <sys/time.h>
#include <sys/ioctl.h>
#include "kread_uapi.h"

#define DEV_KREAD         "/dev/kread"
#define CHUNK_BUFFER_SIZE (64 * 1024)

static inline uint64_t get_time_us(void)
{
	struct timeval tv;
	gettimeofday(&tv, NULL);
	return (uint64_t)tv.tv_sec * 1000000ULL + tv.tv_usec;
}

static inline uint64_t compute_pattern(uint64_t addr, uint64_t index)
{
	return addr ^ (index * 0x9e3779b97f4a7c15ULL) ^ 0xFEEDFACECAFEBABEULL;
}

static int get_proc_page_faults(int pid, unsigned long *minflt, unsigned long *majflt)
{
	char path[64];
	snprintf(path, sizeof(path), "/proc/%d/stat", pid);
	FILE *fp = fopen(path, "r");
	if (!fp)
		return -1;

	char buf[1024];
	if (!fgets(buf, sizeof(buf), fp)) {
		fclose(fp);
		return -1;
	}
	fclose(fp);

	/* Locate last closing parenthesis to skip executable name */
	char *p = strrchr(buf, ')');
	if (!p)
		return -1;
	p++;

	char state;
	int ppid, pgrp, session, tty_nr, tpgid;
	unsigned int flags;
	unsigned long minf, cminf, majf;
	int matched = sscanf(p, " %c %d %d %d %d %d %u %lu %lu %lu",
			     &state, &ppid, &pgrp, &session, &tty_nr, &tpgid,
			     &flags, &minf, &cminf, &majf);
	if (matched >= 10) {
		*minflt = minf;
		*majflt = majf;
		return 0;
	}
	return -1;
}

/* ------------------------------------------------------------------ */
/* Indicator 1: Zero-Page-Fault Assertion (1,000,000 Reads)           */
/* ------------------------------------------------------------------ */

static int run_zero_pagefault_test(int target_pid, unsigned long addr)
{
	printf("[*] Starting Zero-Page-Fault assertion test (1,000,000 reads via /dev/kread ioctl)...\n");
	unsigned long minflt_before = 0, majflt_before = 0;
	unsigned long minflt_after = 0, majflt_after = 0;

	if (get_proc_page_faults(target_pid, &minflt_before, &majflt_before) != 0) {
		fprintf(stderr, "[-] Failed to read initial page fault stats for PID %d\n", target_pid);
		return 1;
	}

	int fd = open(DEV_KREAD, O_RDWR);
	if (fd < 0) {
		perror("[-] Failed to open /dev/kread");
		return 1;
	}

	/* Setup 64 addresses per batch */
	const int batch_count = 64;
	const int total_ops = 1000000;
	const int iterations = total_ops / batch_count; /* 15625 iterations * 64 = 1,000,000 */

	uint64_t req_addrs[64];
	for (int i = 0; i < batch_count; i++) {
		req_addrs[i] = addr + (i * 8);
	}
	uint64_t resp_vals[64];

	struct kread_ioctl_batch req = {
		.target_pid = target_pid,
		.count = batch_count,
		.addrs_ptr = (uint64_t)(uintptr_t)req_addrs,
		.vals_ptr = (uint64_t)(uintptr_t)resp_vals,
		.success_count = 0,
		.pad = 0,
	};

	uint64_t t0 = get_time_us();
	for (int iter = 0; iter < iterations; iter++) {
		if (ioctl(fd, KREAD_IOCTL_READ_BATCH, &req) < 0) {
			perror("[-] Batch ioctl failed");
			close(fd);
			return 1;
		}
	}
	close(fd);
	uint64_t t1 = get_time_us();

	if (get_proc_page_faults(target_pid, &minflt_after, &majflt_after) != 0) {
		fprintf(stderr, "[-] Failed to read post-test page fault stats\n");
		return 1;
	}

	unsigned long delta_min = minflt_after - minflt_before;
	unsigned long delta_maj = majflt_after - majflt_before;
	double elapsed_sec = (double)(t1 - t0) / 1000000.0;
	double ops_sec = (double)total_ops / (elapsed_sec > 0.0 ? elapsed_sec : 0.001);

	printf("  Target PID: %d\n", target_pid);
	printf("  Completed reads: %d in %.2f ms (%.0f reads/sec)\n",
	       total_ops, (t1 - t0) / 1000.0, ops_sec);
	printf("  minflt: before=%lu, after=%lu, delta=%lu\n",
	       minflt_before, minflt_after, delta_min);
	printf("  majflt: before=%lu, after=%lu, delta=%lu\n",
	       majflt_before, majflt_after, delta_maj);

	if (delta_min == 0 && delta_maj == 0) {
		printf("[+] PASS: Zero-Page-Fault Invariant strictly held (0 faults)!\n");
		return 0;
	}

	printf("[-] FAIL: Page faults detected on target process during read!\n");
	return 1;
}

/* ------------------------------------------------------------------ */
/* Indicator 7: THP 2MB Huge Page Data Consistency Verification       */
/* ------------------------------------------------------------------ */

static int run_thp_consistency_test(int target_pid, unsigned long thp_addr, size_t thp_len)
{
	printf("[*] Starting THP Consistency Test (Target PID %d, Addr 0x%lx, Len %zu bytes)...\n",
	       target_pid, thp_addr, thp_len);

	int fd = open(DEV_KREAD, O_RDWR);
	if (fd < 0) {
		perror("[-] Failed to open /dev/kread");
		return 1;
	}

	char *buf = malloc(thp_len);
	if (!buf) {
		perror("[-] malloc buffer failed");
		close(fd);
		return 1;
	}
	memset(buf, 0, thp_len);

	struct kread_ioctl_range req = {
		.target_pid = target_pid,
		.pad = 0,
		.addr = thp_addr,
		.len = thp_len,
		.out_buf = (uint64_t)(uintptr_t)buf,
		.out_len = 0,
	};

	if (ioctl(fd, KREAD_IOCTL_READ_RANGE, &req) < 0) {
		perror("[-] Range ioctl failed");
		free(buf);
		close(fd);
		return 1;
	}

	size_t total_read = (size_t)req.out_len;
	printf("  Read returned: %zu / %zu bytes\n", total_read, thp_len);
	if (total_read != thp_len) {
		printf("[-] FAIL: Incomplete read for THP buffer (expected %zu, got %zu)\n",
		       thp_len, total_read);
		free(buf);
		close(fd);
		return 1;
	}

	/* Verify data consistency against deterministic generator pattern */
	uint64_t *p64 = (uint64_t *)buf;
	size_t count = thp_len / sizeof(uint64_t);
	size_t mismatches = 0;

	for (size_t i = 0; i < count; i++) {
		uint64_t cur_addr = thp_addr + (i * sizeof(uint64_t));
		uint64_t expected = compute_pattern(cur_addr, (uint64_t)i);
		if (p64[i] != expected) {
			if (mismatches < 5) {
				printf("  Mismatch at idx %zu (addr 0x%lx): got 0x%016lx, expected 0x%016lx\n",
				       i, cur_addr, (unsigned long)p64[i], (unsigned long)expected);
			}
			mismatches++;
		}
	}
	free(buf);

	/* Boundary sample test across 2MB boundary via KREAD_IOCTL_READ_BATCH */
	uint64_t boundary_addr = thp_addr + (2UL * 1024 * 1024);
	uint64_t b_addrs[4] = {
		boundary_addr - 16,
		boundary_addr - 8,
		boundary_addr,
		boundary_addr + 8
	};
	uint64_t b_vals[4] = {0};

	struct kread_ioctl_batch b_req = {
		.target_pid = target_pid,
		.count = 4,
		.addrs_ptr = (uint64_t)(uintptr_t)b_addrs,
		.vals_ptr = (uint64_t)(uintptr_t)b_vals,
		.success_count = 0,
		.pad = 0,
	};

	if (ioctl(fd, KREAD_IOCTL_READ_BATCH, &b_req) < 0) {
		perror("[-] Batch boundary ioctl failed");
		mismatches++;
	} else {
		for (int k = 0; k < 4; k++) {
			uint64_t idx = (b_addrs[k] - thp_addr) / sizeof(uint64_t);
			uint64_t exp = compute_pattern(b_addrs[k], idx);
			if (b_vals[k] != exp) {
				printf("  Batch boundary mismatch at 0x%lx: got 0x%016lx, expected 0x%016lx\n",
				       (unsigned long)b_addrs[k], (unsigned long)b_vals[k], (unsigned long)exp);
				mismatches++;
			}
		}
	}
	close(fd);

	if (mismatches == 0) {
		printf("[+] PASS: THP 2MB Leaf data 100%% byte-for-byte consistent!\n");
		return 0;
	}

	printf("[-] FAIL: THP data corruption detected (%zu total mismatches)\n", mismatches);
	return 1;
}

/* ------------------------------------------------------------------ */
/* Indicator 5: 12-Thread Concurrency Stress Test                     */
/* ------------------------------------------------------------------ */

struct thread_arg {
	int id;
	int target_pid;
	unsigned long addr;
	size_t len;
	volatile int *stop;
	unsigned long ops;
	unsigned long max_latency_us;
};

static void *range_reader_thread(void *p)
{
	struct thread_arg *arg = (struct thread_arg *)p;
	char *buf = malloc(arg->len);
	if (!buf)
		return NULL;

	int fd = open(DEV_KREAD, O_RDWR);
	if (fd < 0) {
		free(buf);
		return NULL;
	}

	while (!*(arg->stop)) {
		struct kread_ioctl_range req = {
			.target_pid = arg->target_pid,
			.pad = 0,
			.addr = arg->addr,
			.len = arg->len,
			.out_buf = (uint64_t)(uintptr_t)buf,
			.out_len = 0,
		};
		uint64_t t0 = get_time_us();
		int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &req);
		uint64_t lat = get_time_us() - t0;
		if (ret == 0 && req.out_len > 0) {
			if (lat > arg->max_latency_us)
				arg->max_latency_us = lat;
			arg->ops++;
		}
	}
	close(fd);
	free(buf);
	return NULL;
}

static void *batch_reader_thread(void *p)
{
	struct thread_arg *arg = (struct thread_arg *)p;
	uint64_t addrs[128];
	uint64_t vals[128];
	for (int i = 0; i < 128; i++) {
		addrs[i] = arg->addr + (i * 8);
	}

	int fd = open(DEV_KREAD, O_RDWR);
	if (fd < 0)
		return NULL;

	while (!*(arg->stop)) {
		struct kread_ioctl_batch req = {
			.target_pid = arg->target_pid,
			.count = 128,
			.addrs_ptr = (uint64_t)(uintptr_t)addrs,
			.vals_ptr = (uint64_t)(uintptr_t)vals,
			.success_count = 0,
			.pad = 0,
		};
		uint64_t t0 = get_time_us();
		int ret = ioctl(fd, KREAD_IOCTL_READ_BATCH, &req);
		uint64_t lat = get_time_us() - t0;
		if (ret == 0) {
			if (lat > arg->max_latency_us)
				arg->max_latency_us = lat;
			arg->ops++;
		}
	}
	close(fd);
	return NULL;
}

static int run_concurrency_test(int target_pid, unsigned long addr, int duration_sec)
{
	printf("[*] Starting Concurrency Stress Test (12 threads: 4 range + 8 batch, %d seconds)...\n",
	       duration_sec);

	volatile int stop = 0;
	pthread_t th[12];
	struct thread_arg args[12];

	/* 4 Range Reader threads (64KB chunks across range) */
	for (int i = 0; i < 4; i++) {
		args[i].id = i;
		args[i].target_pid = target_pid;
		args[i].addr = addr;
		args[i].len = 64 * 1024;
		args[i].stop = &stop;
		args[i].ops = 0;
		args[i].max_latency_us = 0;
		pthread_create(&th[i], NULL, range_reader_thread, &args[i]);
	}

	/* 8 Scatter Batch Reader threads */
	for (int i = 4; i < 12; i++) {
		args[i].id = i;
		args[i].target_pid = target_pid;
		args[i].addr = addr;
		args[i].len = 0;
		args[i].stop = &stop;
		args[i].ops = 0;
		args[i].max_latency_us = 0;
		pthread_create(&th[i], NULL, batch_reader_thread, &args[i]);
	}

	sleep((unsigned int)duration_sec);
	stop = 1;

	unsigned long total_range_ops = 0, total_batch_ops = 0;
	unsigned long max_lat_range = 0, max_lat_batch = 0;

	for (int i = 0; i < 4; i++) {
		pthread_join(th[i], NULL);
		total_range_ops += args[i].ops;
		if (args[i].max_latency_us > max_lat_range)
			max_lat_range = args[i].max_latency_us;
	}
	for (int i = 4; i < 12; i++) {
		pthread_join(th[i], NULL);
		total_batch_ops += args[i].ops;
		if (args[i].max_latency_us > max_lat_batch)
			max_lat_batch = args[i].max_latency_us;
	}

	printf("  Concurrent Range Reads : %lu ops (%.1f ops/s, max latency: %.2f ms)\n",
	       total_range_ops, (double)total_range_ops / duration_sec, max_lat_range / 1000.0);
	printf("  Concurrent Batch Reads : %lu ops (%.1f ops/s, max latency: %.2f ms)\n",
	       total_batch_ops, (double)total_batch_ops / duration_sec, max_lat_batch / 1000.0);

	if (total_range_ops > 0 && total_batch_ops > 0) {
		printf("[+] PASS: 12-thread concurrency sustained without deadlock or stall!\n");
		return 0;
	}

	printf("[-] FAIL: Concurrency stress stalled or produced zero operations!\n");
	return 1;
}

/* ------------------------------------------------------------------ */
/* Direct Stream Read (Used in Indicator 4 & 6)                       */
/* ------------------------------------------------------------------ */

static int run_stream_read(int target_pid, unsigned long addr, size_t len)
{
	int fd = open(DEV_KREAD, O_RDWR);
	if (fd < 0) {
		perror("[-] Failed to open /dev/kread");
		return 1;
	}

	char *buf = malloc(len);
	if (!buf) {
		perror("[-] malloc buffer failed");
		close(fd);
		return 1;
	}

	struct kread_ioctl_range req = {
		.target_pid = target_pid,
		.pad = 0,
		.addr = addr,
		.len = len,
		.out_buf = (uint64_t)(uintptr_t)buf,
		.out_len = 0,
	};

	int ret = ioctl(fd, KREAD_IOCTL_READ_RANGE, &req);
	if (ret < 0) {
		perror("[-] Range ioctl failed");
		free(buf);
		close(fd);
		return 1;
	}

	printf("[+] Stream read finished: %llu / %zu bytes transferred\n",
	       (unsigned long long)req.out_len, len);
	free(buf);
	close(fd);
	return (req.out_len > 0) ? 0 : 1;
}

/* ------------------------------------------------------------------ */
/* Entry Point                                                        */
/* ------------------------------------------------------------------ */

int main(int argc, char **argv)
{
	if (argc < 3) {
		fprintf(stderr, "Usage:\n");
		fprintf(stderr, "  %s zero-fault <pid> <addr>\n", argv[0]);
		fprintf(stderr, "  %s thp <pid> <addr> [len]\n", argv[0]);
		fprintf(stderr, "  %s concur <pid> <addr> [duration_sec]\n", argv[0]);
		fprintf(stderr, "  %s stream-read <pid> <addr> [len]\n", argv[0]);
		return 1;
	}

	const char *mode = argv[1];
	int pid = atoi(argv[2]);
	unsigned long addr = (argc >= 4) ? strtoul(argv[3], NULL, 0) : 0;
	addr = addr & 0x00ffffffffffffffULL;

	if (strcmp(mode, "zero-fault") == 0) {
		return run_zero_pagefault_test(pid, addr);
	} else if (strcmp(mode, "thp") == 0) {
		size_t len = (argc >= 5) ? strtoul(argv[4], NULL, 0) : (8UL * 1024 * 1024);
		return run_thp_consistency_test(pid, addr, len);
	} else if (strcmp(mode, "concur") == 0) {
		int duration = (argc >= 5) ? atoi(argv[4]) : 10;
		return run_concurrency_test(pid, addr, duration);
	} else if (strcmp(mode, "stream-read") == 0) {
		size_t len = (argc >= 5) ? strtoul(argv[4], NULL, 0) : (64UL * 1024 * 1024);
		return run_stream_read(pid, addr, len);
	}

	fprintf(stderr, "Unknown mode: %s\n", mode);
	return 1;
}
