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
scripts/       Windows build, setup and start helpers
docs/          The protocol and the architecture diagram
```

## Install

Download `windows-tools-host.exe` from the [latest release](https://github.com/Doge-is-Dope/windows-tools/releases/latest)
and copy it to the session user's `%LOCALAPPDATA%\Programs\WindowsTools\`; no administrator needed. It runs on
64-bit Windows 10 or later. It is not signed, so antivirus or SmartScreen may ask before it first runs.

## Build

Builds only on Windows, with Rust 1.89+, MSVC and the Windows SDK. `scripts\setup-rust.ps1` installs them
under `tools\` (or `WT_TOOLS_ROOT`); missing system components need an administrator.

```powershell
powershell -NoProfile -File scripts\build-rust.ps1
```

checks formatting, runs the tests and Clippy, and builds into `artifacts\rust\debug\`; install it as above.

## Start

In the target user's logged-in, unlocked session:

```text
windows-tools-host.exe --prepare C:\path\state <local-IPv4>
windows-tools-host.exe --serve C:\path\state\connection.json
```

`--prepare` writes a token, a certificate for that address and `connection.json` (port 8767) into a new directory
only that user can read, and prints the certificate's SHA-256. Give the controller the token and that fingerprint
to pin. To run without a window, prefix the command with `conhost.exe --headless`.

A controller that already drives the desktop can skip `--prepare` and start the host with `--attach`, handing the
token over the clipboard; see [who is let in](docs/protocol.md#who-is-let-in).

One host serves a session, and it starts with the human owning it: nothing runs until a controller takes the lease.

Not included: starting at logon, a tray icon, certificate renewal, and desktop automation (the controller drives
the screen; commands reach scriptable apps).

## Tests

The core needs no Windows:

```sh
cargo test --manifest-path rust/Cargo.toml -p windows-tools-core --locked
```

The host's tests run in `scripts\build-rust.ps1`. The acceptance runs from any machine that reaches an idle,
human-owned host, takes about a minute and leaves it that way:

```sh
python3 tests/acceptance.py --url https://<IP>:8767 --token-file tools.token --fingerprint <SHA-256>
```

`tests/client.py` is the standard-library test client it uses, not an SDK.

## Secrets

Keep tokens, keys, certificates, `connection.json` and logs in `.local/`, which Git ignores; `git add -f` still adds
them. Before sharing a commit, scan it and check that it reports a nonzero commit count:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/repo \
  -v "$PWD:/repo:ro" ghcr.io/gitleaks/gitleaks:v8.30.1 git /repo --redact --log-opts=<ref>
```

The scanner finds secrets, not usernames, paths, addresses or other machine details: read the diff for those.

## License

MIT; see [LICENSE](LICENSE).
