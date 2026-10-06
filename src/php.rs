//! Versiones de PHP: dónde están sus binarios y si están instaladas.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Result, bail};

use crate::layout::Layout;
use crate::util::is_executable;

pub const SUPPORTED: [&str; 6] = ["8.0", "8.1", "8.2", "8.3", "8.4", "8.5"];

pub fn validate(v: &str) -> Result<()> {
    if !SUPPORTED.contains(&v) {
        bail!("Versión de PHP no soportada: '{v}'. Disponibles: {}", SUPPORTED.join(" "));
    }
    Ok(())
}

/// Acepta "8.2", "php@8.2" o "php8.2".
pub fn normalize(v: &str) -> String {
    let v = v.strip_prefix("php@").unwrap_or(v);
    v.strip_prefix("php").unwrap_or(v).to_string()
}

/// Versión del `php` del sistema (X.Y), o 8.5 si no hay.
pub fn system_php() -> String {
    static SYS: OnceLock<String> = OnceLock::new();
    SYS.get_or_init(|| {
        Command::new("php")
            .args(["-r", r#"echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;"#])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "8.5".to_string())
    })
    .clone()
}

/// PHP de apt (`/usr/sbin/php-fpmX.Y`) o binario estático de cheka.
#[cfg(unix)]
pub fn fpm_bin(layout: &Layout, v: &str) -> PathBuf {
    let apt = PathBuf::from(format!("/usr/sbin/php-fpm{v}"));
    if is_executable(&apt) { apt } else { layout.opt.join(format!("php/{v}/php-fpm")) }
}

/// Windows no tiene FPM: Apache arranca `php-cgi.exe` con `mod_fcgid`.
#[cfg(windows)]
pub fn fpm_bin(layout: &Layout, v: &str) -> PathBuf {
    layout.opt.join("php").join(v).join("php-cgi.exe")
}

#[cfg(unix)]
pub fn cli_bin(layout: &Layout, v: &str) -> PathBuf {
    let apt = apt_cli(v);
    if is_executable(&apt) { apt } else { layout.bin.join(format!("php{v}")) }
}

#[cfg(windows)]
pub fn cli_bin(layout: &Layout, v: &str) -> PathBuf {
    layout.opt.join("php").join(v).join("php.exe")
}

pub fn apt_cli(v: &str) -> PathBuf {
    PathBuf::from(format!("/usr/bin/php{v}"))
}

pub fn installed(layout: &Layout, v: &str) -> bool {
    is_executable(&fpm_bin(layout, v))
}

pub fn socket(layout: &Layout, v: &str) -> PathBuf {
    layout.run_dir.join(format!("php-{v}/fpm.sock"))
}

pub fn config_dir(layout: &Layout, v: &str) -> PathBuf {
    layout.etc.join(format!("php/{v}"))
}

pub fn unit(v: &str) -> String {
    format!("cheka-php@{v}")
}

/// Zona horaria del sistema para `date.timezone`.
#[cfg(unix)]
pub fn timezone() -> String {
    Command::new("timedatectl")
        .args(["show", "-p", "Timezone", "--value"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

/// Windows usa nombres propios de zona horaria ("Central America Standard Time"), no los
/// de IANA que espera PHP: se usa `TZ` si está definida, si no UTC.
#[cfg(windows)]
pub fn timezone() -> String {
    std::env::var("TZ").ok().filter(|s| s.contains('/')).unwrap_or_else(|| "UTC".to_string())
}
