use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use indexmap::IndexMap;
use native_tls::{Identity, TlsConnector, TlsStream};

use crate::cli::{Compression, Config};
use crate::error::{BackupError, SkipFolderError};
use crate::parser::{
    extract_message_id, folder_to_filename, parse_list, synthesize_message_id,
};
use crate::spinner::Spinner;

/// Unified transport stream supporting both plain IMAP (port 143) and IMAPS TLS (port 993).
pub enum ImapStream {
    Plain(TcpStream),
    Tls(TlsStream<TcpStream>),
}

impl Read for ImapStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            ImapStream::Plain(s) => s.read(buf),
            ImapStream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for ImapStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            ImapStream::Plain(s) => s.write(buf),
            ImapStream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            ImapStream::Plain(s) => s.flush(),
            ImapStream::Tls(s) => s.flush(),
        }
    }
}

/// One untagged response line from the server, plus any attached `{N}\r\n` literal payload.
#[derive(Debug, Clone)]
pub struct UntaggedRecord {
    pub line: String,
    pub literals: Vec<Vec<u8>>,
}

/// RFC 3501 synchronous IMAP4rev1 client with bounded `BufReader` literal streaming.
/// Replaces both `imaplib.IMAP4` / `IMAP4_SSL` and Python's `_fixed_socket_read` workaround.
pub struct ImapConnection<S: Read + Write = ImapStream> {
    reader: BufReader<S>,
    tag_counter: u64,
}

/// Quotes an IMAP string per RFC 3501 (`\` -> `\\`, `"` -> `\"`).
fn quote_imap_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        if ch == '\\' || ch == '"' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('"');
    escaped
}

/// Checks if a CRLF-terminated IMAP line ends with an RFC 3501 byte-count literal `{N}\r\n`.
fn parse_trailing_literal_len(line_bytes: &[u8]) -> Option<usize> {
    let stripped = line_bytes
        .strip_suffix(b"\r\n")
        .or_else(|| line_bytes.strip_suffix(b"\n"))?;
    let without_close = stripped.strip_suffix(b"}")?;
    let open_pos = without_close.iter().rposition(|&b| b == b'{')?;
    let digits = std::str::from_utf8(&without_close[open_pos + 1..]).ok()?;
    digits.parse::<usize>().ok()
}

impl ImapConnection<ImapStream> {
    /// Connects to the IMAP server, configures `TCP_NODELAY` and socket timeouts,
    /// optionally loads PEM client key/cert, reads the server greeting, and authenticates.
    pub fn connect_and_login(config: &Config) -> Result<Self, BackupError> {
        debug_assert!(!(config.keyfilename.is_some() ^ config.certfilename.is_some()));

        let timeout = Duration::from_secs(config.timeout);

        // Resolve hostname -> SocketAddr (maps DNS failures to Exit Code 3, matching socket.gaierror)
        let addr_iter = (config.server.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|err| BackupError::DnsLookup {
                server: config.server.clone(),
                details: err.to_string(),
            })?;

        let addrs: Vec<_> = addr_iter.collect();
        if addrs.is_empty() {
            return Err(BackupError::DnsLookup {
                server: config.server.clone(),
                details: "no addresses returned".to_string(),
            });
        }

        if config.usessl {
            if let (Some(key_path), Some(cert_path)) = (&config.keyfilename, &config.certfilename) {
                println!(
                    "Connecting to '{}' TCP port {}, SSL, key from {}, cert from {}",
                    config.server, config.port, key_path, cert_path
                );
            } else {
                println!(
                    "Connecting to '{}' TCP port {}, SSL",
                    config.server, config.port
                );
            }
        } else {
            println!("Connecting to '{}' TCP port {}", config.server, config.port);
        }

        let mut last_err = None;
        let mut tcp_opt = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(stream) => {
                    tcp_opt = Some(stream);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }

        let tcp = tcp_opt.ok_or_else(|| {
            let detail = last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "connection refused".to_string());
            BackupError::SocketOrIo(format!(
                "could not connect to '{}' ({})",
                config.server, detail
            ))
        })?;

        // Speed up interactions on TCP connections using small packets (TCP_NODELAY = 1)
        tcp.set_nodelay(true).map_err(|e| {
            BackupError::SocketOrIo(format!(
                "could not set TCP_NODELAY on '{}': {}",
                config.server, e
            ))
        })?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;

