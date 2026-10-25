# kread Real-Device Automated Verification Suite

This directory contains the independent automated verification test suite for the `kread` cross-process memory engine on Android GKI 6.12 (`arm64`).

The suite verifies the raw kernel module `kread.ko` across **all 7 production acceptance indicators** directly on real devices, asserting zero page faults, zero memory leaks, clean unloads, anti-exit race safety, high concurrency, low-memory stability, and THP 2MB data consistency.

---

## Directory Structure

```
modules/kread/tests/
├── build_tests.sh               # Cross-compiles arm64 test binaries via prebuilt NDK Clang
├── test_runner.sh               # Master automated test runner covering all 7 indicators
├── README.md                    # Test execution instructions and indicator pass criteria
├── bin/                         # Compiled static arm64 ELF executables (created by build_tests.sh)
│   ├── kread_test_target        # Target dummy process (THP 2MB + sparse holes + hash patterns)
│   ├── kread_test_client        # Multi-mode test client (zero-fault, thp, concur, stream-read)
│   └── mem_pressure             # Low-memory ballooning tool for Indicator 6
└── src/
    ├── kread_test_target.c      # Source: Dummy target memory layout generator
    ├── kread_test_client.c      # Source: High-concurrency zero-fault verification client
    └── mem_pressure.c           # Source: Physical memory pressure generator
```

---

## 1. Cross-Compilation Guide

Test binaries are built using the prebuilt Android NDK r26 Clang toolchain located at:
`android16-6.12/prebuilts/ndk-r26/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang`

To compile all test binaries:
```bash
cd /home/pochita/project/android/gki/modules/kread/tests
./build_tests.sh
```

All binaries are compiled statically (`-O2 -static -pthread`) to ensure standalone execution on any Android GKI 6.12 device without dynamic library dependencies.

---

## 2. Real-Device Execution Instructions

### Step 1: Push Suite to Android Device
Connect the Android device via ADB and stage the module and test suite:
```bash
adb root
adb remount

# Create staging directory
adb shell "mkdir -p /data/local/tmp/kread_tests"

# Push kread.ko and test suite
adb push /home/pochita/project/android/gki/modules/kread/kread.ko /data/local/tmp/kread_tests/
adb push /home/pochita/project/android/gki/modules/kread/tests/bin /data/local/tmp/kread_tests/
adb push /home/pochita/project/android/gki/modules/kread/tests/test_runner.sh /data/local/tmp/kread_tests/

# Set executable permissions
adb shell "chmod +x /data/local/tmp/kread_tests/test_runner.sh /data/local/tmp/kread_tests/bin/*"
```

### Step 2: Execute Automated Test Runner
```bash
adb shell "/data/local/tmp/kread_tests/test_runner.sh"
```

---

## 3. The 7 Acceptance Indicators & Pass Criteria

| Indicator | Objective | Mechanism & Verification Steps | Success Pass Criteria |
| :--- | :--- | :--- | :--- |
| **Indicator 1: Zero-Page-Fault Assertion** | Validate strictly 0 page faults during reads | Reads `/proc/<pid>/stat` fields 10 (`minflt`) and 12 (`majflt`) before and after **1,000,000 cross-process reads** (scatter batch and range lookups). | `delta_minflt == 0` and `delta_majflt == 0`. Any non-zero delta constitutes an immediate failure. |
| **Indicator 2: Memory Leak Detection** | Validate zero residual kernel memory | Flushes caches (`drop_caches`), measures baseline Slab (`SUnreclaim`), executes 100,000 session open/read/close cycles, queries `/proc/meminfo` and `/sys/kernel/debug/kmemleak`. | Net Slab growth `<= 2048 kB` (OS noise threshold) and 0 unreleased `kread` allocations in `kmemleak`. |
| **Indicator 3: Clean Unload Stress Test** | Validate module registration & proc cleanup | Executes **100 consecutive `insmod` -> node validation -> `rmmod` cycles**, inspecting `dmesg` across the entire sequence. | All 100 cycles return 0. `dmesg` contains strictly 0 Warnings, Oopses, BUGs, or Call traces. |
| **Indicator 4: Target Process Crash Race** | Validate anti-exit race safety | Spawns target, launches continuous 64 MiB streaming reads, abruptly fires `kill -9` during active page table traversal (20 iterations). | Client process terminates gracefully; zero deadlocks, zero hung tasks, zero processes in unkillable D-state. |
| **Indicator 5: Concurrency Stress** | Validate per-session lock isolation | 12 concurrent worker threads (4 range read threads [16 MiB each] + 8 scatter batch threads [128 sample points]) for 15 seconds. | High throughput sustained (`> 0 ops/s`); no thread deadlocks or lock contention stalls. |
| **Indicator 6: Low Memory Stress** | Validate O(1) streaming bounce buffer | Memory ballooning tool consumes RAM until `MemAvailable < 150 MB`; performs 64 MiB range read. | Read completes full transfer; **zero `kvzalloc` allocation failures**; does not trigger kernel OOM Killer. |
| **Indicator 7: Huge Page THP Consistency** | Validate 2 MiB PMD leaf traversal | Allocates 8 MiB with `madvise(MADV_HUGEPAGE)`, populates deterministic 64-bit hash values, reads full range and samples 2MB boundary points. | Read back data matches pattern **100% byte-for-byte** with 0 corrupted words. |

---

## 4. Manual / Standalone Invocation of Test Binaries

Individual test components can be executed manually for focused debugging:

```bash
# 1. Start target process in background
./bin/kread_test_target &
TARGET_PID=$!

# 2. Run standalone zero-fault test
./bin/kread_test_client zero-fault $TARGET_PID 0x7f00000000

# 3. Run standalone THP consistency check (8MB)
./bin/kread_test_client thp $TARGET_PID 0x7f00000000 8388608

# 4. Run standalone 12-thread concurrency stress test (30s)
./bin/kread_test_client concur $TARGET_PID 0x7f00000000 30

# 5. Run standalone 64 MiB stream read
./bin/kread_test_client stream-read $TARGET_PID 0x7f00000000 67108864

# 6. Run standalone low memory ballooning (target 100 MB available)
./bin/mem_pressure 100
```
