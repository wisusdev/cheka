//! Mensajes de consola con el mismo formato que la versión en bash.

use std::io::IsTerminal;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);
static SILENT_WARNINGS: AtomicBool = AtomicBool::new(false);

pub struct Colors {
    pub bold: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub red: &'static str,
    pub cyan: &'static str,
    pub reset: &'static str,
}

/// Como en bash, los colores dependen de que la salida estándar sea una terminal.
pub fn colors() -> &'static Colors {
    static COLORS: OnceLock<Colors> = OnceLock::new();
    COLORS.get_or_init(|| {
        if std::io::stdout().is_terminal() {
            Colors {
                bold: "\x1b[1m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                red: "\x1b[31m",
                cyan: "\x1b[36m",
                reset: "\x1b[0m",
            }
        } else {
            Colors { bold: "", green: "", yellow: "", red: "", cyan: "", reset: "" }
        }
    })
}

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

pub fn info(msg: impl AsRef<str>) {
    if !quiet() {
        let c = colors();
        println!("{}›{} {}", c.cyan, c.reset, msg.as_ref());
    }
}

pub fn ok(msg: impl AsRef<str>) {
    if !quiet() {
        let c = colors();
        println!("{}✔{} {}", c.green, c.reset, msg.as_ref());
    }
}

/// Silencia también las advertencias (lo usa el refresh periódico del daemon).
pub fn set_silent_warnings(silent: bool) {
    SILENT_WARNINGS.store(silent, Ordering::Relaxed);
}

pub fn warn(msg: impl AsRef<str>) {
    if SILENT_WARNINGS.load(Ordering::Relaxed) {
        return;
    }
    let c = colors();
    eprintln!("{}!{} {}", c.yellow, c.reset, msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    let c = colors();
    eprintln!("{}✘{} {}", c.red, c.reset, msg.as_ref());
}
