# cord

A serverless peer to peer terminal messenger that talks over Tor v3 onion services. Each cord process hosts its own onion service internally. No central server.

Pre alpha. Expect rough edges, including occasional protocol-level changes that invalidate previously shared contact blobs.

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
- `/unpair <name-or-hex>` remove a contact entirely. Use when you want to start over.
- `/msg <name> <text>` send a text message to a verified contact.
- `/connect <name-or-hex>` dial a verified contact over Tor (or paste a raw `.onion` address for a debug connection).
- `/passphrase` set a passphrase to enable the encrypted offline queue.
- `/unlock` unlock the offline queue for the current session.
- `/quit` exit.
- Esc or Ctrl C exit.
- Enter submit.

Typed text without a leading slash echoes back locally. Use `/msg` to actually send.

## Pairing

Each cord user has an identity made of two pieces: a Tor v3 onion address and a Noise X25519 static public key. The contact blob bundles both, plus an optional display name and a checksum, into a single line of base64 prefixed with `cord1:`.

To pair with someone:

1. Both users run `/share` to print their blob.
2. Each sends their blob to the other through any channel they already trust.
3. Each runs `/pair <blob>` to add the other's blob. The status starts `pending`.
4. When the two cord instances first connect, both chat logs print a short authentication string of 18 digits in six groups of three (about 60 bits of entropy) and identify which pending contact it belongs to. Both users read it aloud on a separate channel.
5. If the codes match on both ends, each user runs `/verify <name-or-hex>` to upgrade the contact to `verified`. If they do not match, run `/reject <name-or-hex>` and start over; you are being intercepted.

In `/verify`, `/reject`, and `/unpair`, the argument can be the display name (case insensitive) or a hex prefix of the peer's Noise public key (at least 4 hex characters).

If you `/pair` a blob whose key matches a contact you previously rejected, cord reopens that entry as pending so you can retry verification. To wipe the entry instead, use `/unpair <name-or-hex>` first.

Contacts persist at `<config_dir>/contacts` with 0600 file mode.

## Messaging

Once two cord users have paired and verified each other, the `/msg` command sends UTF-8 text over the Noise channel.

    /msg alice hey, this works

Requirements:

- The contact must be `Verified`. Pending and rejected contacts cannot receive messages.
- On LAN a connection forms automatically once the peer is discovered through mDNS. Over Tor, someone runs `/connect <name-or-hex>` on one side.

Incoming messages appear in the chat log with the sender's name in bold. Outgoing messages echo back dimmed with a `you →` prefix and a delivery marker that updates in place:

- `sending…` while the message leaves your machine
- `sent` once it reaches the peer's connection
- `delivered ✓` in green once the peer acknowledges receipt
- `queued` if the recipient was offline and you chose to hold the message for later (see below)
- `failed ✗` in red if it could not be sent or queued

The chat history for the current session lives only in memory. cord keeps no readable message history on disk.

## Offline queue

If you `/msg` a verified contact who is not connected right now, cord asks whether to queue the message:

    alice is offline. queue "your message" for delivery on reconnect? (y / n)

Press `y` to hold it in an encrypted on disk queue. It shows as `queued`, and on the next connection to that contact it is resent, acknowledged, and flipped to `delivered ✓`. Press `n` or Esc to discard the message; it is never sent or stored.

The queue is encrypted at rest with a passphrase you choose, because it is the only place your message content ever touches disk. Before cord will queue anything, set a passphrase once:

    /passphrase

You type it twice to confirm. cord derives a key from it with Argon2id; that key wraps a random master key (sealed with XChaCha20 Poly1305) that in turn seals the queue files. The wrapped master key lives at `<config_dir>/queue.key` with 0600 file mode. The passphrase itself is never stored.

On a later run, if a queue already exists cord prompts you to unlock it so pending messages can resume:

    /unlock

The status bar shows the queue state: `off` (no passphrase set yet), `locked` (set but not unlocked this run), or `on` (unlocked and ready). If you forget the passphrase the queued messages cannot be recovered; delete `<config_dir>/queue.key` and the `<config_dir>/queue/` directory to start fresh.

Limits in this version:

- Delivery is at least once. If a connection drops after the peer received a message but before its acknowledgement reached you, the message is resent on the next connect and the peer may see it twice.
- A message sent in the brief moment before a silent disconnect is noticed can still be lost. cord tears a connection down as soon as the peer closes it, so this window is small; but a peer that vanishes without closing the connection (a yanked network cable) is not noticed until the next write fails.
- cord does not yet dial offline contacts on its own to flush the queue. Delivery happens the next time a connection forms through LAN discovery or `/connect`. Automatic background retry is the next milestone.

## Tests

    cargo test

## Platforms

Linux, macOS, Windows. CI builds and tests on all three.
