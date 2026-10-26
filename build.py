#!/usr/bin/env python3
"""
build.py - Python unified build and packaging engine for kread companion module.
Supports:
  - Linux 5.10 (Android 12 GKI) via Android Clang-r416183b & Kbuild
  - Linux 6.12 (Android 16 GKI) via Kleaf Bazel DDK
  - Rust Daemon cross-compilation (aarch64-unknown-linux-musl)
  - Universal Multi-KMI Fat ZIP packaging (auto-detecting 5.10 / 6.12 at runtime)
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import time
import zipfile
from pathlib import Path
from typing import Dict, List, Optional

try:
    from rich.console import Console
    from rich.panel import Panel
    from rich.table import Table

    console = Console()
except ImportError:
    # Minimal fallback if rich is not present
    class SimpleConsole:
        def print(self, *args, **kwargs):
            text = " ".join(str(a) for a in args)
            import re
            clean = re.sub(r"\[/?\w+[^\]]*\]", "", text)
            print(clean)

    console = SimpleConsole()


def compute_sha256(file_path: Path) -> str:
    sha = hashlib.sha256()
    with open(file_path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            sha.update(chunk)
    return sha.hexdigest()


def format_bytes(size: int) -> str:
    for unit in ["B", "KB", "MB", "GB"]:
        if size < 1024.0:
            return f"{size:.1f} {unit}"
        size /= 1024.0
    return f"{size:.1f} TB"


class KreadBuilder:
    def __init__(self, workspace_root: Optional[Path] = None):
        self.module_dir = Path(__file__).resolve().parent
        self.workspace_root = workspace_root or self.module_dir.parent.parent
        self.k510_root = self.workspace_root / "android12-5.10"
        self.k612_root = self.workspace_root / "android16-6.12"
        self.out_dir = self.module_dir / "out"
        self.staging_dir = self.out_dir / "staging"
        self.version = self._get_version()

    def _get_version(self) -> str:
        prop_file = self.module_dir / "module.prop"
        if prop_file.exists():
            for line in prop_file.read_text(encoding="utf-8").splitlines():
                if line.startswith("version="):
                    return line.split("=", 1)[1].strip()
        return "1.0.0"

    def _clean_source_tree_legacy_files(self):
        """Guard: Ensure source tree (module root & src/) is 100% clean of build caches."""
        root_patterns = [
            "*.o", "*.mod", "*.mod.c", "*.mod.o", "*.lto.o", "*.cmd",
            ".*.cmd", "Module.symvers", ".Module.symvers*", "modules.order", ".modules.order*",
            "kread.ko", "kread_5.10.ko", "kread_6.12.ko"
        ]
        for pat in root_patterns:
            for p in self.module_dir.glob(pat):
                try:
                    p.unlink()
                except Exception:
                    pass

        thinlto_dir = self.module_dir / ".thinlto-cache"
        if thinlto_dir.exists():
            shutil.rmtree(thinlto_dir, ignore_errors=True)

        pycache_dir = self.module_dir / "__pycache__"
        if pycache_dir.exists():
            shutil.rmtree(pycache_dir, ignore_errors=True)

        src_dir = self.module_dir / "src"
        if src_dir.exists():
            for pat in ["*.o", "*.cmd", ".*.cmd"]:
                for p in src_dir.glob(pat):
                    try:
                        p.unlink()
                    except Exception:
                        pass

    def _setup_shadow_build_510(self) -> Path:
        """Create and populate isolated shadow build directory under out/build_510."""
        bld = self.out_dir / "build_510"
        bld.mkdir(parents=True, exist_ok=True)
        (bld / "src").mkdir(parents=True, exist_ok=True)

        # Symlink Kbuild, Makefile, include
        for f in ["Kbuild", "Makefile"]:
            link = bld / f
            target = self.module_dir / f
            if not link.exists():
                link.symlink_to(target)

        include_link = bld / "include"
        if not include_link.exists():
            include_link.symlink_to(self.module_dir / "include")

        # Symlink source files in src/
        src_dir = self.module_dir / "src"
        if src_dir.exists():
            for src_file in src_dir.glob("*.c"):
                link = bld / "src" / src_file.name
                if not link.exists():
                    link.symlink_to(src_file)

        return bld

    def build_ko_510(self) -> Optional[Path]:
        """Build kread_5.10.ko using android12-5.10 toolchain and isolated shadow out/build_510."""
        console.print("[bold cyan]🔨 [1/3] Building kread.ko for Android 12 (Linux 5.10 GKI)...[/bold cyan]")
        kdir = self.k510_root / "out" / "android12-5.10" / "common"
        if not kdir.exists():
            console.print(f"[bold red]❌ Linux 5.10 build tree not found at {kdir}. Please build android12-5.10 first.[/bold red]")
            return None

        # 1. Enforce clean source tree guard
        self._clean_source_tree_legacy_files()

        # 2. Setup shadow build directory
        shadow_dir = self._setup_shadow_build_510()

        clang_bin = self.k510_root / "prebuilts-master" / "clang" / "host" / "linux-x86" / "clang-r416183b" / "bin"
        build_tools_bin = self.k510_root / "build" / "build-tools" / "path" / "linux-x86"
        gas_bin = self.k510_root / "prebuilts" / "gas" / "linux-x86"

        custom_path = f"{build_tools_bin}:{clang_bin}:{gas_bin}:{os.environ.get('PATH', '')}"
        env = os.environ.copy()
        env["PATH"] = custom_path

        cmd = [
            "make",
            "-C", str(kdir),
            f"M={shadow_dir}",
            "ARCH=arm64",
            "CROSS_COMPILE=aarch64-linux-gnu-",
            "LLVM=1",
            "LLVM_IAS=1",
            "modules",
        ]

        start_time = time.time()
        res = subprocess.run(cmd, cwd=shadow_dir, env=env, capture_output=True, text=True)
        duration = time.time() - start_time

        ko_file = shadow_dir / "kread.ko"
        if res.returncode == 0 and ko_file.exists():
            self.out_dir.mkdir(parents=True, exist_ok=True)
            dest_510 = self.out_dir / "kread_5.10.ko"
            shutil.copy2(ko_file, dest_510)
            size_str = format_bytes(dest_510.stat().st_size)
            console.print(f"  [bold green]✓ Built kread_5.10.ko ({size_str}) in {duration:.1f}s -> {dest_510}[/bold green]")
            return dest_510
        else:
            console.print(f"[bold red]❌ Build 5.10 failed ({res.returncode}):[/bold red]")
            console.print(res.stderr or res.stdout, markup=False)
            return None

    def build_ko_612(self) -> Optional[Path]:
        """Build kread_6.12.ko using android16-6.12 Kleaf Bazel."""
        console.print("[bold cyan]🔨 [2/3] Building kread.ko for Android 16 (Linux 6.12 GKI via Kleaf)...[/bold cyan]")
        if not self.k612_root.exists():
            console.print(f"[bold red]❌ Kleaf workspace not found at {self.k612_root}.[/bold red]")
            return None

        cmd = ["tools/bazel", "build", "//modules/kread:kread"]
        start_time = time.time()
        res = subprocess.run(cmd, cwd=self.k612_root, capture_output=True, text=True)
        duration = time.time() - start_time

        bazel_out = self.k612_root / "bazel-bin" / "modules" / "kread" / "kread" / "kread.ko"
        if res.returncode == 0 and bazel_out.exists():
            self.out_dir.mkdir(parents=True, exist_ok=True)
            dest_612 = self.out_dir / "kread_6.12.ko"
            shutil.copy2(bazel_out, dest_612)
            size_str = format_bytes(dest_612.stat().st_size)
            console.print(f"  [bold green]✓ Built kread_6.12.ko ({size_str}) in {duration:.1f}s -> {dest_612}[/bold green]")
            return dest_612
        else:
            console.print(f"[bold red]❌ Build 6.12 failed ({res.returncode}):[/bold red]")
            console.print(res.stderr or res.stdout, markup=False)
            return None

    def build_daemon(self) -> Optional[Path]:
        """Compile Rust daemon statically for aarch64-unknown-linux-musl."""
        console.print("[bold cyan]🦀 [3/3] Compiling static ARM64 Rust Daemon (kread_daemon)...[/bold cyan]")
        daemon_dir = self.module_dir / "daemon"
        if not daemon_dir.exists():
            console.print(f"[bold red]❌ Daemon directory {daemon_dir} not found.[/bold red]")
            return None

        cmd = ["cargo", "build", "--release", "--target", "aarch64-unknown-linux-musl"]
        start_time = time.time()
        res = subprocess.run(cmd, cwd=daemon_dir, capture_output=True, text=True)
        duration = time.time() - start_time

        daemon_bin = daemon_dir / "target" / "aarch64-unknown-linux-musl" / "release" / "kread_daemon"
        if res.returncode == 0 and daemon_bin.exists():
            size_str = format_bytes(daemon_bin.stat().st_size)
            console.print(f"  [bold green]✓ Built static kread_daemon ({size_str}) in {duration:.1f}s[/bold green]")
            return daemon_bin
        else:
            console.print(f"[bold red]❌ Cargo build failed ({res.returncode}):[/bold red]")
            console.print(res.stderr or res.stdout, markup=False)
            return None

    def package(
        self,
        target: str = "all",
        ko_510: Optional[Path] = None,
        ko_612: Optional[Path] = None,
        daemon_bin: Optional[Path] = None,
        extra_out_dir: Optional[Path] = None,
    ) -> Optional[Path]:
        """Assemble staging directory and create flashable KernelSU / Magisk ZIP."""
        console.print(f"\n[bold cyan]📦 Packaging kread Companion Module (Target: {target})...[/bold cyan]")
        self.out_dir.mkdir(parents=True, exist_ok=True)
        if self.staging_dir.exists():
            shutil.rmtree(self.staging_dir)
        (self.staging_dir / "bin").mkdir(parents=True, exist_ok=True)

        # 1. Base files
        for f in ["module.prop", "customize.sh", "service.sh", "action.sh"]:
            src = self.module_dir / f
            if src.exists():
                shutil.copy2(src, self.staging_dir / f)

        # 2. Daemon binary
        daemon_bin = daemon_bin or (self.module_dir / "daemon" / "target" / "aarch64-unknown-linux-musl" / "release" / "kread_daemon")
        if daemon_bin and daemon_bin.exists():
            dest_daemon = self.staging_dir / "bin" / "kread_daemon"
            shutil.copy2(daemon_bin, dest_daemon)
            os.chmod(dest_daemon, 0o755)

        # 3. Kernel modules
        if ko_510 is None:
            ko_510 = self.out_dir / "kread_5.10.ko"
            if not ko_510.exists():
                ko_510 = self.module_dir / "kread_5.10.ko"

        if ko_612 is None:
            ko_612 = self.out_dir / "kread_6.12.ko"
            if not ko_612.exists():
                ko_612 = self.module_dir / "kread_6.12.ko"

        ver_clean = self.version.lstrip("v")
        if target in ("all", "universal"):
            if ko_510.exists():
                shutil.copy2(ko_510, self.staging_dir / "kread_5.10.ko")
            if ko_612.exists():
                shutil.copy2(ko_612, self.staging_dir / "kread_6.12.ko")
            # If 6.12 exists, also provide default kread.ko symlink/copy
            if ko_612.exists():
                shutil.copy2(ko_612, self.staging_dir / "kread.ko")
            elif ko_510.exists():
                shutil.copy2(ko_510, self.staging_dir / "kread.ko")
            zip_filename = f"kread-universal-v{ver_clean}.zip"
        elif target == "5.10":
            if ko_510.exists():
                shutil.copy2(ko_510, self.staging_dir / "kread.ko")
                shutil.copy2(ko_510, self.staging_dir / "kread_5.10.ko")
            zip_filename = f"kread-android12-5.10-v{ver_clean}.zip"
        elif target == "6.12":
            if ko_612.exists():
                shutil.copy2(ko_612, self.staging_dir / "kread.ko")
                shutil.copy2(ko_612, self.staging_dir / "kread_6.12.ko")
            zip_filename = f"kread-android16-6.12-v{ver_clean}.zip"
        else:
            zip_filename = f"kread-{target}-v{ver_clean}.zip"

        dest_zip = self.out_dir / zip_filename

        # Write ZIP with proper POSIX file permissions
        with zipfile.ZipFile(dest_zip, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
            for root, _, files in os.walk(self.staging_dir):
                for f in sorted(files):
                    file_path = Path(root) / f
                    rel_path = file_path.relative_to(self.staging_dir)
                    zinfo = zipfile.ZipInfo.from_file(file_path, arcname=str(rel_path))

                    # Set executable bits for scripts and binary
                    if f.endswith(".sh") or f == "kread_daemon":
                        zinfo.external_attr = 0o100755 << 16
                    else:
                        zinfo.external_attr = 0o100644 << 16

                    zf.writestr(zinfo, file_path.read_bytes())

        # Also copy to extra_out_dir if specified (e.g. out/release/modules)
        if extra_out_dir:
            extra_out_dir.mkdir(parents=True, exist_ok=True)
            shutil.copy2(dest_zip, extra_out_dir / zip_filename)
            console.print(f"  [dim]+ Mirrored to: {extra_out_dir / zip_filename}[/dim]")

        # Convenience symlink
        latest_symlink = self.out_dir / "kread-latest.zip"
        try:
            if latest_symlink.is_symlink() or latest_symlink.exists():
                latest_symlink.unlink()
            latest_symlink.symlink_to(dest_zip.name)
        except Exception:
            pass

        file_size = format_bytes(dest_zip.stat().st_size)
        sha256 = compute_sha256(dest_zip)

        table = Table(title="🎉 kread Companion Module Packaging Complete", border_style="green")
        table.add_column("Property", style="bold cyan")
        table.add_column("Value", style="bold white")
        table.add_row("Package", dest_zip.name)
        table.add_row("Target", target.upper())
        table.add_row("Size", file_size)
        table.add_row("SHA256", sha256)
        table.add_row("Location", str(dest_zip))
        console.print(table)

        return dest_zip

    def clean(self, clean_all: bool = False):
        """Clean intermediate compilation artifacts and caches at user request."""
        console.print(f"\n[bold yellow]🧹 Cleaning kread module artifacts (clean_all={clean_all})...[/bold yellow]")
        cleaned_count = 0

        # Guard: clean any legacy files from source tree (root and src/)
        self._clean_source_tree_legacy_files()

        # 1. Clean shadow build intermediate cache (out/build_510)
        shadow_dir = self.out_dir / "build_510"
        if shadow_dir.exists():
            shutil.rmtree(shadow_dir, ignore_errors=True)
            cleaned_count += 1
            console.print(f"  [dim]- Removed build cache: {shadow_dir}[/dim]")

        # 2. If clean_all: deep clean out/ (zips, kos) and daemon/target/
        if clean_all:
            if self.out_dir.exists():
                shutil.rmtree(self.out_dir, ignore_errors=True)
                cleaned_count += 1
                console.print(f"  [dim]- Removed output directory: {self.out_dir}[/dim]")
            daemon_target = self.module_dir / "daemon" / "target"
            if daemon_target.exists():
                shutil.rmtree(daemon_target, ignore_errors=True)
                cleaned_count += 1
                console.print(f"  [dim]- Removed daemon target: {daemon_target}[/dim]")
            tests_bin = self.module_dir / "tests" / "bin"
            if tests_bin.exists():
                shutil.rmtree(tests_bin, ignore_errors=True)
                cleaned_count += 1

        console.print(f"  [bold green]✓ Clean operation complete ({cleaned_count} targets).[/bold green]")

    def run(self, target: str = "all", skip_daemon: bool = False, skip_ko: bool = False, do_package: bool = True, extra_out_dir: Optional[Path] = None):
        """Unified execution workflow."""
        ko_510 = None
        ko_612 = None
        daemon_bin = None

        if not skip_ko:
            if target in ("all", "5.10"):
                ko_510 = self.build_ko_510()
            if target in ("all", "6.12"):
                ko_612 = self.build_ko_612()

        if not skip_daemon:
            daemon_bin = self.build_daemon()

        if do_package:
            return self.package(
                target=target,
                ko_510=ko_510,
                ko_612=ko_612,
                daemon_bin=daemon_bin,
                extra_out_dir=extra_out_dir,
            )
        return None


def main():
    argv = sys.argv[1:]
    # Default to 'build' subcommand if no subcommand is given or build flags are passed directly
    if not argv or argv[0] not in ("build", "clean", "-h", "--help"):
        sys.argv.insert(1, "build")

    parser = argparse.ArgumentParser(description="kread Companion Unified Python Builder")
    subparsers = parser.add_subparsers(dest="subcommand", help="Subcommand to execute")

    p_build = subparsers.add_parser("build", help="Build module components and package ZIP")
    p_build.add_argument("-t", "--target", choices=["all", "5.10", "6.12"], default="all", help="Target kernel (default: all)")
    p_build.add_argument("--skip-daemon", action="store_true", help="Skip compiling Rust daemon")
    p_build.add_argument("--skip-ko", action="store_true", help="Skip compiling kernel modules")
    p_build.add_argument("--no-package", action="store_true", help="Skip assembling flashable zip")
    p_build.add_argument("--extra-out-dir", type=str, default=None, help="Additional output directory")

    p_clean = subparsers.add_parser("clean", help="Clean compilation artifacts and intermediate files")
    p_clean.add_argument("--all", action="store_true", help="Deep clean: remove out/ and daemon target/ as well")

    args = parser.parse_args()
    builder = KreadBuilder()

    if args.subcommand == "clean":
        builder.clean(clean_all=args.all)
    else:
        extra_dir = Path(args.extra_out_dir) if args.extra_out_dir else None
        builder.run(
            target=args.target,
            skip_daemon=args.skip_daemon,
            skip_ko=args.skip_ko,
            do_package=not args.no_package,
            extra_out_dir=extra_dir,
        )


if __name__ == "__main__":
    main()
