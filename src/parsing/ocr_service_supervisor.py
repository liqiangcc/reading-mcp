"""Transient OCR service main: a lost Rust-owned pipe ends the whole unit.

The systemd manager applies KillMode=control-group to remaining descendants when
this main process exits. No document bytes or credentials use this control pipe.
"""
import os
import select
import stat
import subprocess
import sys


def main():
    path, device, inode, token = sys.argv[1:5]
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    observed = os.fstat(descriptor)
    if (not stat.S_ISFIFO(observed.st_mode)
            or observed.st_dev != int(device) or observed.st_ino != int(inode)
            or os.read(descriptor, len(token)) != token.encode("ascii")):
        raise ValueError("OCR owner pipe identity mismatch")
    # A cancelled owner may disappear before systemd has even started this unit.
    # Never start the worker after EOF, even if the original token was buffered.
    try:
        os.read(descriptor, 1)
    except BlockingIOError:
        pass
    else:
        raise ValueError("OCR owner already closed")
    # Only this small trusted supervisor may open the root caller's owner pipe.
    # The PDF/native-library process must not inherit root credentials/capabilities.
    # Numeric overflow/nobody credentials do not require NSS inside an offline root.
    child = subprocess.Popen(sys.argv[5:], user=65534, group=65534,
                             extra_groups=[], umask=0o077)
    while child.poll() is None:
        if select.select([descriptor], [], [], .05)[0]:
            # Any unexpected control data is also fail-closed. Exiting the unit's
            # main process lets systemd terminate/reap the entire cgroup, not just
            # the worker's immediate process group.
            raise SystemExit(125)
    raise SystemExit(child.returncode if child.returncode >= 0 else 128 - child.returncode)


if __name__ == "__main__":
    main()