        let stream = if config.usessl {
            let mut builder = TlsConnector::builder();

            if let (Some(key_path), Some(cert_path)) = (&config.keyfilename, &config.certfilename) {
                let key_pem = fs::read(key_path).map_err(|_| {
                    BackupError::SocketOrIo(format!(
                        "error reading private key file '{}'",
                        key_path
                    ))
                })?;
                let cert_pem = fs::read(cert_path).map_err(|_| {
                    BackupError::SocketOrIo(format!(
                        "error reading certificate chain file '{}'",
                        cert_path
                    ))
                })?;
                let identity = Identity::from_pkcs8(&cert_pem, &key_pem).map_err(|e| {
                    BackupError::SocketOrIo(format!(
                        "error parsing PEM key/certificate ('{}', '{}'): {}",
                        key_path, cert_path, e
                    ))
                })?;
                builder.identity(identity);
            }

            let connector = builder.build().map_err(|e| {
                BackupError::SocketOrIo(format!("TLS initialization failed: {}", e))
            })?;

            let tls_stream = connector.connect(&config.server, tcp).map_err(|e| {
                BackupError::SocketOrIo(format!(
                    "could not connect to '{}' ({})",
                    config.server, e
                ))
            })?;

            ImapStream::Tls(tls_stream)
        } else {
            ImapStream::Plain(tcp)
        };

        let mut conn = ImapConnection::from_stream(stream)?;

        println!("Logging in as '{}'", config.user);
        conn.login(&config.user, &config.password)?;

        Ok(conn)
    }
}

impl<S: Read + Write> ImapConnection<S> {
    /// Wraps an already-connected stream and validates the initial `* OK` / `* PREAUTH` greeting.
    pub fn from_stream(stream: S) -> Result<Self, BackupError> {
        let mut reader = BufReader::new(stream);
        let mut greeting = Vec::new();
        let n = reader.read_until(b'\n', &mut greeting)?;
        if n == 0 {
            return Err(BackupError::Imap(
                "Connection closed before IMAP server greeting".to_string(),
            ));
        }
        let greeting_str = String::from_utf8_lossy(&greeting);
        if !greeting_str.starts_with("* OK") && !greeting_str.starts_with("* PREAUTH") {
            return Err(BackupError::Imap(format!(
                "Unexpected IMAP greeting: {}",
                greeting_str.trim()
            )));
        }

        Ok(Self {
            reader,
            tag_counter: 0,
        })
    }

    /// Sends a tagged RFC 3501 command and collects untagged records (including exact `{N}\r\n` literals)
    /// until the matching tagged completion response arrives.
    pub fn execute_command(&mut self, command: &str) -> Result<Vec<UntaggedRecord>, BackupError> {
        self.tag_counter += 1;
        let tag = format!("A{:04}", self.tag_counter);
        let wire = format!("{} {}\r\n", tag, command);

        self.reader.get_mut().write_all(wire.as_bytes())?;
        self.reader.get_mut().flush()?;

        let tag_prefix = format!("{} ", tag);
        let mut records = Vec::new();
        let mut line_buf = Vec::new();

        loop {
            line_buf.clear();
            let read_bytes = self.reader.read_until(b'\n', &mut line_buf)?;
            if read_bytes == 0 {
                return Err(BackupError::SocketOrIo(
                    "Unexpected EOF while reading IMAP response".to_string(),
                ));
            }

            let line_str = String::from_utf8_lossy(&line_buf);
            if let Some(rest) = line_str.strip_prefix(&tag_prefix) {
                let status_trimmed = rest.trim();
                if status_trimmed.len() >= 2 && status_trimmed[..2].eq_ignore_ascii_case("OK") {
                    return Ok(records);
                } else {
                    return Err(BackupError::Imap(format!(
                        "{} failed: {}",
                        command.split_whitespace().next().unwrap_or("COMMAND"),
                        status_trimmed
                    )));
                }
            }

            // Untagged response line: read any trailing `{N}\r\n` byte-counted literals safely
            let mut combined_line = line_str.trim_end_matches(&['\r', '\n'][..]).to_string();
            let mut literals = Vec::new();
            let mut current_tail = line_buf.clone();

            while let Some(lit_len) = parse_trailing_literal_len(&current_tail) {
                let mut lit_bytes = vec![0u8; lit_len];
                self.reader.read_exact(&mut lit_bytes)?;
                literals.push(lit_bytes);

                current_tail.clear();
                let tail_n = self.reader.read_until(b'\n', &mut current_tail)?;
                if tail_n == 0 {
                    break;
                }
                combined_line.push(' ');
                combined_line.push_str(
                    String::from_utf8_lossy(&current_tail)
                        .trim_end_matches(&['\r', '\n'][..]),
                );
            }

            records.push(UntaggedRecord {
                line: combined_line,
                literals,
            });
        }
    }

