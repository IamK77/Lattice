#!/usr/bin/env python3
"""A bridge child that records the moment it was started.

Used to tell a DEFERRED component from an eager one: the marker file named on
the command line is written when the process starts, so a test can ask "has
this child been started yet?" without guessing from timing. Otherwise it is
the smallest possible tool provider — one tool, `echo`.
"""
import json
import sys

marker = sys.argv[1] if len(sys.argv) > 1 else None
if marker:
    with open(marker, "a", encoding="utf-8") as fh:
        fh.write("started\n")

for line in sys.stdin:
    msg = json.loads(line)
    if "hello" in msg:
        continue
    if "stop" in msg:
        break
    if "deliver" not in msg:
        continue
    payload = msg["deliver"]["event"]["payload"]
    out = {
        "call": payload.get("call"),
        "status": "ok",
        "result": (payload.get("arguments") or {}).get("text"),
    }
    print(json.dumps({"emit": {
        "port": "outcome",
        "type": "core.tool.exec_completed",
        "causes": [msg["deliver"]["event"]["id"]],
        "payload": out,
    }}), flush=True)
    print(json.dumps({"processed": True}), flush=True)
