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

## Who is let in

One token of at least 32 characters, made by `--prepare` and sent as `Authorization: Bearer`. A request without it gets 401 with an RFC 6750
challenge; a browser Origin other than the host's own is refused. A command has all of the user's access, so the
token is the key to the machine.

`--attach <IPv4> <port>` is for a controller that already drives the session's desktop. It puts
`windows-tools-token:<token>` on the session's clipboard and starts the host, which takes the token, ends any other
host of the session, and replaces the token with `windows-tools-fingerprint:<hex SHA-256 of its certificate>` for
the controller to pin. Nothing is written to disk; the token and certificate end with the process.

`--prepare <new directory> <IPv4>` writes the token, a certificate for that address that signs itself, and
`connection.json`, in a directory only the user can read, and prints the certificate's SHA-256. A controller pins
that fingerprint, or the certificate it first sees.
