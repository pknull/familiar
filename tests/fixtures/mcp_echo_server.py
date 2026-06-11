#!/usr/bin/env python3
"""Minimal MCP stdio server for acceptance tests.

Speaks just enough JSON-RPC over stdin/stdout for thallus-core's stdio
client: initialize handshake, tools/list (one tool: echo), tools/call
(returns a fixed text block). No external dependencies.
"""
import json
import sys


def respond(req_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue

        method = msg.get("method", "")
        req_id = msg.get("id")

        if req_id is None:
            continue  # notification (e.g. notifications/initialized)

        if method == "initialize":
            respond(req_id, {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "fixture", "version": "0.0.1"},
                "capabilities": {"tools": {}},
            })
        elif method == "tools/list":
            respond(req_id, {
                "tools": [{
                    "name": "echo",
                    "description": "Echo fixture tool",
                    "inputSchema": {"type": "object", "properties": {}},
                }]
            })
        elif method == "tools/call":
            respond(req_id, {
                "content": [{"type": "text", "text": "fixture-echo-output"}],
                "isError": False,
            })
        else:
            respond(req_id, {})


if __name__ == "__main__":
    main()
