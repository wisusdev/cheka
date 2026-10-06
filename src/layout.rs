//! Rutas del sistema. Con `CHEKA_PREFIX` todo se escribe bajo un prefijo y no se
//! tocan servicios: es el modo de prueba sin root (igual que en bash).

use std::path::PathBuf;

pub const TLD: &str = "test";
pub const DB_USER: &str = "cheka";
pub const DB_PASS: &str = "secret";
pub const DNS_PORT: u16 = 5300;

#[derive(Debug, Clone)]
pub struct Layout {
    /// Prefijo de prueba; vacío en el sistema real.
    pub prefix: String,
    pub bin: PathBuf,
    pub etc: PathBuf,
    pub opt: PathBuf,
    pub apache_sites: PathBuf,
    pub log_dir: PathBuf,
    pub run_dir: PathBuf,
    pub units: PathBuf,
    pub resolved_dropin: PathBuf,
    pub apache_conf: PathBuf,
    pub apache_site_conf: PathBuf,
    pub apache_envvars: PathBuf,
}

impl Layout {
    pub fn from_env() -> Self {
        Self::with_prefix(std::env::var("CHEKA_PREFIX").unwrap_or_default())
    }

    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        // Concatenación de texto, como "$PREFIX/usr/local/bin" en bash: no se resuelven
        // symlinks del prefijo (importa para la longitud de los sockets).
        let p = |s: &str| PathBuf::from(format!("{prefix}{s}"));
        Self {
            bin: p("/usr/local/bin"),
            etc: p("/etc/cheka"),
            opt: p("/opt/cheka"),
            apache_sites: p("/etc/apache2/cheka/sites"),
            log_dir: p("/var/log/cheka"),
            run_dir: p("/run/cheka"),
            units: p("/etc/systemd/system"),
            resolved_dropin: p("/etc/systemd/resolved.conf.d/cheka.conf"),
            apache_conf: p("/etc/apache2/conf-available/cheka.conf"),
            apache_site_conf: p("/etc/apache2/sites-available/cheka.conf"),
            apache_envvars: p("/etc/apache2/envvars"),
            prefix,
        }
    }

    /// Socket del daemon (API para la CLI y la futura UI).
    pub fn socket(&self) -> PathBuf {
        self.run_dir.join("cheka.sock")
    }

    /// Modo prueba: sin servicios, sin `apache2ctl`, sin sudo.
    pub fn is_test(&self) -> bool {
        !self.prefix.is_empty()
    }
}
