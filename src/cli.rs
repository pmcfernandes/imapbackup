use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
}

impl Compression {
    pub fn suffix(self) -> &'static str {
        match self {
            Compression::None => "",
            Compression::Gzip => ".gz",
            Compression::Bzip2 => ".bz2",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub compress: Compression,
    pub overwrite: bool,
    pub usessl: bool,
    pub thunderbird: bool,
    pub nospinner: bool,
    pub server: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub timeout: u64,
    pub keyfilename: Option<String>,
    pub certfilename: Option<String>,
    pub folders: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct PartialConfig {
    compress: Option<Compression>,
    overwrite: bool,
    usessl: bool,
    thunderbird: bool,
    nospinner: bool,
    raw_server: Option<String>,
    user: Option<String>,
    password: Option<String>,
    raw_timeout: Option<String>,
    keyfilename: Option<String>,
    certfilename: Option<String>,
    raw_folders: Option<String>,
}

/// Expand leading `~` or `~/` to the current user's home directory, matching `os.path.expanduser`.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
            let mut buf = PathBuf::from(home);
            if path.len() > 2 {
                buf.push(&path[2..]);
            }
            return buf;
        }
    }
    PathBuf::from(path)
}

/// Read a string from a file when prefixed with `@`, or unescape `\@` to a literal `@`.
pub fn string_from_file(value: &str) -> io::Result<String> {
    if value.is_empty() {
        return Ok(String::new());
    }
    if let Some(stripped) = value.strip_prefix('\\') {
        return Ok(stripped.to_string());
    }
    if let Some(file_ref) = value.strip_prefix('@') {
        let path = expand_tilde(file_ref);
        let content = fs::read_to_string(path)?;
        return Ok(content.trim().to_string());
    }
    Ok(value.to_string())
}

pub fn print_usage_and_exit() -> ! {
    println!("Usage: imapbackup [OPTIONS] -s HOST -u USERNAME [-p PASSWORD]");
    println!(" -a --append-to-mboxes     Append new messages to mbox files. (default)");
    println!(" -y --yes-overwrite-mboxes Overwrite existing mbox files instead of appending.");
    println!(" -n --compress=none        Use one plain mbox file for each folder. (default)");
    println!(" -z --compress=gzip        Use mbox.gz files.  Appending may be very slow.");
    println!(" -b --compress=bzip2       Use mbox.bz2 files. Appending not supported: use -y.");
    println!(" -f --folders=LIST         Specify which folders to include. Comma separated list.");
    println!(" -e --ssl                  Use SSL.  Port defaults to 993.");
    println!(" -k KEY --keyfile=KEY      PEM private key file for SSL.  Specify cert, too.");
    println!(" -c CERT --certfile=CERT   PEM certificate chain for SSL.  Specify key, too.");
    println!(" -s HOST --server=HOST     Address of server, port optional, eg. mail.com:143");
    println!(" -u USER --user=USER       Username to log into server");
    println!(" -p PASS --pass=PASS       Prompts for password if not specified.  If the first");
    println!("                           character is '@', treat the rest as a path to a file");
    println!("                           containing the password.  Leading '\\' makes it literal.");
    println!(" -t SECS --timeout=SECS    Sets socket timeout to SECS seconds.");
    println!(" --thunderbird             Create Mozilla Thunderbird compatible mailbox");
    println!(" --nospinner               Disable spinner (makes output log-friendly)");
    println!("\nNOTE: mbox files are created in the current working directory.");
    process::exit(2);
}

