"""Drive the real native client through a terminal, including its background query."""
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

port, mode, answer, stdin = sys.argv[1:]
port = int(port)
if not 0 < port <= 65535:
    raise ValueError("expected a loopback TCP port")
origin = f"http://127.0.0.1:{port}"
theme = mode == "theme"
master, slave = pty.openpty()
fcntl.fcntl(master, fcntl.F_SETFL, os.O_NONBLOCK)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

env = {k: v for k, v in os.environ.items() if k not in ("NO_COLOR", "GM_TUI_THEME", "COLORFGBG")}
env["TERM"] = "xterm-256color"
if theme:
    env["COLORTERM"] = "truecolor"
args = ["./graphite-meter-client", "-url", origin]
if not theme:
    args += ["-stages", "latency", "-latency-duration", "5s", "-warmup", "0", "-ping", "80ms"]
p = subprocess.Popen(args, stdin=subprocess.DEVNULL if stdin == "redirected" else slave,
                     stdout=slave, stderr=slave, preexec_fn=session, env=env)
os.close(slave)
checked, measuring = b"about ", "░".encode()
keys = {
    "theme": [],
    "check-quit": [(b"checking paths", b"q")],
    "redirected-quit": [(checked, b"q")],
    "setup-interrupt": [(checked, b"\x03")],
    "run-interrupt": [(checked, b"r"), (measuring, b"\x03")],
    "run-quit": [(checked, b"r"), (measuring, b"q")],
    "run-abort": [(checked, b"r"), (measuring, b"\x03\x03")],
    "confirmed-stop": [(checked, b"r"), (measuring, b"\x1b"), (b"confirm", b"\x1b"), (b"Stopped", b"q")],
}[mode]
output, step, mark = b"", 0, 0
drawn, alive = None, None
started = time.monotonic()
deadline = started + 8
answer = answer.encode()
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                output += os.read(master, 65536)
            except OSError:
                break
            if answer and b"\x1b[c" in output:
                os.write(master, answer)
                answer = b""
            if theme and drawn is None and b"Graphite Meter" in output:
                drawn = time.monotonic() - started
                time.sleep(0.3)
                alive = p.poll() is None
                os.write(master, b"q")
            if step < len(keys) and keys[step][0] in output[mark:]:
                os.write(master, keys[step][1])
                step += 1
                mark = len(output)
        if p.poll() is not None:
            while select.select([master], [], [], 0)[0]:
                try:
                    output += os.read(master, 65536)
                except OSError:
                    break
            break
    code = p.wait(timeout=max(0, deadline - time.monotonic()))
    print(json.dumps({"code": code, "step": step, "drawn": drawn, "alive": alive,
                      "text": output.decode("utf-8", "replace")}))
finally:
    if p.poll() is None:
        p.kill()
        p.wait()
    os.close(master)
