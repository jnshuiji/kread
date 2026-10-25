# kread: Zero-Page-Fault Cross-Process Memory Engine (Android GKI 6.12 / arm64)

## 1. Overview & Architectural Principles

`kread` is a high-performance, production-grade memory engine engineered for Android Generic Kernel Image (GKI) 6.12 on the `arm64` architecture (4 KiB page size). It provides modern user-space profiling, analysis, and debugging engines with high-concurrency, low-latency, strictly zero-page-fault cross-process read capabilities.

### Why Standard Linux Kernel Interfaces Are Unsuitable
Standard Linux interfaces for cross-process memory inspection (`/proc/<pid>/mem`, `process_vm_readv`, `ptrace(PTRACE_PEEKDATA)`) depend internally on `get_user_pages_remote()` or `access_process_vm()`. When referencing unmapped virtual addresses, swapped-out pages, demand-zero allocations, or copy-on-write faults, these interfaces actively trigger kernel page faults on behalf of the target process:
- Increment target `minflt` / `majflt` counters in `/proc/<pid>/stat` (observable by anti-cheat / anti-tamper engines).
- Generate TLB invalidation broadcasts and page table writes.
- Disrupt target process RSS and cache hierarchy.

### The Zero-Page-Fault Invariant
`kread` bypasses `get_user_pages_remote()` entirely. Instead, it implements a read-only software page-table walker executing under `mmap_read_lock_killable()`:
1. **Zero Fault Guarantee**: The module inspects the target process page tables (`pgd` -> `p4d` -> `pud` -> `pmd` -> `pte`). Unmapped virtual address intervals, unallocated intermediate page tables, non-present PTEs, or invalid physical frame numbers (`!pfn_valid`) are treated as unbacked holes and filled with zeroes. No page fault handler (`handle_mm_fault()`) is ever invoked.
2. **Strictly Read-Only**: Target memory and page tables are never modified.
3. **Huge-Page-First Detection**: Arm64 block descriptors (`pud_leaf`, `pmd_leaf`, `pmd_trans_huge`) are verified *prior* to `*_bad()` macros. This prevents valid 2 MiB Transparent Huge Pages (THP) and 1 GiB gigabyte pages from being incorrectly classified as corrupted entries.
4. **Hardware Fault Shielding**: Memory dereferences from the direct physical map (`phys_to_virt()`) are executed strictly within page boundaries using `copy_from_kernel_nofault()`, shielding the kernel from Synchronous External Aborts (SEA) or asynchronous machine check exceptions.
5. **ARM64 TBI Untagging**: Automatically untags pointer tags in top byte (bits 63:56) via `kread_untagged_addr()`.
6. **Standard GKI ABI Compliance**: Built exclusively against exported GKI symbols (`EXPORT_SYMBOL` / `EXPORT_SYMBOL_GPL`).

---

## 2. Modern Unified Architecture (`/dev/kread` ioctl)

```
+-----------------------------------------------------------------------------------+
|                        User-Space Application / Rust Daemon                       |
+-----------------------------------------------------------------------------------+
       |                                                               |
       | ioctl(fd, KREAD_IOCTL_READ_RANGE, &req)                       | ioctl(fd, KREAD_IOCTL_READ_BATCH, &req)
       v                                                               v
+-----------------------------------------------------------------------------------+
|                           /dev/kread (miscdevice, 0666)                           |
| - Stateless, atomic, reentrant system calls (no session locks / no state machine) |
| - Bounded 64 KiB chunked bounce buffer (O(1) kernel memory footprint)             |
| - Page-coalesced scatter batch table walking (1 page table walk per 4 KiB page)   |
| - Cooperative scheduling: mmap_read_lock_killable + chunked cond_resched()        |
+-----------------------------------------------------------------------------------+
                                       |
                                       v
+-----------------------------------------------------------------------------------+
|                  Target Process Address Space (Linux GKI 6.12)                    |
+-----------------------------------------------------------------------------------+
```