    pub fn login(&mut self, user: &str, pass: &str) -> Result<(), BackupError> {
        let cmd = format!(
            "LOGIN {} {}",
            quote_imap_string(user),
            quote_imap_string(pass)
        );
        self.execute_command(&cmd)?;
        Ok(())
    }

    /// Queries the IMAP server for its hierarchy delimiter (`LIST "" ""`).
    pub fn get_hierarchy_delimiter(&mut self) -> Result<String, BackupError> {
        let records = self.execute_command("LIST \"\" \"\"")?;
        for rec in &records {
            if rec.line.starts_with("* LIST ") {
                let parsed = parse_list(&rec.line).map_err(BackupError::Imap)?;
                if parsed.delimiter.eq_ignore_ascii_case("NIL") || parsed.delimiter.is_empty() {
                    return Ok(".".to_string());
                }
                return Ok(parsed.delimiter);
            }
        }
        Ok(".".to_string())
    }

    /// Lists all folders on the server and computes their target local filenames.
    /// Mirrors `get_names(server, compress, thunderbird, nospinner)`.
    pub fn get_names(
        &mut self,
        compress: Compression,
        thunderbird: bool,
        nospinner: bool,
    ) -> Result<Vec<(String, String)>, BackupError> {
        let mut spinner = Spinner::new("Finding Folders", nospinner);

        let delim = self.get_hierarchy_delimiter()?;
        spinner.spin();

        let records = self.execute_command("LIST \"\" \"*\"")?;
        spinner.spin();

        let mut names = Vec::new();
        for rec in &records {
            if rec.line.starts_with("* LIST ") {
                // If the folder name itself was returned as an IMAP literal, substitute it in
                let parsed = if let Some(first_lit) = rec.literals.first() {
                    let lit_mailbox = String::from_utf8_lossy(first_lit).into_owned();
                    let prefix_end = rec.line.rfind('{').unwrap_or(rec.line.len());
                    let synthesized = format!(
                        "{}{}",
                        &rec.line[..prefix_end],
                        quote_imap_string(&lit_mailbox)
                    );
                    parse_list(&synthesized).map_err(BackupError::Imap)?
                } else {
                    parse_list(&rec.line).map_err(BackupError::Imap)?
                };

                let filename =
                    folder_to_filename(&parsed.mailbox, &delim, compress, thunderbird);
                names.push((parsed.mailbox, filename));
            }
        }

        spinner.stop();
        println!(": {} folders", names.len());
        Ok(names)
    }

