"""Hosted-only fault evidence for the proposed OCR-specific systemd boundary."""
import argparse
import errno
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid


def child(case):
    if case == "bounds":
        group = Path("/sys/fs/cgroup") / Path("/proc/self/cgroup").read_text().split("::", 1)[1].strip().lstrip("/")
        memory = int((group / "memory.max").read_text())
        pids = int((group / "pids.max").read_text())
        assert memory == 1536 * 1024 * 1024 and pids == 64
        assert sorted(path.name for path in Path("/sys/class/net").iterdir()) == ["lo"]
        with socket.socket() as sock:
            sock.settimeout(1)
            assert sock.connect_ex(("1.1.1.1", 443)) != 0
        stats = os.statvfs("/tmp")
        capacity = stats.f_blocks * stats.f_frsize
        assert 0 < capacity <= 512 * 1024 * 1024
        written = 0
        with tempfile.TemporaryFile(dir="/tmp") as first, tempfile.TemporaryFile(dir="/tmp") as second:
            block = b"x" * (1024 * 1024)
            try:
                for index in range(520):
                    target = first if index < 300 else second
                    written += target.write(block)
                    target.flush()
            except OSError as error:
                assert error.errno == errno.ENOSPC
            else:
                raise AssertionError("private tmpfs did not enforce its capacity")
        print(json.dumps({"memory_max": memory, "pids_max": pids,
                          "tmp_capacity": capacity, "tmp_written": written,
                          "net_namespace": os.readlink("/proc/self/ns/net")}), flush=True)
    elif case == "memory":
        print(json.dumps({"pid": os.getpid(), "attempted_bytes": 1800 * 1024 * 1024}), flush=True)
        allocation = bytearray(1800 * 1024 * 1024)
        raise AssertionError(f"memory cap failed: allocated {len(allocation)}")
    elif case == "cancel":
        parent = os.getpid()
        descendant = os.fork()
        if descendant == 0:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            while True:
                time.sleep(1)
        print(json.dumps({"parent": parent, "descendant": descendant}), flush=True)
        while True:
            time.sleep(1)


def run(output):
    assert os.geteuid() == 0, "requires isolated hosted root, not production execution"
    output.parent.mkdir(parents=True, exist_ok=True)
    report = {"schema": "ocr-systemd-boundary-probe/v1", "cases": {},
              "scope": "mechanism fault evidence only; not yet runtime integration"}
    for case in ("bounds", "memory", "cancel"):
        unit = "reading-mcp-ocr-probe-" + uuid.uuid4().hex + ".service"
        command = ["systemd-run", "--quiet", "--wait", "--pipe", "--unit=" + unit,
                   "--property=MemoryMax=1536M", "--property=MemorySwapMax=0",
                   "--property=TasksMax=64", "--property=PrivateNetwork=yes",
                   "--property=TemporaryFileSystem=/tmp:rw,size=512M,mode=0700",
                   "--property=RuntimeMaxSec=20", "--property=TimeoutStopSec=1",
                   "--property=KillMode=control-group", "--property=LimitNOFILE=256",
                   "/usr/bin/python3", str(Path(__file__).resolve()), "--child", case]
        process = None
        try:
            if case == "cancel":
                process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                assert select.select([process.stdout], [], [], 5)[0], "child did not report readiness"
                ids = json.loads(process.stdout.readline())
                started = time.monotonic()
                subprocess.run(["systemctl", "stop", unit], check=True, timeout=3, capture_output=True)
                process.communicate(timeout=2)
                while any(Path(f"/proc/{pid}").exists() for pid in ids.values()):
                    assert time.monotonic() - started < 2, "descendant survived cleanup deadline"
                    time.sleep(.01)
                report["cases"][case] = {"pids": ids, "cleanup_seconds": time.monotonic() - started}
            else:
                result = subprocess.run(command, capture_output=True, text=True, timeout=25)
                record = json.loads(result.stdout.strip())
                if case == "bounds":
                    assert result.returncode == 0, result.stderr[-2048:]
                    assert record["net_namespace"] != os.readlink("/proc/self/ns/net")
                else:
                    status = subprocess.run(["systemctl", "show", unit, "--property=Result", "--value"],
                                            capture_output=True, text=True, check=True, timeout=3).stdout.strip()
                    assert result.returncode != 0 and status == "oom-kill", status
                    record["unit_result"] = status
                report["cases"][case] = record
        except Exception as error:
            report["cases"][case] = {"error": str(error)}
        finally:
            # Only the unpredictable unit owned by this iteration is stopped.
            subprocess.run(["systemctl", "stop", unit], capture_output=True, timeout=5)
            subprocess.run(["systemctl", "reset-failed", unit], capture_output=True, timeout=5)
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate(timeout=3)
            output.write_text(json.dumps(report, indent=2))
        print(json.dumps({case: report["cases"][case]}), flush=True)
    if any("error" in item for item in report["cases"].values()):
        raise SystemExit("sandbox mechanism failed; preserve report")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--child", choices=["bounds", "memory", "cancel"])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.child:
        child(args.child)
    else:
        run(args.output)
