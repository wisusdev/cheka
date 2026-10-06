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

    #[cfg(unix)]
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

    /// Windows: todo lo del sistema vive en `%ProgramData%\cheka` (Apache y PHP incluidos,
    /// porque cheka los descarga). Con prefijo, en `<prefijo>\ProgramData\cheka`.
    #[cfg(windows)]
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let base = if prefix.is_empty() {
            PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join("cheka")
        } else {
            PathBuf::from(format!(r"{prefix}\ProgramData\cheka"))
        };
        let apache = base.join("apache");
        Self {
            bin: base.join("bin"),
            etc: base.join("etc"),
            apache_sites: base.join(r"etc\apache\sites"),
            log_dir: base.join("logs"),
            run_dir: base.join("run"),
            // Sin equivalente en Windows (servicios de Windows y archivo hosts): no se usan.
            units: base.join(r"etc\services"),
            resolved_dropin: base.join(r"etc\dns"),
            apache_envvars: apache.join(r"conf\envvars"),
            apache_conf: apache.join(r"conf\extra\cheka.conf"),
            apache_site_conf: apache.join(r"conf\extra\cheka-sites.conf"),
            opt: base,
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
