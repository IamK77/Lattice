#!/usr/bin/env python3
"""A Lattice component in ~40 lines of stdlib Python.

Speaks the v1 bridge contract (docs/contracts/06): one JSON per line.
Ports: input "execute" (core.tool.exec_started) -> output "outcome"
(core.tool.exec_completed). One tool: sha256 over arguments.text.
The "die" tool exits mid-handling, for the crash-containment test.
"""
import hashlib
import json
import sys

for line in sys.stdin:
    msg = json.loads(line)
    if "hello" in msg:
        continue  # instance/config noted; nothing to set up
    if "stop" in msg:
        break
    if "deliver" not in msg:
        continue  # tolerate unknown lines (forward compat)
    event = msg["deliver"]["event"]
    payload = event["payload"]
    tool = payload.get("tool")
    if tool == "die":
        sys.exit(1)  # simulate a crash mid-handling: no emit, no processed
    if tool == "sha256":
        text_arg = (payload.get("arguments") or {}).get("text")
        if isinstance(text_arg, str):
            digest = hashlib.sha256(text_arg.encode()).hexdigest()
            out = {"call": payload.get("call"), "status": "ok", "result": digest}
        else:
            out = {"call": payload.get("call"), "status": "error", "error": {
                "code": "tool.bad_arguments",
                "message": "arguments.text must be a string",
                "blame": "request",
            }}
    else:
        out = {"call": payload.get("call"), "status": "error", "error": {
            "code": "tool.unknown",
            "message": f"unknown tool: {tool}",
            "blame": "request",
        }}
    print(json.dumps({"emit": {
        "port": "outcome",
        "type": "core.tool.exec_completed",
        "causes": [event["id"]],
        "payload": out,
    }}), flush=True)
    print(json.dumps({"processed": True}), flush=True)
