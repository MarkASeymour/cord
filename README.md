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

- `/share [name]` print your own contact blob to copy and send to a peer. Optional display name.
- `/pair <blob>` add a peer's contact blob to your contacts (status starts pending).
- `/contacts` list paired contacts and their status.
- `/verify <name-or-hex>` upgrade a pending contact to verified, after you have compared the SAS aloud.
- `/reject <name-or-hex>` mark a contact as rejected.
- `/connect <address>` dial a peer over Tor (debug only).
- `/quit` exit.
- Esc or Ctrl C exit.
- Enter submit.

Typed text without a leading slash echoes back. Messaging is not implemented yet.

## Pairing

Each cord user has an identity made of two pieces: a Tor v3 onion address and a Noise X25519 static public key. The contact blob bundles both, plus an optional display name and a checksum, into a single line of base64 prefixed with `cord1:`.

To pair with someone:

1. Both users run `/share` to print their blob.
2. Each sends their blob to the other through any channel they already trust.
3. Each runs `/pair <blob>` to add the other's blob. The status starts `pending`.
4. When the two cord instances first connect, both chat logs print a short authentication string of 18 digits in six groups of three (about 60 bits of entropy) and identify which pending contact it belongs to. Both users read it aloud on a separate channel.
5. If the codes match on both ends, each user runs `/verify <name-or-hex>` to upgrade the contact to `verified`. If they do not match, run `/reject <name-or-hex>` and start over; you are being intercepted.

Contacts persist at `<config_dir>/contacts` with 0600 file mode.

## Tests

    cargo test

## Platforms

Linux, macOS, Windows. CI builds and tests on all three.