### 2.1 Character Device Interface (`/dev/kread`)
- Registered via Linux `miscdevice` (`MISC_DYNAMIC_MINOR`) with mode `0666`.
- Replaces legacy multi-phase procfs text protocols with clean, atomic `ioctl` calls.
- Purely reentrant: no stateful session handles, mailbox fallbacks, or coarse locks.

### 2.2 Chunked Direct Streaming (O(1) Kernel Memory Footprint)
- Range read memory overhead is strictly bounded to a 64 KiB temporary bounce buffer.
- Physical page slices are traversed, mapped, copied to the local bounce buffer, and directly streamed to user space via `copy_to_user()`.
- Unmapped holes are filled directly via `clear_user()`.
- After each chunk, the driver yields the target's `mmap_read_lock`, invokes `cond_resched()`, and tests `fatal_signal_pending()`, guaranteeing zero deadlocks even if the target is terminated (`kill -9`) mid-transfer.

### 2.3 Page-Coalesced Scatter Batch Read
- Accepts up to 65,536 disparate 64-bit virtual addresses in a single ioctl.
- Virtual addresses are untagged (ARM64 TBI) and sorted by 4 KiB page frame base.
- Hardware page tables are walked **only once per unique 4 KiB page frame**, resolving all sample offsets on that page before proceeding.
- Samples straddling 4 KiB page boundaries are handled safely by split dereference.

### 2.4 Stealth & Diagnostic Cleansing
- In release builds, diagnostic logging (`pr_info`) is compiled out via `#ifdef KREAD_DEBUG`.
- Module binaries contain **0** undefined `printk` or `_printk` symbols, ensuring zero footprints in `dmesg`.

---

## 3. UAPI Binary Interface (`include/kread_uapi.h`)

```c
#ifndef _UAPI_KREAD_H
#define _UAPI_KREAD_H

#include <linux/types.h>
#include <linux/ioctl.h>

#define KREAD_IOCTL_MAGIC 'k'

struct kread_ioctl_range {
    __s32 target_pid;   /* Target process ID (> 0) */
    __u32 pad;          /* 8-byte alignment padding */
    __u64 addr;         /* Target virtual address (TBI untagged in kernel) */
    __u64 len;          /* Number of bytes to read (<= 64 MiB) */
    __u64 out_buf;      /* User-space pointer to destination buffer */
    __u64 out_len;      /* [OUT] Actual bytes transferred */
};

struct kread_ioctl_batch {
    __s32 target_pid;       /* Target process ID (> 0) */
    __u32 count;            /* Number of entries (<= 65536) */
    __u64 addrs_ptr;        /* User-space pointer to uint64_t target VAs */
    __u64 vals_ptr;         /* User-space pointer to uint64_t destination values */
    __u32 success_count;    /* [OUT] Count of resident/non-hole pages read */
    __u32 pad;              /* 8-byte alignment padding */
};

#define KREAD_IOCTL_READ_RANGE _IOWR(KREAD_IOCTL_MAGIC, 1, struct kread_ioctl_range)
#define KREAD_IOCTL_READ_BATCH _IOWR(KREAD_IOCTL_MAGIC, 2, struct kread_ioctl_batch)

#endif /* _UAPI_KREAD_H */
```

---

## 4. Unified Directory Layout

```
modules/kread/
├── BUILD.bazel             # Kleaf Bazel target (//modules/kread:kread -> kread.ko)
├── build.sh                # One-click build script (Kernel Module + Rust Daemon + KernelSU zip)
├── module.prop             # KernelSU module metadata (id=kread, name=kread Companion)
├── customize.sh            # KernelSU flash installer environment verification script
├── service.sh              # KernelSU late-boot autostart (insmod + launch daemon)
├── action.sh               # KernelSU App Action status diagnostics panel
├── README.md               # Unified engineering & technical documentation
├── include/
│   └── kread_uapi.h        # Standard UAPI binary header
├── src/
│   └── kread.c             # Kernel driver source (/dev/kread ioctl)
├── daemon/                 # Rust Daemon complete source & project
│   ├── Cargo.toml
│   └── src/                # Safe Rust driver abstraction & practical examples
└── tests/                  # 7-indicator automated verification test suite
    ├── build_tests.sh      # Cross-compiler script with NDK r26 Clang
    ├── test_runner.sh      # Complete automated real-device runner
    └── src/                # C test client, target generator, and memory pressure tool
```

