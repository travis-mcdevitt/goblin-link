#!/usr/bin/env python3
"""ipc-call.py <socket> <method> [json-params]: one request to goblind over its Unix socket.

Debug helper for devices without goblinctl's IPC client. Prints the JSON response line.
Works with Python 3.7+ (the Brick's PortMaster Python)."""
import json, socket, sys
sock, method = sys.argv[1], sys.argv[2]
params = json.loads(sys.argv[3]) if len(sys.argv) > 3 else {}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(10)
s.connect(sock)
s.sendall((json.dumps({"id": 0, "method": "hello", "params": {"client": "ctl", "version": "ipc-call"}}) + "\n").encode())
s.sendall((json.dumps({"id": 1, "method": method, "params": params}) + "\n").encode())
buf = b""
while True:
    chunk = s.recv(65536)
    if not chunk:
        break
    buf += chunk
    for line in buf.split(b"\n")[:-1]:
        msg = json.loads(line)
        if msg.get("id") == 1:
            print(json.dumps(msg, indent=1))
            sys.exit(0 if msg.get("ok") else 1)
    buf = buf.split(b"\n")[-1]
