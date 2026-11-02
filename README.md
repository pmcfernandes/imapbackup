# imapbackup

Command-line utility for backing up IMAP folders to local mbox files. It compares message IDs on the server with those already in each
local mailbox and downloads messages that are not present, so regular runs
incrementally update the backup.

It retains the original folder mapping, mbox formatting, and `@password-file` convention,
while fixing the original overwrite-mode bug and supporting streamed mailbox
reading.

## Build

Install Rust and Cargo, then build the release executable:

```sh
cargo build --release
```

The executable is written to `target/release/imapbackup` on Unix-like systems
and `target/release/imapbackup.exe` on Windows.

## Quick start

Connect to an IMAP server over SSL and back up all folders. The example reads
the password from a file instead of putting it directly in the command:

```sh
./target/release/imapbackup --ssl --server=imap.example.com:993 \
  --user=user@example.com --pass=@~/.imap_pass
```

If `--pass` is omitted, the program prompts for the password without echoing
it. Password-file references begin with `@`; a leading `~` is expanded to the
user's home directory when available. The file is read as UTF-8 and surrounding
whitespace is trimmed. To pass a literal password beginning with `@`, prefix
it with `\`.

Mailbox files and any required subdirectories are created in the current
working directory. By default, folders are written as `.mbox` files; for
example, `INBOX.Sent` becomes `INBOX.Sent.mbox`.

## Options

| Option | Description |
| --- | --- |
| `-s HOST`, `--server=HOST` | IMAP server hostname, optionally followed by `:PORT`. Required. |
| `-u USER`, `--user=USER` | Username for IMAP login. Required. |
| `-p PASS`, `--pass=PASS` | Password, `@FILE` password-file reference, or omit to be prompted. |
| `-e`, `--ssl` | Connect using SSL/TLS. Default port is `993` with SSL and `143` otherwise. |
| `-t SECS`, `--timeout=SECS` | Socket timeout in seconds. Default: `60`. |
| `-f LIST`, `--folders=LIST` | Back up only the comma-separated IMAP folder names listed. |
| `-a`, `--append-to-mboxes` | Append new messages to existing mailbox files. This is the default. |
| `-y`, `--yes-overwrite-mboxes` | Replace existing mailbox files and download the folders again. |
| `-n`, `--compress=none` | Use uncompressed mbox files. This is the default. |
| `-z`, `--compress=gzip` | Write gzip-compressed mbox files (`.gz`). Appending can be slow. |
| `-b`, `--compress=bzip2` | Write bzip2-compressed mbox files (`.bz2`). Requires `-y`; appending is not supported. |
| `-k FILE`, `--keyfile=FILE` | PEM client private key for SSL. Specify together with a certificate. |
| `-c FILE`, `--certfile=FILE` | PEM client certificate chain for SSL. Specify together with a key. |
| `--thunderbird` | Use a Mozilla Thunderbird-compatible mailbox hierarchy. |
| `--nospinner` | Disable progress spinners for log-friendly output. |

Short options can be combined when they take no value (for example, `-ey`).
Options that require a value accept it either after a space or directly after
the short option, such as `-s imap.example.com` or `-simap.example.com`.

## Examples

Back up only the Inbox and Sent folders, using SSL and gzip compression:

```sh
imapbackup -e -s imap.example.com -u user@example.com \
  -p @~/.imap_pass -f INBOX,Sent -z
```

Overwrite existing mailboxes during a full re-download:

```sh
imapbackup --ssl --server=imap.example.com:993 --user=user@example.com \
  --pass=@~/.imap_pass --yes-overwrite-mboxes
```

For bzip2 output, include overwrite mode:

```sh
imapbackup -e -s imap.example.com -u user@example.com \
  -p @~/.imap_pass -b -y
```

On Windows, run `target\release\imapbackup.exe` (or use the executable's full
path) and provide the password-file path in a form accepted by your shell.

## Backup behavior and cautions

- Incremental runs skip messages whose `Message-Id` is already in the local
  mailbox. Messages without one are assigned a deterministic synthetic ID based
  on selected headers.
- `-y` is destructive to existing mailbox contents: existing files are
  replaced, not merged. Use it only when a fresh download is intended.
- Gzip files can be appended to, but doing so may be slow. Bzip2 files cannot
  be appended to; use `-y` to recreate them.
- SSL is opt-in. Use `-e` / `--ssl` when the server expects an encrypted
  connection.
- With `--thunderbird`, the output uses Thunderbird's `.sbd` folder hierarchy.

## Exit codes

| Code | Meaning |
| --- | --- |
| `2` | Invalid command-line options or configuration. |
| `3` | Server name lookup failed. |
| `4` | Socket, TLS, or local filesystem error. |
| `5` | IMAP protocol error. |

## Tests

Run the unit tests with:

```sh
cargo test
```