/// Parses GNU/Python `getopt`-compatible short and long options.
fn process_cline(
    args: &[String],
) -> Result<(PartialConfig, Vec<String>, Vec<String>), ()> {
    if args.is_empty() {
        return Err(());
    }

    let mut cfg = PartialConfig {
        compress: Some(Compression::None),
        overwrite: false,
        ..Default::default()
    };
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            for extra in &args[i + 1..] {
                errors.push(format!("Unknown argument: {}", extra));
            }
            break;
        } else if let Some(long_opt) = arg.strip_prefix("--") {
            let (name, inline_val) = match long_opt.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (long_opt, None),
            };

            let mut take_val = |inline: Option<String>, idx: &mut usize| -> Result<String, ()> {
                if let Some(v) = inline {
                    Ok(v)
                } else if *idx + 1 < args.len() {
                    *idx += 1;
                    Ok(args[*idx].clone())
                } else {
                    Err(())
                }
            };

            match name {
                "help" => return Err(()),
                "append-to-mboxes" => {
                    if inline_val.is_some() {
                        return Err(());
                    }
                    cfg.overwrite = false;
                }
                "yes-overwrite-mboxes" => {
                    if inline_val.is_some() {
                        return Err(());
                    }
                    warnings.push("Existing mbox files will be overwritten!".to_string());
                    cfg.overwrite = true;
                }
                "compress" => {
                    let val = take_val(inline_val, &mut i)?;
                    match val.as_str() {
                        "none" => cfg.compress = Some(Compression::None),
                        "gzip" => cfg.compress = Some(Compression::Gzip),
                        "bzip2" => cfg.compress = Some(Compression::Bzip2),
                        _ => errors.push("Invalid compression type specified.".to_string()),
                    }
                }
                "ssl" => {
                    if inline_val.is_some() {
                        return Err(());
                    }
                    cfg.usessl = true;
                }
                "keyfile" | "key" => {
                    cfg.keyfilename = Some(take_val(inline_val, &mut i)?);
                }
                "certfile" | "cert" => {
                    cfg.certfilename = Some(take_val(inline_val, &mut i)?);
                }
                "folders" | "folder" => {
                    cfg.raw_folders = Some(take_val(inline_val, &mut i)?);
                }
                "server" => {
                    cfg.raw_server = Some(take_val(inline_val, &mut i)?);
                }
                "user" => {
                    cfg.user = Some(take_val(inline_val, &mut i)?);
                }
                "pass" => {
                    let raw = take_val(inline_val, &mut i)?;
                    match string_from_file(&raw) {
                        Ok(pw) => cfg.password = Some(pw),
                        Err(ex) => errors.push(format!("Can't read password: {}", ex)),
                    }
                }
                "timeout" => {
                    cfg.raw_timeout = Some(take_val(inline_val, &mut i)?);
                }
                "thunderbird" => {
                    if inline_val.is_some() {
                        return Err(());
                    }
                    cfg.thunderbird = true;
                }
                "nospinner" => {
                    if inline_val.is_some() {
                        return Err(());
                    }
                    cfg.nospinner = true;
                }
                _ => return Err(()),
            }
        } else if arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut c_idx = 0;
            while c_idx < chars.len() {
                let ch = chars[c_idx];
                let takes_value = matches!(ch, 'k' | 't' | 'c' | 's' | 'u' | 'p' | 'f');
                if takes_value {
                    let val = if c_idx + 1 < chars.len() {
                        chars[c_idx + 1..].iter().collect::<String>()
                    } else if i + 1 < args.len() {
                        i += 1;
                        args[i].clone()
                    } else {
                        return Err(());
                    };
                    match ch {
                        'k' => cfg.keyfilename = Some(val),
                        't' => cfg.raw_timeout = Some(val),
                        'c' => cfg.certfilename = Some(val),
                        's' => cfg.raw_server = Some(val),
                        'u' => cfg.user = Some(val),
                        'p' => match string_from_file(&val) {
                            Ok(pw) => cfg.password = Some(pw),
                            Err(ex) => errors.push(format!("Can't read password: {}", ex)),
                        },
                        'f' => cfg.raw_folders = Some(val),
                        _ => unreachable!(),
                    }
                    break;
                } else {
                    match ch {
                        'a' => cfg.overwrite = false,
                        'y' => {
                            warnings.push("Existing mbox files will be overwritten!".to_string());
                            cfg.overwrite = true;
                        }
                        'n' => cfg.compress = Some(Compression::None),
                        'z' => cfg.compress = Some(Compression::Gzip),
                        'b' => cfg.compress = Some(Compression::Bzip2),
                        'e' => cfg.usessl = true,
                        'h' => return Err(()),
                        _ => return Err(()),
                    }
                }
                c_idx += 1;
            }
        } else {
            errors.push(format!("Unknown argument: {}", arg));
        }
        i += 1;
    }

    Ok((cfg, warnings, errors))
}

#[derive(Debug)]
pub struct CheckedConfig {
    pub compress: Compression,
    pub overwrite: bool,
    pub usessl: bool,
    pub thunderbird: bool,
    pub nospinner: bool,
    pub server: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub timeout: u64,
    pub keyfilename: Option<String>,
    pub certfilename: Option<String>,
    pub folders: Option<Vec<String>>,
}

