// SPDX-License-Identifier: GPL-2.0
/*
 * mem_pressure.c - Low memory pressure generator for kread test suite
 *
 * Allocates and touches anonymous memory in 32 MiB chunks until
 * /proc/meminfo MemAvailable drops to or below the requested target (e.g., 120 MB).
 * Used to validate Indicator 6 (O(1) streaming reads under low-memory conditions).
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>

static volatile sig_atomic_t g_stop = 0;

static void on_sig(int s)
{
	(void)s;
	g_stop = 1;
}

static unsigned long get_mem_available_mb(void)
{
	FILE *fp = fopen("/proc/meminfo", "r");
	if (!fp)
		return 0;

	char line[256];
	unsigned long kb = 0;
	while (fgets(line, sizeof(line), fp)) {
		if (sscanf(line, "MemAvailable: %lu kB", &kb) == 1)
			break;
	}
	fclose(fp);
	return kb / 1024;
}

int main(int argc, char **argv)
{
	struct sigaction sa;
	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = on_sig;
	sigaction(SIGTERM, &sa, NULL);
	sigaction(SIGINT, &sa, NULL);

	unsigned long target_avail_mb = (argc >= 2) ? strtoul(argv[1], NULL, 0) : 120;
	printf("[*] Ballooning memory until MemAvailable <= %lu MB...\n", target_avail_mb);

	const size_t chunk_size = 32UL * 1024 * 1024; /* 32 MiB */
	const int max_chunks = 1024;
	void **ptrs = malloc(sizeof(void *) * max_chunks);
	if (!ptrs) {
		perror("[-] malloc ptr array failed");
		return 1;
	}

	int count = 0;
	while (!g_stop && count < max_chunks) {
		unsigned long cur_avail = get_mem_available_mb();
		if (cur_avail <= target_avail_mb)
			break;

		void *p = malloc(chunk_size);
		if (!p) {
			printf("[-] malloc failed at %d MB allocated\n", count * 32);
			break;
		}

		/* Actively touch pages so physical memory is mapped */
		memset(p, 0x5A, chunk_size);
		ptrs[count++] = p;

		printf("  Allocated %d MB (MemAvailable: %lu MB)\n",
		       count * 32, get_mem_available_mb());
	}

	printf("[+] Target memory pressure reached (%lu MB available, %d MB allocated). Holding...\n",
	       get_mem_available_mb(), count * 32);
	fflush(stdout);

	while (!g_stop) {
		sleep(1);
	}

	printf("[*] Cleaning up allocated balloon memory...\n");
	for (int i = 0; i < count; i++) {
		free(ptrs[i]);
	}
	free(ptrs);
	return 0;
}
