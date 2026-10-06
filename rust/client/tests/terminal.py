"""Drive the client binary in the working directory through a pseudo-terminal.

    python3 terminal.py quit|interrupt|signals PORT

It runs ./graphite-meter-client against http://127.0.0.1:PORT in a 100x24 terminal, presses the mode's keys once
their cues appear in the output, and prints the exit status and everything the client wrote as JSON. In the signals
mode the client runs with --report and its background query goes unanswered; once the query appears it gets SIGINT,
then SIGTERM once no thread holds the SIGINT pending, and the JSON adds the seconds from SIGTERM to its exit.
"""

import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time

# Each mode's arguments and keys, each key sent once its cue follows the previous one: the first frame's title, then
# the run's busy progress bar.
FRAME, PREPARING, QUERY = b"\x1b]2;Graphite Meter", b"\x1b]9;4;3\x07", b"\x1b]11;?"
MODES = {
    "quit": ([], [(FRAME, b"q")]),
    "interrupt": ([], [(FRAME, b"r"), (PREPARING, b"\x03")]),
    "signals": (["--report"], []),
}

mode, port = sys.argv[1], int(sys.argv[2])
if mode not in MODES or not 0 < port <= 65535:
    raise SystemExit("usage: terminal.py quit|interrupt|signals PORT")
arguments, keys = MODES[mode]
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))


def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)


def interrupt_then_terminate(pid):
    """Sends SIGINT, waits until no thread holds it pending, then sends SIGTERM; when SIGTERM went."""
    os.kill(pid, signal.SIGINT)
    held = 1 << (signal.SIGINT - 1)
    for _ in range(2000):
        with open(f"/proc/{pid}/status") as status:
            pending = next(int(line.split()[1], 16) for line in status if line.startswith("ShdPnd:"))
        if not pending & held:
            break
        time.sleep(0.001)
    os.kill(pid, signal.SIGTERM)
    return time.monotonic()


env = dict(os.environ, TERM="xterm-256color")
client = subprocess.Popen(["./graphite-meter-client", *arguments, "-url", f"http://127.0.0.1:{port}"], stdin=slave,
                          stdout=slave, stderr=slave, preexec_fn=session, env=env)
os.close(slave)
output, step, mark, terminated = b"", 0, 0, None
deadline = time.monotonic() + 10
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                output += os.read(master, 65536)
            except OSError:
                break
            if step < len(keys) and keys[step][0] in output[mark:]:
                os.write(master, keys[step][1])
                step, mark = step + 1, len(output)
            if mode == "signals" and terminated is None and QUERY in output:
                terminated = interrupt_then_terminate(client.pid)
        elif client.poll() is not None:
            break
    code = client.wait(timeout=max(0.0, deadline - time.monotonic()))
    exited = None if terminated is None else time.monotonic() - terminated
    print(json.dumps({"code": code, "keys": step, "text": output.decode("utf-8", "replace"), "exited": exited}))
finally:
    if client.poll() is None:
        client.kill()
        client.wait()
    os.close(master)
