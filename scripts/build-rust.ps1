$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
$tools = if ($env:WT_TOOLS_ROOT) { $env:WT_TOOLS_ROOT } else { Join-Path $root 'tools' }
$env:CARGO_HOME=Join-Path $tools 'cargo'
$env:RUSTUP_HOME=Join-Path $tools 'rustup'
$env:CARGO_TARGET_DIR=Join-Path $root 'artifacts\rust'
$env:PATH=(Join-Path $env:CARGO_HOME 'bin')+';'+$env:PATH
$manifest=Join-Path $root 'rust\Cargo.toml'
$cargo=Join-Path $env:CARGO_HOME 'bin\cargo.exe'
& $cargo fmt --manifest-path $manifest --all -- --check
if($LASTEXITCODE -ne 0){throw 'Rust format failed.'}
& $cargo test --manifest-path $manifest --workspace --locked
if($LASTEXITCODE -ne 0){throw 'Rust tests failed.'}
& $cargo clippy --manifest-path $manifest --workspace --locked --all-targets -- -D warnings
if($LASTEXITCODE -ne 0){throw 'Rust clippy failed.'}
& $cargo build --manifest-path $manifest --workspace --locked
if($LASTEXITCODE -ne 0){throw 'Rust build failed.'}
