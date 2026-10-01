"""Bounded startup and shutdown for real interoperability server processes."""
from contextlib import contextmanager
import signal
import subprocess
import sys
import time


@contextmanager
def running(command, environment, log, ready):
    with log.open("w") as output:
        process = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"server exited {process.returncode}: {log.read_text()}")
                try:
                    if result := ready():
                        break
                except (OSError, TimeoutError):
                    pass
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"server did not become ready: {log.read_text()}")
                time.sleep(0.05)
            yield result
        finally:
            failed = sys.exc_info()[0] is not None
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                if not failed:
                    raise RuntimeError(f"server failed to shut down: {log.read_text()}")
            if process.returncode != 0 and not failed:
                raise RuntimeError(f"server exited {process.returncode}: {log.read_text()}")
