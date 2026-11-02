use std::io::{self, IsTerminal, Write};
use std::process;

use indexmap::IndexMap;

use imapbackup::cli::get_config;
use imapbackup::error::BackupError;
use imapbackup::imap_client::ImapConnection;
use imapbackup::mbox::{create_folder_structure, download_messages, scan_file};

fn install_ctrlc_handler() {
    if io::stdin().is_terminal() {
        let _ = ctrlc::set_handler(|| {
            let mut stdout = io::stdout();
            let _ = writeln!(stdout);
            let _ = stdout.flush();
            process::exit(130);
        });
    }
}

fn run() -> Result<(), BackupError> {
    let config = get_config();
    let mut server = ImapConnection::connect_and_login(&config)?;

    let mut names = server.get_names(config.compress, config.thunderbird, config.nospinner)?;

    if let Some(ref filter_folders) = config.folders {
        let normalized_filters: Vec<String> = filter_folders
            .iter()
            .map(|dir| {
                if config.thunderbird && dir.starts_with("Inbox") {
                    dir.replacen("Inbox", "INBOX", 1)
                } else {
                    dir.clone()
                }
            })
            .collect();

        names.retain(|(foldername, _)| normalized_filters.contains(foldername));
    }

    create_folder_structure(&names)?;

    for (foldername, filename) in &names {
        let fol_messages = match server.scan_folder(foldername, config.nospinner) {
            Ok(msgs) => msgs,
            Err(skip_err) => {
                println!("{}", skip_err);
                continue;
            }
        };

        let fil_messages = scan_file(
            filename,
            config.compress,
            config.overwrite,
            config.nospinner,
        )?;

        let mut new_messages = IndexMap::new();
        for (msg_id, &seq_num) in &fol_messages {
            if !fil_messages.contains(msg_id) {
                new_messages.insert(msg_id.clone(), seq_num);
            }
        }

        download_messages(&mut server, filename, &new_messages, &config)?;
    }

    println!("Disconnecting");
    server.logout()?;
    Ok(())
}

fn main() {
    install_ctrlc_handler();

    if let Err(err) = run() {
        let code = err.exit_code();
        eprintln!("ERROR: {}", err);
        process::exit(code);
    }
}
