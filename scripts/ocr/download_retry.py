"""Retry slow download commands without changing what they fetch.

Hosted runners occasionally reach the Ubuntu mirror or the package index at a
few kilobytes per second, so a single fixed per-attempt timeout fails the
component build for a network reason rather than a packaging reason. This
wrapper only re-runs a command that timed out or that exited with a message
that is known to be transient; it never weakens the pinned package set, the
hash verification or any other check performed by the caller.
"""
import subprocess
import sys
import time


ATTEMPTS = 3
ATTEMPT_TIMEOUT_SECONDS = 600
PAUSE_SECONDS = 15
MAX_LOG_LINES = 20

APT_TRANSIENT_MESSAGES = (
    "Temporary failure",
    "Could not connect",
    "Connection timed out",
    "Hash Sum mismatch",
)
PIP_TRANSIENT_MESSAGES = (
    "Temporary failure",
    "Connection",
    "timed out",
    "Hash Sum mismatch",
    "502 Bad Gateway",
    "503 Service Unavailable",
)


class AttemptLog:
    """Bound retry logging so a retried download cannot flood the job log."""

    def __init__(self, limit=MAX_LOG_LINES, stream=None):
        self.limit = limit
        self.stream = sys.stderr if stream is None else stream
        self.lines = 0

    def write(self, message):
        if self.lines >= self.limit:
            return
        self.lines += 1
        print(message, file=self.stream, flush=True)


LOG = AttemptLog()


def _text(error):
    parts = []
    for stream in (error.stdout, error.stderr):
        if isinstance(stream, bytes):
            stream = stream.decode("utf-8", "replace")
        if stream:
            parts.append(stream)
    return "\n".join(parts)


def _tail(text, limit=300):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return " | ".join(lines[-3:])[-limit:]


def apt_transient(error):
    """apt-get exits 100 for every failure; only these messages are transient."""
    return error.returncode == 100 and any(
        message in _text(error) for message in APT_TRANSIENT_MESSAGES)


def pip_transient(error):
    """pip exits 1 for every failure; only these messages are transient."""
    return error.returncode == 1 and any(
        message in _text(error) for message in PIP_TRANSIENT_MESSAGES)


def run_with_retry(args, transient, runner=subprocess.run, attempts=ATTEMPTS,
                   timeout=ATTEMPT_TIMEOUT_SECONDS, pause=PAUSE_SECONDS,
                   sleep=time.sleep, log=None, **kwargs):
    """Run one download command, retrying only transient failures.

    Returns the completed process of the first successful attempt and re-raises
    the last failure when the attempts are exhausted or when the failure is not
    transient. One log line per attempt, at most MAX_LOG_LINES per process.
    """
    log = LOG if log is None else log
    label = args[0] if args else "command"
    failure = None
    for attempt in range(1, attempts + 1):
        started = time.monotonic()
        try:
            completed = runner(args, check=True, timeout=timeout, **kwargs)
        except subprocess.TimeoutExpired as error:
            failure, outcome = error, "timed out after %d s" % timeout
        except subprocess.CalledProcessError as error:
            if not transient(error):
                log.write("%s attempt %d/%d in %.1f s: exit %d, not transient, giving up"
                          % (label, attempt, attempts, time.monotonic() - started, error.returncode))
                raise
            failure, outcome = error, "transient exit %d (%s)" % (error.returncode, _tail(_text(error)))
        else:
            log.write("%s attempt %d/%d in %.1f s: ok"
                      % (label, attempt, attempts, time.monotonic() - started))
            return completed
        elapsed = time.monotonic() - started
        if attempt == attempts:
            log.write("%s attempt %d/%d in %.1f s: %s, giving up"
                      % (label, attempt, attempts, elapsed, outcome))
            raise failure
        log.write("%s attempt %d/%d in %.1f s: %s, retrying in %d s"
                  % (label, attempt, attempts, elapsed, outcome, pause))
        sleep(pause)
    raise failure
