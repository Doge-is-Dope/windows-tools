# Windows Tools

Lets a controller elsewhere on the network, such as an agent runtime or any MCP client, run PowerShell commands in a
logged-in Windows user session.

The host is one Rust executable serving HTTPS with one token. It offers two MCP tools, `windows_describe` and
`windows_exec_run`, and runs commands only while the controller holds a lease a few seconds long. When the lease
ends (heartbeats stop, the controller hands the session back, or the session locks), what is running is stopped.
See the [protocol](docs/protocol.md).

![Architecture](docs/architecture.svg)

## Layout

```text
rust/core/     Calls, the lease and dispatch: no transport and no Windows
rust/host/     The Windows session, job objects, PowerShell, MCP over HTTPS
tests/         A test client and the acceptance against a running host
scripts/       The installer, and Windows build and setup helpers
docs/          The protocol and the architecture diagram
```

## Install

As the Windows user whose desktop the controller drives, run
[install-windows-tools.cmd](scripts/install-windows-tools.cmd), optionally with the IPv4 address the controller
reaches (it asks when the computer has several). It downloads the release it names, checks its SHA-256, puts it in
`%LOCALAPPDATA%\Programs\WindowsTools\`, makes a token and a certificate once, starts the host with no window now and
at each sign-in, and prints one line for the controller:

```text
https://<token>@<IPv4>:8767/<certificate SHA-256>
```

Running it again keeps the token and certificate, so the line stays the same, and restarts the host from the
installed release. To install offline, put that release's `windows-tools-host.exe` from the
[releases](https://github.com/Doge-is-Dope/windows-tools/releases) beside the installer. No administrator is needed.
The host runs on 64-bit Windows 10 or later and is unsigned, so antivirus or SmartScreen may ask before it first runs,
and a firewall may need an administrator to allow port 8767 once.

The line is the key to the machine: run the installer while no controller watches the screen. To change the token,
delete `%LOCALAPPDATA%\WindowsTools\state` and run the installer again. To remove the host, end
`windows-tools-host.exe`, run `reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v "Windows Tools" /f`,
and delete `%LOCALAPPDATA%\Programs\WindowsTools` and `%LOCALAPPDATA%\WindowsTools`.

## Build

Builds only on Windows, with Rust 1.89+, MSVC and the Windows SDK. `scripts\setup-rust.ps1` installs them
under `tools\` (or `WT_TOOLS_ROOT`); missing system components need an administrator.

```powershell
powershell -NoProfile -File scripts\build-rust.ps1
```

checks formatting, runs the tests and Clippy, and builds into `artifacts\rust\debug\`; run that build with
`--install <IPv4>` to serve from it.

## How it runs

One host serves a session, and it starts with the human owning it: nothing runs until a controller takes the lease.
It listens on the installed address alone, and at sign-in waits for that address to come up. Signing out ends it; the
next sign-in starts it again.

Not included: a tray icon, certificate renewal, and desktop automation (the controller drives
the screen; commands reach scriptable apps).

## Tests

The core needs no Windows:

```sh
cargo test --manifest-path rust/Cargo.toml -p windows-tools-core --locked
```

The host's tests run in `scripts\build-rust.ps1`. The acceptance runs from any machine that reaches an idle,
human-owned host, takes about a minute and leaves it that way:

```sh
python3 tests/acceptance.py 'https://<token>@<IPv4>:8767/<certificate SHA-256>'
```

`tests/client.py` is the standard-library test client it uses, not an SDK.

## Secrets

Keep tokens, keys, certificates and logs in `.local/`, which Git ignores; `git add -f` still adds
them. Before sharing a commit, scan it and check that it reports a nonzero commit count:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/repo \
  -v "$PWD:/repo:ro" ghcr.io/gitleaks/gitleaks:v8.30.1 git /repo --redact --log-opts=<ref>
```

The scanner finds secrets, not usernames, paths, addresses or other machine details: read the diff for those.

## License

MIT; see [LICENSE](LICENSE).
