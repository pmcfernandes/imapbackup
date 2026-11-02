use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use chrono::Local;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::cli::{Compression, Config};
use crate::error::BackupError;
use crate::imap_client::ImapConnection;
use crate::parser::{extract_message_id, UUID};
use crate::spinner::{pretty_byte_count, Spinner};

static FROM_QUOTE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\n(>*)From ").expect("valid FROM_QUOTE_RE"));

/// Creates parent directories on disk for nested mailbox paths (e.g. Thunderbird `.sbd/` directories).
pub fn create_folder_structure(names: &[(String, String)]) -> io::Result<()> {
    let mut sorted = names.to_vec();
    sorted.sort();

    for (_imap_folder, filename) in sorted {
        if let Some(parent) = Path::new(&filename).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
    }
    Ok(())
}

/// Quotes body lines beginning with `From ` according to either Thunderbird's space-prefix rule
/// or RFC 4155 / qmail `>From ` quoting.
pub fn quote_mbox_body(raw_rfc822: &[u8], thunderbird: bool) -> String {
    let text = String::from_utf8_lossy(raw_rfc822);
    let cleaned = text.trim().replace('\r', "");
    if thunderbird {
        cleaned.replace("\nFrom ", "\n From ")
    } else {
        FROM_QUOTE_RE
            .replace_all(&cleaned, "\n>${1}From ")
            .into_owned()
    }
}

/// Formats the current local timestamp in ISO C `ctime(3)` English format (`Mon Nov  2 17:28:45 2026`),
/// as required by RFC 4155 for `From nobody ...` mbox delimiter lines.
pub fn format_ctime_envelope() -> String {
    let now = Local::now();
    format!("From nobody {}\n", now.format("%a %b %e %H:%M:%S %Y"))
}

/// Scans an existing local `.mbox`, `.mbox.gz`, or `.mbox.bz2` file and returns the set of `Message-Id`s
/// already stored on disk.
pub fn scan_file(
    filename: &str,
    compress: Compression,
    overwrite: bool,
    nospinner: bool,
) -> Result<HashSet<String>, BackupError> {
    if overwrite {
        return Ok(HashSet::new());
    }
    debug_assert_ne!(compress, Compression::Bzip2);

    let path = Path::new(filename);
    if !path.exists() {
        println!("File {}: not found", filename);
        return Ok(HashSet::new());
    }

    let mut spinner = Spinner::new(format!("File {}", filename), nospinner);
    let file = File::open(path)?;

    let reader: Box<dyn BufRead> = match compress {
        Compression::None => Box::new(BufReader::new(file)),
        Compression::Gzip => Box::new(BufReader::new(MultiGzDecoder::new(file))),
        Compression::Bzip2 => Box::new(BufReader::new(BzDecoder::new(file))),
    };

    let messages = parse_mbox_message_ids(reader, filename, &mut spinner)?;

    spinner.stop();
    println!(": {} messages", messages.len());
    Ok(messages)
}

/// Streaming Unix `mbox` reader that extracts `Message-Id` headers without loading multi-GB mailboxes into RAM.
pub fn parse_mbox_message_ids<R: BufRead>(
    mut reader: R,
    filename: &str,
    spinner: &mut Spinner,
) -> io::Result<HashSet<String>> {
    let mut messages = HashSet::new();
    let mut line = String::new();

    let mut in_headers = false;
    let mut prev_line_blank = true;
    let mut msg_index: usize = 0;
    let mut current_msgid_raw: Option<String> = None;
    let mut collecting_continuation = false;

    let finalize_message = |idx: usize,
                            raw_hdr: Option<String>,
                            seen: &mut HashSet<String>,
                            spin: &mut Spinner| {
        match raw_hdr {
            None => {
                println!();
                println!(
                    "WARNING: Message #{} in {} has no Message-Id header.",
                    idx, filename
                );
            }
            Some(hdr) => match extract_message_id(&hdr) {
                Some(msg_id) => {
                    seen.insert(msg_id);
                }
                None => {
                    println!();
                    println!(
                        "WARNING: Message #{} in {} has a malformed Message-Id header.",
                        idx, filename
                    );
                }
            },
        }
        spin.spin();
    };

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }

        let trimmed_crlf = line.trim_end_matches(&['\r', '\n'][..]);

        if prev_line_blank && trimmed_crlf.starts_with("From ") {
            if msg_index > 0 {
                finalize_message(
                    msg_index - 1,
                    current_msgid_raw.take(),
                    &mut messages,
                    spinner,
                );
            }
            msg_index += 1;
            in_headers = true;
            collecting_continuation = false;
            prev_line_blank = false;
            continue;
        }

        if in_headers {
            if trimmed_crlf.is_empty() {
                in_headers = false;
                collecting_continuation = false;
                prev_line_blank = true;
            } else if trimmed_crlf.starts_with(' ') || trimmed_crlf.starts_with('\t') {
                if collecting_continuation {
                    if let Some(ref mut existing) = current_msgid_raw {
                        existing.push(' ');
                        existing.push_str(trimmed_crlf.trim());
                    }
                }
                prev_line_blank = false;
            } else if trimmed_crlf.len() >= 11
                && trimmed_crlf[..11].eq_ignore_ascii_case("message-id:")
            {
                if current_msgid_raw.is_none() {
                    current_msgid_raw = Some(trimmed_crlf.to_string());
                    collecting_continuation = true;
                } else {
                    collecting_continuation = false;
                }
                prev_line_blank = false;
            } else {
                collecting_continuation = false;
                prev_line_blank = false;
            }
        } else {
            prev_line_blank = trimmed_crlf.is_empty();
        }
    }

    if msg_index > 0 {
        finalize_message(
            msg_index - 1,
            current_msgid_raw.take(),
            &mut messages,
            spinner,
        );
    }

    Ok(messages)
}