---

## 5. Rust Daemon Companion (`daemon/`)

The companion daemon is written in pure safe Rust with zero external crate dependencies, statically cross-compiled for `aarch64-unknown-linux-musl`:

- **Safe Driver Abstraction (`src/driver.rs`)**:
  - `KreadDriver::open()`: Opens `/dev/kread`.
  - `read_range(pid, addr, len)`: Atomic range read returning `Vec<u8>`.
  - `read_batch(pid, &[addrs])`: Scatter batch read returning `(Vec<u64>, success_count)`.
  - `read_pod<T>(pid, addr)`: Typed POD memory read.
  - `read_string(pid, addr, max_len)`: Null-terminated string reader.
- **CLI Subcommands**:
  - `kread_daemon range --pid <PID> --addr <HEX> --len <BYTES>`: Single-shot range read with hexdump.
  - `kread_daemon batch --pid <PID> --addrs <A1,A2,...>`: Scatter batch read.
  - `kread_daemon bench --pid <PID> --addr <HEX> --len <BYTES> --ops <N>`: Latency & throughput benchmark.
  - `kread_daemon daemon`: Background service monitoring device health.
  - `kread_daemon status`: JSON diagnostic report for KernelSU action panel.

---

## 6. KernelSU Module Packaging

The module is packaged as a standard flashable zip (`out/kread-1.zip`):
- `module.prop`: Declares `id=kread`, `name=kread Companion`, `version=1`.
- `customize.sh`: Validates CPU architecture (`arm64`), checks GKI 6.12 kernel version, sets permissions.
- `service.sh`: Waits for `sys.boot_completed == 1`, loads `kread.ko`, sets `chmod 0666 /dev/kread`, launches `kread_daemon daemon`.
- `action.sh`: KernelSU App Action button showing live driver, character device, and daemon diagnostics.

---

## 7. Build & Verification Guide

### One-Click Build & Package
```bash
cd modules/kread
./build.sh
```
This automatically:
1. Builds `kread.ko` using Kleaf Bazel (`//modules/kread:kread`).
2. Builds `bin/kread_daemon` static ARM64 binary using Cargo musl cross-compiler.
3. Stages and creates flashable zip `out/kread-1.zip`.

### Compiling Test Suite
```bash
cd modules/kread/tests
./build_tests.sh
```

### Real-Device 7-Indicator Test Execution
Deploy and run on device:
```bash
adb root
adb push modules/kread/kread.ko /data/local/tmp/kread_tests/
adb push modules/kread/tests/bin /data/local/tmp/kread_tests/
adb push modules/kread/tests/test_runner.sh /data/local/tmp/kread_tests/
adb push modules/kread/action.sh /data/local/tmp/kread_tests/
adb push modules/kread/daemon/target/aarch64-unknown-linux-musl/release/kread_daemon /data/local/tmp/kread_tests/bin/

adb shell "chmod +x /data/local/tmp/kread_tests/test_runner.sh /data/local/tmp/kread_tests/bin/* /data/local/tmp/kread_tests/action.sh"
adb shell "/data/local/tmp/kread_tests/test_runner.sh"
```

