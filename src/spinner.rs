use std::io::{self, IsTerminal, Write};

const GLYPHS: [char; 4] = ['|', '/', '-', '\\'];

/// Prints a status message with an inline rotating ASCII spinner when attached to a TTY.
pub struct Spinner {
    pos: usize,
    message: String,
    enabled: bool,
}

impl Spinner {
    pub fn new(message: impl Into<String>, nospinner: bool) -> Self {
        let message = message.into();
        let enabled = io::stdin().is_terminal() && !nospinner;

        let mut stdout = io::stdout();
        let _ = write!(stdout, "{}", message);
        let _ = stdout.flush();

        let mut spinner = Self {
            pos: 0,
            message,
            enabled,
        };
        spinner.spin();
        spinner
    }

    /// Advance the spinner glyph by one frame.
    pub fn spin(&mut self) {
        if self.enabled {
            let mut stdout = io::stdout();
            let _ = write!(stdout, "\r{} {}", self.message, GLYPHS[self.pos]);
            let _ = stdout.flush();
            self.pos = (self.pos + 1) % GLYPHS.len();
        }
    }

    /// Erase the trailing spinner glyph from the current terminal line.
    pub fn stop(&self) {
        if self.enabled {
            let mut stdout = io::stdout();
            let _ = write!(stdout, "\r{}  \r{}", self.message, self.message);
            let _ = stdout.flush();
        }
    }
}

/// Converts a byte count into a human-readable string (e.g. `12.243 MB`).
/// Preserves the exact thresholds and decimal precision of `pretty_byte_count` in Python.
pub fn pretty_byte_count(num: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;
    const TB: f64 = 1_099_511_627_776.0;

    if num == 1 {
        "1 byte".to_string()
    } else if num < 1024 {
        format!("{} bytes", num)
    } else if num < 1_048_576 {
        format!("{:.2} KB", (num as f64) / KB)
    } else if num < 1_073_741_824 {
        format!("{:.3} MB", (num as f64) / MB)
    } else if num < 1_099_511_627_776 {
        format!("{:.3} GB", (num as f64) / GB)
    } else {
        format!("{:.3} TB", (num as f64) / TB)
    }
}

#[cfg(test)]
mod tests {
    use super::pretty_byte_count;

    #[test]
    fn formats_byte_counts_identically_to_python() {
        assert_eq!(pretty_byte_count(0), "0 bytes");
        assert_eq!(pretty_byte_count(1), "1 byte");
        assert_eq!(pretty_byte_count(512), "512 bytes");
        assert_eq!(pretty_byte_count(2048), "2.00 KB");
        assert_eq!(pretty_byte_count(12_837_724), "12.243 MB");
        assert_eq!(pretty_byte_count(2_147_483_648), "2.000 GB");
    }
}
