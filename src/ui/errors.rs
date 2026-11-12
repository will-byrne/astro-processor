//! Collects error-level log messages from libraries (the file-dialog crate
//! reports failures only through `log`) so the UI can show them.

use std::sync::Mutex;

static LAST: Mutex<Option<String>> = Mutex::new(None);

struct Collector;

impl log::Log for Collector {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        // The RAW decoder warns about every telescope shot (no lens data);
        // only the dialog crate's warnings are worth showing.
        let from_dialog = record.target().starts_with("rfd");
        if !self.enabled(record.metadata()) || (record.level() > log::Level::Error && !from_dialog) {
            return;
        }
        eprintln!("[{}] {}: {}", record.level(), record.target(), record.args());
        if record.level() == log::Level::Error && from_dialog {
            *LAST.lock().unwrap() = Some(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

pub fn install() {
    if log::set_logger(&Collector).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// The most recent file-dialog failure, if one happened since the last call.
pub fn take_dialog_error() -> Option<String> {
    LAST.lock().unwrap().take()
}
