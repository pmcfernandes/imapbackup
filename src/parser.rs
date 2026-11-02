use once_cell::sync::Lazy;
use regex::Regex;
use sha1::{Digest, Sha1};

use crate::cli::Compression;

/// Constant UUID namespace when synthesizing missing Message-Ids.
pub const UUID: &str = "19AF1258-1AAF-44EF-9D9A-731079D6FAD7";

pub static MSGID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?im)^Message-Id:\s*(.+)").expect("valid MSGID_RE"));

pub static BLANKS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\s+").expect("valid BLANKS_RE"));

static NAME_ATTRIB_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(\\[a-zA-Z0-9_]+)\s*").expect("valid NAME_ATTRIB_RE"));

static STRING_LIST_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\"([^\"]*)\"|(\S+)"#).expect("valid STRING_LIST_RE"));

/// Represents an attribute inside a parenthesized IMAP `LIST` attribute list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListAttribute {
    Flag(String),
    Nested(Vec<ListAttribute>),
}

/// Parsed representation of an RFC 3501 `LIST` response line:
/// `(\HasNoChildren) "." "INBOX.Sent"`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedList {
    pub attributes: Vec<ListAttribute>,
    pub delimiter: String,
    pub mailbox: String,
}

/// Recursive descent parser for parenthesized attribute lists at the start of an IMAP `LIST` line.
/// Safe port of `parse_paren_list(row)` without panicking `assert` calls.
pub fn parse_paren_list(input: &str) -> Result<(Vec<ListAttribute>, &str), String> {
    let mut row = input.trim_start();
    if !row.starts_with('(') {
        return Err(format!("Expected '(' at start of LIST attributes, got: {}", row));
    }
    row = &row[1..];

    let mut result = Vec::new();

    loop {
        row = row.trim_start();
        if row.is_empty() {
            return Err("Unterminated '(' in LIST response".to_string());
        }
        if let Some(rest) = row.strip_prefix(')') {
            return Ok((result, rest));
        }
        if row.starts_with('(') {
            let (nested, rest) = parse_paren_list(row)?;
            result.push(ListAttribute::Nested(nested));
            row = rest;
        } else if let Some(caps) = NAME_ATTRIB_RE.captures(row) {
            let full = caps.get(0).unwrap();
            let flag = caps.get(1).unwrap().as_str().trim().to_string();
            result.push(ListAttribute::Flag(flag));
            row = &row[full.end()..];
        } else {
            // Gracefully consume non-standard atom flags (e.g. `\HasChildren` variants with hyphens)
            let end = row
                .find(|c: char| c.is_whitespace() || c == ')' || c == '(')
                .unwrap_or(row.len());
            if end == 0 {
                return Err(format!("Unexpected token in LIST attributes: {}", row));
            }
            result.push(ListAttribute::Flag(row[..end].to_string()));
            row = &row[end..];
        }
    }
}

/// Parses quoted and unquoted strings following the attribute list in a `LIST` response.
/// Mirrors `parse_string_list(row)` while supporting empty quoted root `""`.
pub fn parse_string_list(row: &str) -> Vec<String> {
    STRING_LIST_RE
        .captures_iter(row)
        .filter_map(|cap| {
            cap.get(1)
                .map(|m| m.as_str().to_string())
                .or_else(|| cap.get(2).map(|m| m.as_str().to_string()))
        })
        .collect()
}

/// Parses a single `LIST` response body (without leading `* LIST `) into attributes, delimiter, and folder name.
pub fn parse_list(raw_row: &str) -> Result<ParsedList, String> {
    let row = raw_row
        .trim()
        .strip_prefix("* LIST ")
        .unwrap_or_else(|| raw_row.trim());

    let (attributes, rest) = parse_paren_list(row)?;
    let strings = parse_string_list(rest);
    if strings.len() != 2 {
        return Err(format!(
            "Expected [delimiter, mailbox] in LIST response, got {:?} from '{}'",
            strings, raw_row
        ));
    }

    Ok(ParsedList {
        attributes,
        delimiter: strings[0].clone(),
        mailbox: strings[1].clone(),
    })
}

