# Windows Tools

Windows Tools lets an agent on another machine run PowerShell commands on a Windows desktop as the signed-in user.
For finding, reading, moving and renaming files, or changing data in bulk, a command is faster and surer than the
mouse. The program that sends the commands is called the controller here.

What it does:

- **Runs a command.** One PowerShell command as the signed-in user. It waits for the command to end and returns its
  exit code and the end of what it printed. Each command has a deadline, and one runs at a time.
- **Stops when control changes hands.** Commands run only while the controller holds a lease a few seconds long. When
  the controller goes quiet, hands the desktop back, or the session locks, the running command is stopped.
- **Lets in one key.** One HTTPS address with one token and a pinned certificate, printed by the installer as a
  single line.
- **Installs once, without an administrator.** It starts with no window at each sign-in.
- **Relays browser tools, when installed.** Chrome DevTools MCP's tools, named `browser_*`, under the same lease.

It is one Rust executable that offers two MCP tools, `windows_describe` and `windows_exec_run`, and the browser tools
when they are installed.

![Architecture](docs/architecture.svg)

## Install

The installer prints the key to this computer, so first make sure no controller is watching the screen: no task
running and no live view open. Then, as the Windows user whose desktop the controller drives:

1. Run [install-windows-tools.cmd](scripts/install-windows-tools.cmd). To choose the address the controller reaches,
   pass it: `install-windows-tools.cmd <IPv4>`. Otherwise the installer uses the computer's one IPv4 address, or asks
   when there are several.
2. Give the line it prints to the controller:

   ```text
   https://<token>@<IPv4>:8767/<certificate SHA-256>
   ```

The installer downloads the release it names, checks its SHA-256 and puts it in `%LOCALAPPDATA%\Programs\WindowsTools\`.
It makes a token and a certificate once, then starts the host now and at each sign-in.

- **Offline:** put that release's `windows-tools-host.exe` from the
  [releases](https://github.com/Doge-is-Dope/windows-tools/releases) beside the installer.
- **Antivirus and firewall:** the host runs on 64-bit Windows 10 or later and is unsigned, so antivirus or
  SmartScreen may ask before it first runs. A firewall may need an administrator to allow port 8767 once.

### Update, move or reset

- **Update:** run the new release's installer. The token and certificate are kept, so the line stays the same.
- **New address:** run the installer again with the new IPv4. Only the address in the line changes.
- **New token:** delete `%LOCALAPPDATA%\WindowsTools\state` and run the installer again. The line changes, and the
  controller needs the new one.

### Remove

```bat
taskkill /F /IM windows-tools-host.exe
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v "Windows Tools" /f
rmdir /S /Q "%LOCALAPPDATA%\Programs\WindowsTools"
rmdir /S /Q "%LOCALAPPDATA%\WindowsTools"
```

### Browser tools

Optional. Run `scripts\install-browser-tools.ps1` as the same user: it puts pinned, hash-checked Node and
[Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp) beside the host, without npm. Install
Google Chrome separately, then restart the host. The server starts at the first browser call and opens Chrome in a
profile of its own, without the user's tabs or sign-ins. A call stopped by the lease or its 60-second deadline ends
the server and that Chrome with it; the next call starts them again.

## Connect a controller

The line is not enough for a plain MCP client: commands run only under a lease, which the controller holds on a route
of its own. With the token as `Authorization: Bearer` and the certificate checked against the line's SHA-256, the
controller:

1. Calls the `windows_describe` tool on `/mcp` to get the session's target, then `control.resume` with it on
   `/connector/control` to take the lease.
2. Sends `control.heartbeat` on `/connector/control` every second or two. The lease ends 5 seconds after the last one.
3. Calls the MCP tools on `/mcp` while it holds the lease.
4. Sends `control.takeover` to hand the desktop back.

The [protocol](docs/protocol.md) has the calls, their errors and what `windows_exec_run` returns.

## How it runs

One host serves one session. It starts with the user in control, so nothing runs until a controller takes the lease.
It listens on the installed address alone, and at sign-in waits for that address to come up. Signing out ends it,
and the next sign-in starts it again.

Not included: a tray icon, certificate renewal, and desktop automation. The controller drives the screen, and
commands reach scriptable apps.

## Build

Builds only on Windows, with Rust 1.89+, MSVC and the Windows SDK. `scripts\setup-rust.ps1` installs them under
`tools\` (or `WT_TOOLS_ROOT`); missing system components need an administrator. `build-rust.ps1` checks formatting,
runs the tests and Clippy, and builds into `artifacts\rust\debug\`:

```powershell
powershell -NoProfile -File scripts\build-rust.ps1
```

Run that build with `--install <IPv4>` to serve from it.

## Tests

The core needs no Windows:

```sh
cargo test --manifest-path rust/Cargo.toml -p windows-tools-core --locked
```

The host's tests run in `scripts\build-rust.ps1`. The acceptance runs from any machine that reaches a host the user is
in control of. It takes about a minute and hands control back to the user at the end:

```sh
python3 tests/acceptance.py 'https://<token>@<IPv4>:8767/<certificate SHA-256>'
```

`tests/client.py` is the standard-library test client it uses, not an SDK.

## Layout

```text
rust/core/     Calls, the lease, dispatch and the relay: no transport and no Windows
rust/host/     The Windows session, job objects, PowerShell, MCP over HTTPS
tests/         A test client and the acceptance against a running host
scripts/       The installers, and Windows build and setup helpers
docs/          The protocol and the architecture diagram
```

## Secrets

Keep tokens, keys, certificates and logs in `.local/`, which Git ignores; `git add -f` still adds them. Before sharing
a commit, scan it:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/repo \
  -v "$PWD:/repo:ro" ghcr.io/gitleaks/gitleaks:v8.30.1 git /repo --redact --log-opts=<ref>
```

Check that it reports a nonzero commit count: zero means `<ref>` matched nothing and nothing was scanned. The scanner
finds secrets, not usernames, paths, addresses or other machine details: read the diff for those.

## License

MIT; see [LICENSE](LICENSE).
