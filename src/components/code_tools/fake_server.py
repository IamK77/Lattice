"""Deterministic stdio peer for client tests, not a language implementation."""
import json
import pathlib
import sys

trace = pathlib.Path(sys.argv[1])
documents = {}
mode = "normal"


def send(value):
    body = json.dumps(value, ensure_ascii=False).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


def receive():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    with trace.open("a", encoding="utf-8") as output:
        output.write(json.dumps(message, ensure_ascii=False) + "\n")
    return message


while (message := receive()) is not None:
    method = message.get("method")
    params = message.get("params", {})
    if method == "initialize":
        options = params.get("initializationOptions") or {}
        mode = options.get("mode", "normal")
        if mode == "idle":
            import socket
            idle_socket = socket.create_connection(("127.0.0.1", options["port"]))
        result = {"capabilities": {"textDocumentSync": 1, "documentSymbolProvider": True,
                                    "definitionProvider": True, "referencesProvider": True}}
        if mode == "exit-with-child":
            import os
            import socket
            import subprocess
            child_socket = socket.create_connection(("127.0.0.1", options["port"]))
            fd = child_socket.fileno()
            subprocess.Popen([sys.executable, "-c",
                              f"import os, signal; os.write({fd}, (str(os.getpid()) + '\\n').encode()); signal.pause()"],
                             pass_fds=[fd])
            child_socket.close()
            # Only this leader owns the second socket; EOF proves it exited
            # without the test reaping it before process-group cleanup.
            leader_socket = socket.create_connection(("127.0.0.1", options["port"]))
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})
            os._exit(0)
    elif method == "initialized":
        if mode == "idle":
            send({"jsonrpc": "2.0", "id": "idle-config", "method": "workspace/configuration",
                  "params": {"items": [{"section": "fixture"}]}})
            answer = receive()
            assert answer["result"] == [None]
            idle_socket.sendall(b"done")
        continue
    elif method == "textDocument/didClose":
        documents.pop(params["textDocument"]["uri"], None)
        continue
    elif method == "textDocument/didOpen":
        doc = params["textDocument"]
        documents[doc["uri"]] = doc["text"]
        continue
    elif method == "textDocument/didChange":
        documents[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
        continue
    elif method == "textDocument/documentSymbol":
        uri = params["textDocument"]["uri"]
        text = documents[uri]
        # Exercise server-to-client requests without granting edit authority.
        send({"jsonrpc": "2.0", "id": "edit-probe", "method": "workspace/applyEdit",
              "params": {"edit": {"changes": {uri: []}}}})
        answer = receive()
        assert answer["result"]["applied"] is False
        if mode == "mutate":
            from urllib.parse import unquote, urlparse
            pathlib.Path(unquote(urlparse(uri).path)).write_text("x" * len(text), encoding="utf-8")
        lines = text.split("\n")
        end = {"line": len(lines) - 1, "character": len(lines[-1].encode("utf-16-le")) // 2}
        result = [{"name": "sample", "kind": 12,
                   "range": {"start": {"line": 0, "character": 0}, "end": end},
                   "selectionRange": {"start": {"line": 0, "character": 0},
                                      "end": {"line": 0, "character": 1}}}]
    elif method in ("textDocument/definition", "textDocument/references"):
        result = [{"uri": params["textDocument"]["uri"],
                   "range": {"start": {"line": 0, "character": 0},
                             "end": {"line": 0, "character": 1}}}]
    else:
        if "id" not in message:
            continue
        result = None
    send({"jsonrpc": "1.0" if mode == "bad-version" else "2.0", "id": message["id"], "result": result})
