"""Drive the client binary in the working directory through a pseudo-terminal.

    python3 terminal.py quit|interrupt|signals|query PORT

Prints the exit status, the client's output and whether the terminal echoes again as JSON; signals adds the seconds
from SIGTERM to the exit.
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
ANSWER = b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c"
MODES = {
    "quit": ([], [(FRAME, b"q")], {}),
    "interrupt": ([], [(FRAME, b"r"), (PREPARING, b"\x03")], {"TERM_PROGRAM": "ghostty"}),
    "signals": (["--report"], [], {}),
    "query": (["--report"], [(QUERY, b"\x03")], {}),
}

mode, port = sys.argv[1], int(sys.argv[2])
if mode not in MODES or not 0 < port <= 65535:
    raise SystemExit("usage: terminal.py quit|interrupt|signals|query PORT")
arguments, keys, named = MODES[mode]
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


progress = ("TERM_PROGRAM", "TERM_PROGRAM_VERSION", "WT_SESSION", "ConEmuANSI")
env = {name: value for name, value in os.environ.items() if name not in progress}
env.update(TERM="xterm-256color", **named)
client = subprocess.Popen(["./graphite-meter-client", *arguments, "-url", f"http://127.0.0.1:{port}"], stdin=slave,
                          stdout=slave, stderr=slave, preexec_fn=session, env=env)
# A copy of the terminal's side stays open to read its modes after the exit.
probe = os.dup(slave)
os.close(slave)
output, step, mark, terminated, answered = b"", 0, 0, None, False
deadline = time.monotonic() + 10
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                output += os.read(master, 65536)
            except OSError:
                break
            if mode not in ("signals", "query") and not answered and QUERY in output:
                os.write(master, ANSWER)
                answered = True
            if step < len(keys) and keys[step][0] in output[mark:]:
                os.write(master, keys[step][1])
                step, mark = step + 1, len(output)
            if mode == "signals" and terminated is None and QUERY in output:
                terminated = interrupt_then_terminate(client.pid)
        elif client.poll() is not None:
            break
    code = client.wait(timeout=max(0.0, deadline - time.monotonic()))
    exited = None if terminated is None else time.monotonic() - terminated
    modes = termios.tcgetattr(probe)[3]
    restored = bool(modes & termios.ECHO and modes & termios.ICANON)
    text = output.decode("utf-8", "replace")
    print(json.dumps({"code": code, "keys": step, "text": text, "exited": exited, "restored": restored}))
finally:
    if client.poll() is None:
        client.kill()
        client.wait()
    os.close(master)
    os.close(probe)
