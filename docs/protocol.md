# Protocol

The host serves one Windows session over HTTPS. A controller reaches it on two routes with one bearer token:
`/mcp` for the tools a model may call, and `/connector/control` for the lease, which a model never sees.

## Methods

`describe` lists them in `capabilities`.

| Method | MCP tool | What it does |
| --- | --- | --- |
| `describe` | `windows_describe` | The session's identity, who holds it, and what the host can do. Grants nothing |
| `control.resume` | none | Takes the lease for the described target. Joining a lease already held keeps it as it is |
| `control.heartbeat` | none | Renews the lease |
| `control.takeover` | none | Hands the session back to the human and stops what is running |
| `exec.run` | `windows_exec_run` | Runs one PowerShell command to its end and returns how it ended and what it printed |
| none | `browser_*` | Relayed to Chrome DevTools MCP, when installed (see below) |

To add a tool: give `Toolset` a method for it, list it in `rust/host/src/mcp/catalog.rs` with its schema, and call
it from the handler in `rust/host/src/mcp/handler.rs`.

## Target and lease

`describe` returns a `target`: `hostInstance`, new each time a host starts, and `controlEpoch`. Every other call
names a target, and one naming another instance or epoch is refused with `stale_session`.

A host starts with the human owning the session. `control.resume` gives the controller a lease that lasts
5 seconds unless `control.heartbeat` renews it. The lease ends, and whatever is running is
stopped, when heartbeats stop, on `control.takeover`, or when the session is locked or disconnected; each of these
starts a new epoch, and `describe` reports why in `lastRevocation`. Nothing resumes by itself: the controller
describes the session again and resumes.

## Calls

A lease call is a POST to `/connector/control`:

```json
{"version": 1, "id": "any-id", "method": "control.heartbeat", "target": {"hostInstance": "h", "controlEpoch": 4}, "args": {}}
```

and answers `{"id", "result", "error"}`. A tool call is MCP `tools/call` on `/mcp` (Streamable HTTP, JSON
responses), with the target in `_meta["windows-tools/target"]`; a call without one is made for the current
target. The same envelope comes back as
`structuredContent`, with `isError` set when `error` is.

An error carries `code`, `message` and `delivery`. `not_sent` means the call surely did not run. `unknown` means
it may have: nothing is replayed, so the caller looks at what happened before it tries again.

## exec.run

Arguments: `command` (PowerShell, up to 32,000 UTF-16 units) and `timeoutMs` (1,000 to 600,000, default 30,000).
Each command runs in a new Windows PowerShell process as the session's user, from the profile directory, inside a
job object. It is stopped with everything it started at the deadline, when the lease ends, or when the caller closes
the HTTP response. A command that ends on its own leaves running what it started, such as a program opened with
`Start-Process`; output such a program writes to the command's own console is read for at most a second more.

Result: `state` (`succeeded`, `failed`, `timed_out` or `cancelled`), `exitCode`, `durationMs`, `output` (stdout and
stderr in the order they arrived, the last 64 KiB), `outputTruncated`, and `error`: why the host could not run it,
with `exitCode` null, or null. The exit code is that of
`powershell -Command` for the same text, except that a program's own code is kept: non-zero only when the last
statement failed. One command runs at a time; a second waits for the first to end.

## Browser tools

When installed, `tools/list` adds Chrome DevTools MCP's tools with `browser_` before each name, their schemas as the
server lists them. A call runs like `exec.run`: one action at a time, only under the lease, its target in `_meta`.
`result` is the server's own `tools/call` result (`content`, `isError`). A call the server refuses comes back
`invalid_arguments`, `not_sent`. A call stopped when the lease ends or the caller leaves (`cancelled`), after 60
seconds (`timed_out`), or whose server exits (`exited`) comes back `unknown`; the server and its Chrome are ended,
and the next call starts them again.

## Who is let in

One token of at least 32 characters, made by `--install` and sent as `Authorization: Bearer`. A request without it
gets 401 with an RFC 6750 challenge. A browser Origin other than the host's own is refused, and so is a request that
names another host than the address given to `--install`. A command has all of the user's access, so the token is the
key to the machine.

`--install <IPv4>` makes the token and a certificate for that address that signs itself, once, in
`%LOCALAPPDATA%\WindowsTools\state`, which only the user can read; a later run keeps them. It prints one line for the
controller, `https://<token>@<IPv4>:8767/<hex SHA-256 of the certificate>`, and the controller pins that fingerprint.