/// Validates option combinations, host:port splitting, and timeout bounds.
fn check_config(
    partial: PartialConfig,
    mut warnings: Vec<String>,
    mut errors: Vec<String>,
) -> (Option<CheckedConfig>, Vec<String>, Vec<String>) {
    let compress = partial.compress.unwrap_or(Compression::None);

    if compress == Compression::Bzip2 && !partial.overwrite {
        errors.push("Cannot append new messages to mbox.bz2 files.  Please specify -y.".to_string());
    }
    if compress == Compression::Gzip && !partial.overwrite {
        warnings.push(
            "Appending new messages to mbox.gz files is very slow.  Please Consider\n  using -y and compressing the files yourself with gzip -9 *.mbox"
                .to_string(),
        );
    }
    if partial.raw_server.is_none() {
        errors.push("No server specified.".to_string());
    }
    if partial.user.is_none() {
        errors.push("No username specified.".to_string());
    }
    if partial.keyfilename.is_some() ^ partial.certfilename.is_some() {
        errors.push("Please specify both key and cert or neither.".to_string());
    }
    if partial.keyfilename.is_some() && !partial.usessl {
        errors.push("Key specified without SSL.  Please use -e or --ssl.".to_string());
    }
    if partial.certfilename.is_some() && !partial.usessl {
        errors.push("Certificate specified without SSL.  Please use -e or --ssl.".to_string());
    }

    let mut host = String::new();
    let mut parsed_port: Option<u16> = None;

    if let Some(raw_srv) = &partial.raw_server {
        if let Some((h, port_str)) = raw_srv.split_once(':') {
            host = h.to_string();
            if !port_str.is_empty() {
                match port_str.parse::<u32>() {
                    Ok(p) if p <= 65535 => parsed_port = Some(p as u16),
                    _ => errors.push(
                        "Invalid port.  Port must be an integer between 0 and 65535.".to_string(),
                    ),
                }
            }
        } else {
            host = raw_srv.clone();
        }
    }

    let mut timeout_secs = 60_u64;
    if let Some(raw_t) = &partial.raw_timeout {
        match raw_t.parse::<i64>() {
            Ok(t) if t > 0 => timeout_secs = t as u64,
            _ => errors.push("Invalid timeout value.  Must be an integer greater than 0.".to_string()),
        }
    }

    if !errors.is_empty() {
        return (None, warnings, errors);
    }

    let port = parsed_port.unwrap_or(if partial.usessl { 993 } else { 143 });
    let folders = partial.raw_folders.map(|f| {
        f.split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect::<Vec<String>>()
    });

    let checked = CheckedConfig {
        compress,
        overwrite: partial.overwrite,
        usessl: partial.usessl,
        thunderbird: partial.thunderbird,
        nospinner: partial.nospinner,
        server: host,
        port,
        user: partial.user.unwrap_or_default(),
        password: partial.password,
        timeout: timeout_secs,
        keyfilename: partial.keyfilename,
        certfilename: partial.certfilename,
        folders,
    };

    (Some(checked), warnings, errors)
}

/// Parses CLI options, prints warnings/errors, and prompts for password if omitted.
pub fn get_config() -> Config {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let (partial, warnings, errors) = match process_cline(&raw_args) {
        Ok(res) => res,
        Err(()) => print_usage_and_exit(),
    };

    let (checked_opt, warnings, errors) = check_config(partial, warnings, errors);

    for warning in &warnings {
        println!("WARNING: {}", warning);
    }
    for error in &errors {
        println!("ERROR {}", error);
    }
    if !errors.is_empty() {
        process::exit(2);
    }

    let checked = checked_opt.unwrap_or_else(|| process::exit(2));

    let password = match checked.password {
        Some(pw) => pw,
        None => rpassword::prompt_password("Password: ").unwrap_or_else(|err| {
            eprintln!("ERROR Can't read password from terminal: {}", err);
            process::exit(2);
        }),
    };

    Config {
        compress: checked.compress,
        overwrite: checked.overwrite,
        usessl: checked.usessl,
        thunderbird: checked.thunderbird,
        nospinner: checked.nospinner,
        server: checked.server,
        port: checked.port,
        user: checked.user,
        password,
        timeout: checked.timeout,
        keyfilename: checked.keyfilename,
        certfilename: checked.certfilename,
        folders: checked.folders,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_escaped_password_prefix() {
        assert_eq!(string_from_file(r"\@literalpass").unwrap(), "@literalpass");
        assert_eq!(string_from_file("plainpass").unwrap(), "plainpass");
    }

    #[test]
    fn rejects_bzip2_without_overwrite() {
        let args = vec![
            "-b".to_string(),
            "-s".to_string(),
            "imap.example.com:993".to_string(),
            "-u".to_string(),
            "alice".to_string(),
        ];
        let (p, w, e) = process_cline(&args).unwrap();
        let (checked, _w, errors) = check_config(p, w, e);
        assert!(checked.is_none());
        assert!(errors
            .iter()
            .any(|msg| msg.contains("Cannot append new messages to mbox.bz2")));
    }

    #[test]
    fn parses_host_port_and_ssl_defaults() {
        let args = vec![
            "-e".to_string(),
            "-s".to_string(),
            "mail.example.org".to_string(),
            "-u".to_string(),
            "bob".to_string(),
            "-p".to_string(),
            "secret".to_string(),
        ];
        let (p, w, e) = process_cline(&args).unwrap();
        let (checked, _, errors) = check_config(p, w, e);
        assert!(errors.is_empty());
        let cfg = checked.unwrap();
        assert_eq!(cfg.server, "mail.example.org");
        assert_eq!(cfg.port, 993);
        assert_eq!(cfg.timeout, 60);
    }
}
