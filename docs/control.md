# Control API: ctl and mcp

Every user action is one entry in the action manifest (`crates/core/src/action.rs`, generated to
`config/actions.json`). The running app (`oats-meetings-rs`, or `oats-meetings-rs --headless`)
serves it on a control socket in its runtime dir.

## ctl

`oats-meetings-rs ctl <action words> [params]` sends one action: `ctl list-actions` lists them,
`ctl config set app.menubar true` is `config-set`. Exit codes: 0 ok, 1 error response, 2 usage,
3 engine not running. `--runtime-dir D` or `--socket S` picks another engine.

## mcp

`oats-meetings-rs mcp` is an MCP server on stdio (JSON-RPC 2.0, one message per line, protocol
2025-06-18: `initialize`, `ping`, `tools/list`, `tools/call`). Each action is one tool, generated
from the manifest (the list is `config/mcp-tools.json`):

- name: the action name with `-` as `_` (`session-list` -> `session_list`);
- description: the manifest doc;
- inputSchema: the action's params (an `Option` param is not required);
- annotations: `readOnlyHint` for read actions, `destructiveHint` for deletes and removes
  (`session_delete`, `hook_remove`, `trash_empty`, ...).

`tools/call` sends the action to the running app over the same socket ctl uses, one connection per
call, and returns its JSON result as text content. An error response, or the app not running
(`error[not_running]: cannot connect to ...`), is a result with `isError: true`. `subscribe` returns
only its ack: tools do not stream events. Options: `--runtime-dir D`, `--socket S`.

A client config entry (Claude Code `.mcp.json`, Claude Desktop and others take the same shape):

```json
{"mcpServers": {"oats": {"command": "oats-meetings-rs", "args": ["mcp"]}}}

The Settings section **API & MCP** controls `api.enabled`, `mcp.enabled`, `mcp.read_only`, and
`mcp.http.*`. Disabling the API closes the control socket; disabling MCP makes `mcp` exit with a
clear error. Read-only mode rejects MCP write and control tools. MCP HTTP is off by default; when
enabled it serves the same tools at `http://127.0.0.1:47960/mcp` using Streamable HTTP. Change
`mcp.http.bind` to listen on another interface. `ctl api-info` reports the current socket,
stdio command, HTTP URL, and client JSON snippet.

```toml
[api]
enabled = true

[mcp]
enabled = true
read_only = false

[mcp.http]
enabled = false
port = 47960
bind = "127.0.0.1"
```
```

Smoke test:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"sh","version":"0"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | oats-meetings-rs mcp
```
