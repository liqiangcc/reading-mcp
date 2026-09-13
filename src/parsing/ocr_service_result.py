"""Trusted ExecStopPost callback: manager result only, never worker stdout.

The owning Rust process keeps a nonblocking FIFO. No files survive cancellation,
and absent owners do not delay systemd's automatic collection.
"""
import os
import stat
import sys


def main():
    path, device, inode, token = sys.argv[1:5]
    descriptor = os.open(path, os.O_WRONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        observed = os.fstat(descriptor)
        if (not stat.S_ISFIFO(observed.st_mode)
                or observed.st_dev != int(device) or observed.st_ino != int(inode)):
            raise ValueError('OCR result pipe identity mismatch')
        fields = [token] + [os.environ.get(name, '') for name in
                            ('SERVICE_RESULT', 'EXIT_CODE', 'EXIT_STATUS')]
        if any(len(field) > 64 or '\n' in field or not field.isascii() for field in fields):
            raise ValueError('invalid OCR manager result')
        payload = ('\n'.join(fields) + '\n').encode('ascii')
        if os.write(descriptor, payload) != len(payload):
            raise ValueError('incomplete OCR result write')
    finally:
        os.close(descriptor)


if __name__ == '__main__':
    main()