    /// Selects a folder in read-only mode (`EXAMINE`) and scans every message's `Message-Id`.
    /// Synthesizes `<UUID.sha1>` when `Message-Id` is absent.
    pub fn scan_folder(
        &mut self,
        foldername: &str,
        nospinner: bool,
    ) -> Result<IndexMap<String, u32>, SkipFolderError> {
        let mut messages = IndexMap::new();
        let quoted_folder = quote_imap_string(foldername);
        let mut spinner = Spinner::new(format!("Folder {}", quoted_folder), nospinner);

        let result = (|| -> Result<u32, SkipFolderError> {
            let examine_records = self
                .execute_command(&format!("EXAMINE {}", quoted_folder))
                .map_err(|e| SkipFolderError(format!("SELECT failed: {}", e)))?;

            let mut num_msgs: u32 = 0;
            for rec in &examine_records {
                // Match `* 42 EXISTS`
                let parts: Vec<&str> = rec.line.split_whitespace().collect();
                if parts.len() >= 3
                    && parts[0] == "*"
                    && parts[2].eq_ignore_ascii_case("EXISTS")
                {
                    if let Ok(count) = parts[1].parse::<u32>() {
                        num_msgs = count;
                    }
                }
            }

            for num in 1..=num_msgs {
                // BODY.PEEK ensures messages are never marked as \Seen
                let fetch_records = self
                    .execute_command(&format!(
                        "FETCH {} (BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
                        num
                    ))
                    .map_err(|e| SkipFolderError(format!("FETCH {} failed: {}", num, e)))?;

                let header_bytes = fetch_records
                    .iter()
                    .find_map(|r| r.literals.first().cloned())
                    .unwrap_or_default();
                let header_str = String::from_utf8_lossy(&header_bytes);

                if let Some(msg_id) = extract_message_id(&header_str) {
                    messages.entry(msg_id).or_insert(num);
                } else {
                    let fb_records = self
                        .execute_command(&format!(
                            "FETCH {} (BODY.PEEK[HEADER.FIELDS (FROM TO CC DATE SUBJECT)])",
                            num
                        ))
                        .map_err(|e| SkipFolderError(format!("FETCH {} failed: {}", num, e)))?;

                    let fb_bytes = fb_records
                        .iter()
                        .find_map(|r| r.literals.first().cloned())
                        .unwrap_or_default();
                    let synth_id = synthesize_message_id(&fb_bytes);
                    messages.entry(synth_id).or_insert(num);
                }

                spinner.spin();
            }

            Ok(num_msgs)
        })();

        spinner.stop();
        print!(": ");

        match result {
            Ok(_) => {
                println!("{} messages", messages.len());
                Ok(messages)
            }
            Err(skip_err) => {
                println!();
                Err(skip_err)
            }
        }
    }

    /// Fetches the complete `RFC822` message body for sequence number `seq_num`.
    pub fn fetch_rfc822(&mut self, seq_num: u32) -> Result<Vec<u8>, BackupError> {
        let records = self.execute_command(&format!("FETCH {} (RFC822)", seq_num))?;
        for rec in records {
            if let Some(body) = rec.literals.into_iter().next() {
                return Ok(body);
            }
        }
        Err(BackupError::Imap(format!(
            "Server returned no RFC822 literal for message {}",
            seq_num
        )))
    }

    pub fn logout(&mut self) -> Result<(), BackupError> {
        let _ = self.execute_command("LOGOUT")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockWire {
        scripted_input: io::Cursor<Vec<u8>>,
        written_output: Vec<u8>,
    }

    impl MockWire {
        fn new(server_responses: &[u8]) -> Self {
            Self {
                scripted_input: io::Cursor::new(server_responses.to_vec()),
                written_output: Vec::new(),
            }
        }
    }

    impl Read for MockWire {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.scripted_input.read(buf)
        }
    }

    impl Write for MockWire {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written_output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn executes_full_imap_session_over_mock_wire() {
        let transcript = concat!(
            "* OK Dovecot ready.\r\n",
            "A0001 OK Logged in\r\n",
            "* LIST (\\Noselect) \".\" \"\"\r\n",
            "A0002 OK LIST completed\r\n",
            "* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n",
            "A0003 OK LIST completed\r\n",
            "* 1 EXISTS\r\n",
            "A0004 OK [READ-ONLY] EXAMINE completed\r\n",
            "* 1 FETCH (BODY[HEADER.FIELDS (MESSAGE-ID)] {34}\r\n",
            "Message-ID: <msg-01@example.com>\r\n",
            ")\r\n",
            "A0005 OK FETCH completed\r\n",
            "* BYE Logging out\r\n",
            "A0006 OK LOGOUT completed\r\n",
        );

        let wire = MockWire::new(transcript.as_bytes());
        let mut conn = ImapConnection::from_stream(wire).unwrap();
        conn.login("alice", "s3cr3t").unwrap();

        let folders = conn.get_names(Compression::None, false, true).unwrap();
        assert_eq!(
            folders,
            vec![("INBOX".to_string(), "INBOX.mbox".to_string())]
        );

        let scanned = conn.scan_folder("INBOX", true).unwrap();
        assert_eq!(scanned.get("<msg-01@example.com>"), Some(&1));

        conn.logout().unwrap();
    }
}
