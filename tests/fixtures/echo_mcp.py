"""A tiny MCP server (stdio, newline-delimited JSON-RPC) for live tests.

Tools: `secret_word` returns the token given as the first argument; `forbidden`
must never be called when the owner allows only `secret_word`. Every tool call is
appended to the file named by ECHO_MCP_LOG (if set), as evidence of what ran.
"""

import json
import os
import sys

TOKEN = sys.argv[1] if len(sys.argv) > 1 else "MCP-ECHO"
LOG = os.environ.get("ECHO_MCP_LOG")

TOOLS = [
    {
        "name": "secret_word",
        "description": "Returns today's secret word.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "forbidden",
        "description": "Returns a different word. Not for tasks.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    method, mid = msg.get("method"), msg.get("id")
    if mid is None:
        continue  # a notification
    params = msg.get("params") or {}
    if method == "initialize":
        result = {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo", "version": "1.0.0"},
        }
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        name = params.get("name")
        if LOG:
            with open(LOG, "a", encoding="utf-8") as log:
                log.write(f"{name}\n")
        text = f"The secret word is {TOKEN}." if name == "secret_word" else "FORBIDDEN-TOOL-RAN"
        result = {"content": [{"type": "text", "text": text}]}
    elif method == "ping":
        result = {}
    else:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"no method {method}"}})
        continue
    send({"jsonrpc": "2.0", "id": mid, "result": result})
