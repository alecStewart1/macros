---
title: MCP server
description: Let an AI assistant such as Claude Code evaluate Steel and read buffers in the running editor, for debugging and package development.
---

# MCP server

Macros can run a [Model Context Protocol](https://modelcontextprotocol.io) server so an AI assistant can work against the **live editor**: evaluate Steel, redefine a package's functions without restarting, and read back buffers, messages, and documentation. It's meant for debugging and package development.

The server is **off by default**.

## Turning it on

For one session, run <kbd>M-x</kbd> `mcp-server-start` (and `mcp-server-stop` to turn it off). To start it at launch, add this to `init.scm`:

```scheme
(set-option "mcp-server" #t)
```

| Option | Default | Meaning |
|---|---|---|
| `mcp-server` | `#f` | Run the server |
| `mcp-server-port` | `7811` | Port on 127.0.0.1 |
| `mcp-server-token` | (generated) | Bearer token clients must send |

Changing any of these restarts the server. Without `mcp-server-token`, a random 256-bit token is generated on first start and saved in `mcp-token` in your config directory, so a client's saved configuration keeps working across restarts. On macOS and Linux the file is set to be readable only by you each time it's used; on Windows it has your config folder's permissions. If you set your own token, make it long and random (32 or more characters): any program on your machine can try tokens against the port.

## Connecting Claude Code

Run <kbd>M-x</kbd> `mcp-server-info`. It shows the server's status, the exact command to register it (token included), and a log of recent requests:

```bash
claude mcp add --transport http macros http://127.0.0.1:7811/mcp --header "Authorization: Bearer <token>"
```

## Tools

| Tool | What it does |
|---|---|
| `eval` | Evaluates Steel code in the editor and returns the value, any error, and echo-area messages. Editor commands it queues (insert, render a buffer, set an option, …) are applied. |
| `buffer_text` | A buffer's text with its name, file, major mode, point, and modified flag. Takes an optional line range and can include overlay face spans. |
| `list_buffers` | Every open buffer with its mode and file. |
| `messages` | Recent `*Messages*` lines, where errors from hooks and async callbacks end up. |
| `describe` | Documentation for a function, command, or option. |
| `api` | The package-API reference. With no arguments, an index of every primitive category (buffers, overlays, processes, http, sockets, sqlite, git, …) and bundled Scheme library module. With `category`, each function's signature, argument and return types, and doc. With `search`, matches across names, signatures, and docs. |
| `apropos` | Searches names and documentation. |

Before writing a package, have the agent call `api` to see what it can build on. The reference is generated from the running editor, so it always matches the build you're connected to. The same text is available from Scheme as `(api-reference category search)`.

A typical package-development loop: edit your package file, `eval` the changed definitions (or `(load "…/my-package.scm")`), run the command, then check the result with `buffer_text` and `messages`.

`eval` runs on the Scheme engine thread, the same one that runs commands, hooks, and keymaps. The editor keeps drawing and you can keep typing, but anything that needs Scheme waits until the evaluation finishes. An evaluation that never finishes (an infinite loop) leaves Scheme stuck until you restart the editor, so test risky code in small steps.

## Security

`eval` can do anything your editor can, including running programs and reading files, so the server is locked down:

- It listens only on 127.0.0.1.
- Every request must carry the bearer token.
- Requests with a non-local `Host` or `Origin` header are refused, so a web page open in your browser can't reach it.
- Nothing is read past the request headers until the token checks out, and it serves at most 16 connections at once, so a misbehaving local program can't exhaust the editor's memory.

Treat the token like a password and only turn the server on while you're using it. On a computer other people also log in to, keep in mind that a configured client sends the token to whatever is listening on that port; if the server can't start because the port is taken, find out what holds it before reconnecting.
