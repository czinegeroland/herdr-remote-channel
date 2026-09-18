#!/usr/bin/env python3
"""Send one request to a running daemon's local socket and print the answer.

The trusted interface is the only path that can admit a joiner or release a
quarantined body: PRD section 22.7 puts those decisions behind it, and the CLI
refuses them outright. A pane normally drives it, and a pane needs a terminal
it will not get on a runner -- so the end-to-end test speaks the protocol
directly. This is not a bypass. It is the same socket, carrying the same
request, that `Remote channel review` sends when a person presses a key.

The frame is a big-endian `u32` length followed by a JSON body, matching
`crates/hrc-ipc/src/frame.rs`.
"""

import json
import socket
import struct
import sys
import time

MAX_FRAME_BYTES = 1 << 20


def call(path: str, request: dict, attempts: int = 80) -> dict:
    # The daemon announces itself before the socket is necessarily accepting,
    # so a first connection can lose a race that a second one wins.
    last = None
    for _ in range(attempts):
        try:
            client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            client.connect(path)
            break
        except OSError as error:  # noqa: PERF203 - retry is the point
            last = error
            time.sleep(0.05)
    else:
        raise SystemExit(f"could not connect to {path}: {last}")

    with client:
        body = json.dumps(request).encode()
        client.sendall(struct.pack(">I", len(body)) + body)

        header = read_exactly(client, 4)
        (length,) = struct.unpack(">I", header)
        if length > MAX_FRAME_BYTES:
            raise SystemExit(f"answer declared {length} bytes")
        return json.loads(read_exactly(client, length))


def read_exactly(client: socket.socket, count: int) -> bytes:
    chunks = []
    remaining = count
    while remaining:
        chunk = client.recv(remaining)
        if not chunk:
            raise SystemExit("the daemon closed the connection mid-frame")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: trusted-call.py <socket> <method> <params-json>")

    socket_path, method, params = sys.argv[1], sys.argv[2], sys.argv[3]
    answer = call(socket_path, {"method": method, "params": json.loads(params)})
    print(json.dumps(answer))

    # A transport-level answer is not an application-level success, and a test
    # that only checked the former would pass through every refusal.
    if answer.get("status") != "ok":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
