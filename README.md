# cord

A serverless peer to peer terminal messenger that talks over Tor v3 onion services. Each cord process hosts its own onion service internally. No central server.

Pre alpha. Pairing and message framing are not implemented yet.

## Requirements

- Rust 1.89 or newer
- A working network connection for Tor

## Build

    cargo build --release

The first build pulls a large dependency tree from arti. Expect a couple of minutes.

## Run

    cargo run --release

To run two instances side by side for local testing, give each its own config directory:

    cargo run --release -- --config-dir /tmp/cord-a
    cargo run --release -- --config-dir /tmp/cord-b

## First run

1. cord writes its identity to the config directory.
2. The TUI shows `status: bootstrapping…`.
3. The LAN listener binds and mDNS announces. Another cord on the same network appears in the chat log within two seconds.
4. arti bootstraps Tor. Takes 10 to 30 seconds on a cold start.
5. The TUI status line shows your `.onion` address. Same address every run.

## Using the TUI

The screen has four regions: a status bar, the chat log, your input line, and a key hint footer.

Type `/help` and press Enter to list every command. Quick reference:

- `/help` show commands
- `/connect <address>` dial a peer over Tor (debug only, goes away when pairing lands)
- `/quit` exit
- Esc or Ctrl C exit
- Enter submit

Typed text without a leading slash echoes back. Messaging is not implemented yet.

## Tests

    cargo test

## Platforms

Linux, macOS, Windows. CI builds and tests on all three.