enum MboxWriter {
    Plain(BufWriter<File>),
    Gzip(GzEncoder<BufWriter<File>>),
    Bzip2(BzEncoder<BufWriter<File>>),
}

impl Write for MboxWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            MboxWriter::Plain(w) => w.write(buf),
            MboxWriter::Gzip(w) => w.write(buf),
            MboxWriter::Bzip2(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            MboxWriter::Plain(w) => w.flush(),
            MboxWriter::Gzip(w) => w.flush(),
            MboxWriter::Bzip2(w) => w.flush(),
        }
    }
}

impl MboxWriter {
    fn finish(self) -> io::Result<()> {
        match self {
            MboxWriter::Plain(mut w) => w.flush(),
            MboxWriter::Gzip(w) => {
                let mut inner = w.finish()?;
                inner.flush()
            }
            MboxWriter::Bzip2(w) => {
                let mut inner = w.finish()?;
                inner.flush()
            }
        }
    }
}

/// Downloads missing messages from the currently selected IMAP folder and appends/writes them to the local mbox.
pub fn download_messages<S: Read + Write>(
    server: &mut ImapConnection<S>,
    filename: &str,
    messages: &IndexMap<String, u32>,
    config: &Config,
) -> Result<(), BackupError> {
    let path = Path::new(filename);

    if config.overwrite && path.exists() {
        println!("Deleting {}", filename);
        fs::remove_file(path)?;
    } else if !config.overwrite {
        debug_assert_ne!(config.compress, Compression::Bzip2);
    }

    if messages.is_empty() {
        println!("New messages: 0");
        return Ok(());
    }

    let file = if config.compress == Compression::Bzip2 || config.overwrite {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?
    } else {
        OpenOptions::new().create(true).append(true).open(path)?
    };

    let buf_file = BufWriter::new(file);
    let mut mbox = match config.compress {
        Compression::None => MboxWriter::Plain(buf_file),
        Compression::Gzip => MboxWriter::Gzip(GzEncoder::new(buf_file, flate2::Compression::best())),
        Compression::Bzip2 => {
            MboxWriter::Bzip2(BzEncoder::new(buf_file, bzip2::Compression::best()))
        }
    };

    let mut spinner = Spinner::new(
        format!("Downloading {} new messages to {}", messages.len(), filename),
        config.nospinner,
    );

    let mut total: u64 = 0;
    let mut biggest: u64 = 0;

    for (msg_id, &seq_num) in messages {
        let mut envelope = format_ctime_envelope();
        if msg_id.contains(UUID) {
            envelope.push_str(&format!("Message-Id: {}\n", msg_id));
        }
        mbox.write_all(envelope.as_bytes())?;

        let raw_msg = server.fetch_rfc822(seq_num)?;
        let body = quote_mbox_body(&raw_msg, config.thunderbird);

        mbox.write_all(body.as_bytes())?;
        mbox.write_all(b"\n\n")?;

        let size = body.len() as u64;
        biggest = biggest.max(size);
        total += size;

        spinner.spin();
    }

    mbox.finish()?;
    spinner.stop();
    println!(
        ": {} total, {} for largest message",
        pretty_byte_count(total),
        pretty_byte_count(biggest)
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn quotes_from_lines_for_rfc4155_and_thunderbird() {
        let sample = b"Subject: Hello\r\n\r\nParagraph 1\r\nFrom someone in body\r\n>From already quoted\r\n";
        let rfc_quoted = quote_mbox_body(sample, false);
        assert!(rfc_quoted.contains("\n>From someone in body"));
        assert!(rfc_quoted.contains("\n>>From already quoted"));

        let tb_quoted = quote_mbox_body(sample, true);
        assert!(tb_quoted.contains("\n From someone in body"));
    }

    #[test]
    fn scans_unix_mbox_stream_including_folded_headers() {
        let mbox_data = "\
From nobody Mon Nov  2 09:00:00 2026
From: alice@example.com
Message-Id:
 <first-msg@example.com>
Subject: One

Hello world!
>From line inside body should not split message

From nobody Mon Nov  2 09:05:00 2026
From: bob@example.com
Message-ID: <second-msg@example.com>

Second body.

";
        let mut spinner = Spinner::new("Test", true);
        let ids = parse_mbox_message_ids(Cursor::new(mbox_data), "test.mbox", &mut spinner).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("<first-msg@example.com>"));
        assert!(ids.contains("<second-msg@example.com>"));
    }
}
