"""Drive the client binary in the working directory through a pseudo-terminal.

    python3 terminal.py quit|interrupt PORT

It runs ./graphite-meter-client against http://127.0.0.1:PORT in a 100x24 terminal, presses the mode's keys once
their cues appear in the output, and prints the exit status and everything the client wrote as JSON.
"""

import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time

# Each mode's keys, each sent once its cue follows the previous one: the first frame's title, then the run's busy
# progress bar.
FRAME, PREPARING = b"\x1b]2;Graphite Meter", b"\x1b]9;4;3\x07"
MODES = {
    "quit": [(FRAME, b"q")],
    "interrupt": [(FRAME, b"r"), (PREPARING, b"\x03")],
}

mode, port = sys.argv[1], int(sys.argv[2])
if mode not in MODES or not 0 < port <= 65535:
    raise SystemExit("usage: terminal.py quit|interrupt PORT")
keys = MODES[mode]
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))


def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)


env = dict(os.environ, TERM="xterm-256color")
client = subprocess.Popen(["./graphite-meter-client", "-url", f"http://127.0.0.1:{port}"], stdin=slave,
                          stdout=slave, stderr=slave, preexec_fn=session, env=env)
os.close(slave)
output, step, mark = b"", 0, 0
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
        elif client.poll() is not None:
            break
    code = client.wait(timeout=max(0.0, deadline - time.monotonic()))
    print(json.dumps({"code": code, "keys": step, "text": output.decode("utf-8", "replace")}))
finally:
    if client.poll() is None:
        client.kill()
        client.wait()
    os.close(master)