/// Maps an IMAP folder name + hierarchy delimiter to a local relative file path.
/// Mirrors `get_names` filename generation for both standard `.mbox` and `--thunderbird` `.sbd/` layouts.
pub fn folder_to_filename(
    foldername: &str,
    delim: &str,
    compress: Compression,
    thunderbird: bool,
) -> String {
    let suffix = compress.suffix();
    let parts: Vec<&str> = if delim.is_empty() {
        vec![foldername]
    } else {
        foldername.split(delim).collect()
    };

    if thunderbird {
        let mut filename = format!("{}{}", parts.join(".sbd/"), suffix);
        if filename.starts_with("INBOX") {
            filename = filename.replacen("INBOX", "Inbox", 1);
        }
        filename
    } else {
        format!("{}.mbox{}", parts.join("."), suffix)
    }
}

/// Collapses folded header whitespace (an Exchange quirk) and extracts the `Message-Id` value.
pub fn extract_message_id(raw_header: &str) -> Option<String> {
    let trimmed = raw_header.trim();
    if trimmed.is_empty() {
        return None;
    }
    let collapsed = BLANKS_RE.replace_all(trimmed, " ");
    MSGID_RE
        .captures(&collapsed)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Synthesizes a deterministic `<UUID.sha1>` Message-Id from fallback headers (`FROM TO CC DATE SUBJECT`),
pub fn synthesize_message_id(fallback_header_bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(fallback_header_bytes);
    let normalized = text.trim().replace("\r\n", "\t");
    let mut hasher = Sha1::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(40);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{:02x}", byte);
    }
    format!("<{}.{}>", UUID, hex)
}

/// Extracts the byte-counted literal `{N}\r\n<N bytes>` or quoted payload from a raw IMAP `FETCH` response.
pub fn extract_fetch_payload(raw_response: &[u8]) -> Option<Vec<u8>> {
    // Look for IMAP literal `{byte_count}\r\n` on the untagged `* <seq> FETCH` line
    for (idx, window) in raw_response.windows(3).enumerate() {
        if window[1] == b'\r' && window[2] == b'\n' && window[0] == b'}' {
            // Walk backwards to find matching '{'
            if let Some(open_rel) = raw_response[..idx].iter().rposition(|&b| b == b'{') {
                if let Ok(len_str) = std::str::from_utf8(&raw_response[open_rel + 1..idx]) {
                    if let Ok(byte_len) = len_str.parse::<usize>() {
                        let start = idx + 3;
                        let end = start.saturating_add(byte_len);
                        if end <= raw_response.len() {
                            return Some(raw_response[start..end].to_vec());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_and_nested_imap_list_lines() {
        let parsed = parse_list(r#"* LIST (\HasNoChildren \Marked) "." "INBOX.Sent""#).unwrap();
        assert_eq!(parsed.delimiter, ".");
        assert_eq!(parsed.mailbox, "INBOX.Sent");
        assert_eq!(
            parsed.attributes,
            vec![
                ListAttribute::Flag("\\HasNoChildren".to_string()),
                ListAttribute::Flag("\\Marked".to_string()),
            ]
        );

        let root = parse_list(r#"(\Noselect) "/" """#).unwrap();
        assert_eq!(root.delimiter, "/");
        assert_eq!(root.mailbox, "");
    }

    #[test]
    fn maps_thunderbird_and_mbox_filenames() {
        assert_eq!(
            folder_to_filename("INBOX/Receipts/2026", "/", Compression::None, false),
            "INBOX.Receipts.2026.mbox"
        );
        assert_eq!(
            folder_to_filename("INBOX/Receipts/2026", "/", Compression::Gzip, true),
            "Inbox.sbd/Receipts.sbd/2026.gz"
        );
    }

    #[test]
    fn extracts_folded_exchange_message_ids_and_synthesizes_fallback() {
        let folded = "Message-ID:\r\n \t<CA+9f02a@mail.gmail.com>\r\n\r\n";
        assert_eq!(
            extract_message_id(folded).as_deref(),
            Some("<CA+9f02a@mail.gmail.com>")
        );

        let fallback = b"From: alice@example.com\r\nSubject: Draft note\r\n\r\n";
        let synth = synthesize_message_id(fallback);
        assert!(synth.starts_with("<19AF1258-1AAF-44EF-9D9A-731079D6FAD7."));
        assert!(synth.ends_with('>'));
    }

    #[test]
    fn extracts_imap_literal_from_fetch_response() {
        let raw = b"* 1 FETCH (BODY[HEADER.FIELDS (MESSAGE-ID)] {38}\r\nMessage-ID: <test-123@example.org>\r\n\r\n)\r\na1 OK FETCH completed\r\n";
        let payload = extract_fetch_payload(raw).unwrap();
        let id = extract_message_id(&String::from_utf8_lossy(&payload)).unwrap();
        assert_eq!(id, "<test-123@example.org>");
    }
}