### 7 Acceptance Indicators
1. **Indicator 1 (Zero-Page-Fault Assertion)**: 1,000,000 reads strictly maintain `delta_minflt == 0` and `delta_majflt == 0`.
2. **Indicator 2 (Memory Leak Detection)**: Slab and kmalloc show 0 persistent leaks over 100,000 operations.
3. **Indicator 3 (Clean Unload Stress)**: 100 consecutive `insmod` / `rmmod` cycles with 0 dmesg warnings/oops.
4. **Indicator 4 (Crash Race Handling)**: Abrupt `kill -9` during active 64 MiB stream read results in zero deadlocks or D-state stalls.
5. **Indicator 5 (Concurrency Stress)**: 12 concurrent worker threads sustain high throughput with zero deadlocks.
6. **Indicator 6 (Low Memory Stress)**: 64 MiB streaming read completes under memory pressure (`MemAvailable < 150 MB`).
7. **Indicator 7 (Huge Page THP Consistency)**: 2 MiB-aligned THP memory read achieves 100% byte-for-byte consistency.

---

## 8. Real-World Integration: UE4 Reverse Engineering & Real-Time ESP

`kread` serves as the foundational kernel reading engine powering the **PUBG Mobile (`libUE4.so`) reverse-engineering suite** located at `pubgm-re/ue4` and the production root daemon `cored` at `pubgm/daemon`:

### 8.1 UE4 Runtime Primitives ↔ kread IOCTL Mapping

| Reverse Engineering Target | UE4 Architectural Reference | `kread` Driver Primitive | Performance Impact |
|---|---|---|---|
| **GNames Table Decoding** | Static VA `0x172E2738` (`pubgm-re/ue4/05-gnames.md`) | `KREAD_IOCTL_READ_BATCH` | Decodes 641-chunk pointer tree in < 1 ms |
| **GWorld & Level Hierarchy** | GWorld `0x1771CC38` → Levels (+0x120) (`08-object-graph.md`) | `KREAD_IOCTL_READ_RANGE` | Traverses PersistentLevel and secondary levels |
| **Actor Entity Tracking** | `ULevel::Actors` (+0xA0) (`10-open-items.md`) | `KREAD_IOCTL_READ_RANGE` | Chunked batch read of active actor pointer arrays |
| **High-Frequency Property Polling** | `Health` (0x1060), `TeamID` (0xB78), `Location` (+0x200) | `KREAD_IOCTL_READ_BATCH` | Gathers full-match entity state at 60 Hz |
| **ELF .text & .bss Inspection** | Decryption & reflection scanning (`02-text-encryption.md`) | `KREAD_IOCTL_READ_RANGE` | Streams 14.4 MB .bss global space in O(1) memory |

### 8.2 Architectural Decoupling: Kernel ↔ Daemon ↔ Android Overlay

```
┌───────────────────────────────── Linux Kernel Space (GKI 6.12) ─────────────────────────────────┐
│  /dev/kread (miscdevice) ── Pure 4-Level Page Table Walk (PGD→P4D→PUD→PMD→PTE, 0 Page Faults)    │
└───────────────────────────────────────────────┬─────────────────────────────────────────────────┘
                                                │ ioctl (READ_RANGE / READ_BATCH)
┌────────────────────────────────────── Root Userspace (cored) ───────────────────────────────────┐
│  pubgm/daemon (Rust musl binary)                                                                │
│  - GWorld (0x1771CC38) → Levels (+0x120) → Actors (+0xA0)                                     │
│  - Reads Entity coordinates, health, team ID, and view matrix at 60 Hz                          │
│  - Serializes to fixed-length binary protocol frame (2.4 KB)                                     │
└───────────────────────────────────────────────┬─────────────────────────────────────────────────┘
                                                │ Linux Abstract Unix Domain Socket ("@perfstats")
┌────────────────────────────────── Unprivileged Android App (APK) ────────────────────────────────┐
│  pubgm/android (Kotlin Overlay)                                                                  │
│  - Untrusted app context, zero root permissions, zero game memory offsets                        │
│  - World-to-Screen matrix projection & Canvas 2D/3D hardware-accelerated rendering               │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
```

