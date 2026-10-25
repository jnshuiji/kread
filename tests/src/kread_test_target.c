// SPDX-License-Identifier: GPL-2.0
/*
 * kread_test_target.c - Target test process for kread verification suite
 *
 * Allocates:
 *   1. 8 MiB (4 x 2 MiB) THP memory buffer aligned to 2 MiB boundaries,
 *      marked with madvise(MADV_HUGEPAGE), filled with deterministic hash values.
 *   2. 64 MiB sparse memory mapping with alternating 4 KiB holes and data pages.
 *
 * Exposes layout parameters on stdout for test_runner.sh parsing.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <sys/mman.h>

#define THP_SIZE        (2UL * 1024 * 1024)
#define TOTAL_THP_SIZE  (8UL * 1024 * 1024)
#define SPARSE_SIZE     (64UL * 1024 * 1024)

static volatile sig_atomic_t g_running = 1;

static void sig_handler(int sig)
{
	(void)sig;
	g_running = 0;
}

static inline uint64_t compute_pattern(uint64_t addr, uint64_t index)
{
	return addr ^ (index * 0x9e3779b97f4a7c15ULL) ^ 0xFEEDFACECAFEBABEULL;
}

int main(int argc, char **argv)
{
	(void)argc;
	(void)argv;

	struct sigaction sa;
	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = sig_handler;
	sigaction(SIGTERM, &sa, NULL);
	sigaction(SIGINT, &sa, NULL);

	/* 1. Allocate 2MB-aligned THP memory buffer via mmap */
	size_t alloc_sz = TOTAL_THP_SIZE + THP_SIZE;
	void *raw_mmap = mmap(NULL, alloc_sz, PROT_READ | PROT_WRITE,
			      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (raw_mmap == MAP_FAILED) {
		perror("mmap THP failed");
		return 1;
	}
	uintptr_t aligned_addr = ((uintptr_t)raw_mmap + (THP_SIZE - 1)) & ~(THP_SIZE - 1);
	void *thp_buf = (void *)((aligned_addr) & 0x00ffffffffffffffULL);

	/* Request transparent hugepage backing */
	madvise(thp_buf, TOTAL_THP_SIZE, MADV_HUGEPAGE);

	/* Lock memory to prevent Android kswapd/zRAM from paging it out under memory pressure */
	mlock(thp_buf, TOTAL_THP_SIZE);

	/* Fill THP buffer with verifiable deterministic hash patterns */
	uint64_t *p64 = (uint64_t *)thp_buf;
	size_t count64 = TOTAL_THP_SIZE / sizeof(uint64_t);
	for (size_t i = 0; i < count64; i++) {
		p64[i] = compute_pattern((uint64_t)(p64 + i), (uint64_t)i);
	}

	/* 2. Allocate 64 MiB sparse anonymous mapping */
	void *sparse_buf = mmap(NULL, SPARSE_SIZE, PROT_READ | PROT_WRITE,
				MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (sparse_buf == MAP_FAILED) {
		perror("mmap sparse buffer failed");
		munmap(raw_mmap, alloc_sz);
		return 1;
	}

	/* Write to even 4KB pages only (leave odd pages as unmapped zero holes) */
	for (size_t off = 0; off < SPARSE_SIZE; off += 8192) {
		uint64_t *sp = (uint64_t *)((char *)sparse_buf + off);
		*sp = compute_pattern((uint64_t)sp, off);
	}

	/* Lock sparse buffer so low-memory stress test does not swap it to zRAM */
	mlock(sparse_buf, SPARSE_SIZE);

	/* Output parameters to stdout for test runner scripting */
	printf("TARGET_PID=%d\n", getpid());
	printf("THP_ADDR=0x%lx\n", (unsigned long)thp_buf);
	printf("THP_LEN=%lu\n", TOTAL_THP_SIZE);
	printf("SPARSE_ADDR=0x%lx\n", (unsigned long)sparse_buf);
	printf("SPARSE_LEN=%lu\n", SPARSE_SIZE);
	fflush(stdout);

	/* Wait until test runner signals completion */
	while (g_running) {
		pause();
	}

	munmap(sparse_buf, SPARSE_SIZE);
	munmap(raw_mmap, alloc_sz);
	return 0;
}
