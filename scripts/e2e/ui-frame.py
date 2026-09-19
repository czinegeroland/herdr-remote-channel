#!/usr/bin/env python3
"""Open a Herdr plugin pane at a known size and print what it drew.

The only test in this repository that looks at the rendered interface the way
a person does: through a real Herdr, on a real terminal, reading the cells
that actually reached the screen. Everything else draws into a ratatui buffer
and asserts on that, which cannot see a frame broken by the host, a pane that
exited on startup, or a title longer than the border it sits in -- all three
of which shipped.

The terminal size is set on the pty rather than by resizing the pane, because
a pane's own size is the host's business and a test that drove it would be
testing Herdr. What this fixes is the width the interface is asked to draw
into, which is the input that broke.
"""

import argparse
import json
import os
import pty
import shutil
import signal
import socket
import struct
import subprocess
import sys
import termios
import time
import fcntl


def pty_at(columns: int, rows: int) -> tuple[int, int]:
    """A pty whose slave already reports the size under test."""
    primary, secondary = pty.openpty()
    size = struct.pack("HHHH", rows, columns, 0, 0)
    fcntl.ioctl(secondary, termios.TIOCSWINSZ, size)
    return primary, secondary


def call(socket_path: str, method: str, params: dict) -> dict:
    """One newline-delimited JSON request, and its answer."""
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(15)
    client.connect(socket_path)
    request = json.dumps({"id": "ui-frame", "method": method, "params": params})
    client.sendall(request.encode() + b"\n")

    buffered = b""
    while not buffered.endswith(b"\n"):
        chunk = client.recv(65536)
        if not chunk:
            break
        buffered += chunk
    client.close()

    answer = json.loads(buffered.decode().splitlines()[0])
    if "error" in answer:
        raise SystemExit(f"herdr refused {method}: {answer['error']}")
    return answer["result"]


def wait_for(path: str, seconds: float) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if os.path.exists(path):
            return
        time.sleep(0.1)
    raise SystemExit(f"herdr never created {path}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--entrypoint", default="inbox")
    parser.add_argument("--plugin", default="herdr-remote-channel")
    parser.add_argument("--session", default="hrc-ui")
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--settle", type=float, default=3.0)
    parser.add_argument("--keys", default="", help="keys to send before reading")
    parser.add_argument("--layout", action="store_true", help="report pane rects")
    parser.add_argument(
        "--hrc-home",
        default="",
        help="HRC_HOME for the captured Herdr, so its panes read that installation",
    )
    arguments = parser.parse_args()

    herdr = shutil.which("herdr")
    if herdr is None:
        raise SystemExit("herdr is not on PATH")

    socket_path = os.path.expanduser(
        f"~/.config/herdr/sessions/{arguments.session}/herdr.sock"
    )
    primary, secondary = pty_at(arguments.columns, arguments.rows)

    # A session of its own, so a UI test never touches a session someone is
    # working in, and start_new_session so stopping it cannot reach this
    # process group.
    # A plugin pane inherits the environment of the server that launched it,
    # so the installation under test is chosen here rather than per pane.
    environment = dict(os.environ)
    if arguments.hrc_home:
        environment["HRC_HOME"] = arguments.hrc_home

    client = subprocess.Popen(
        [herdr, "--session", arguments.session],
        stdin=secondary,
        stdout=secondary,
        stderr=secondary,
        start_new_session=True,
        env=environment,
    )
    os.close(secondary)

    try:
        wait_for(socket_path, 30)

        opened = call(
            socket_path,
            "plugin.pane.open",
            {
                "plugin_id": arguments.plugin,
                "entrypoint": arguments.entrypoint,
                "placement": "split",
                "direction": "right",
                "focus": True,
            },
        )
        pane_id = opened["plugin_pane"]["pane"]["pane_id"]

        # The interface has to paint before it can be read. A plugin pane is a
        # process Herdr spawns, so this waits for the program rather than for
        # a redraw.
        time.sleep(arguments.settle)

        for key in arguments.keys.split():
            call(socket_path, "pane.send_keys", {"pane_id": pane_id, "keys": [key]})
            time.sleep(0.4)

        if arguments.layout:
            layout = call(socket_path, "pane.layout", {"pane_id": pane_id})["layout"]
            area = layout["area"]
            sys.stderr.write(f"terminal {area['width']}x{area['height']}\n")
            for pane in layout["panes"]:
                rect = pane["rect"]
                sys.stderr.write(
                    f"  {pane['pane_id']} x={rect['x']} width={rect['width']}\n"
                )

        frame = call(
            socket_path,
            "pane.read",
            {"pane_id": pane_id, "source": "visible", "strip_ansi": True},
        )
        sys.stdout.write(frame.get("read", frame).get("text", ""))
        return 0
    finally:
        try:
            call(socket_path, "server.stop", {})
        except Exception:
            client.send_signal(signal.SIGTERM)
        os.close(primary)
        try:
            client.wait(timeout=10)
        except subprocess.TimeoutExpired:
            client.kill()


if __name__ == "__main__":
    raise SystemExit(main())
