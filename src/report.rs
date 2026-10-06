//! Datos de solo lectura (sitios, versiones de PHP, estado de servicios) como estructuras
//! serializables. La CLI los muestra como texto o con `--json`; la UI usa el JSON.

use std::process::{Command, Stdio};

use serde::Serialize;

use crate::detect::detect;
use crate::layout::TLD;
use crate::{Ctx, ipc, php, sites};

#[derive(Debug, Serialize)]
pub struct SiteInfo {
    pub name: String,
    pub kind: String,
    pub php: String,
    pub php_installed: bool,
    /// Versión asignada solo a este sitio (`isolate`); si no, usa la por defecto.
    pub isolated: bool,
    pub secure: bool,
    pub url: String,
    pub path: String,
    pub docroot: String,
    /// Publicado con `link` (y no por estar en una carpeta aparcada).
    pub linked: bool,
}

pub fn sites(ctx: &Ctx) -> Vec<SiteInfo> {
    let st = &ctx.state;
    sites::list(st)
        .into_iter()
        .map(|s| {
            let det = detect(&s.path, st.docroot.get(&s.name).map(String::as_str));
            let php = st.site_php(&s.name);
            let secure = st.is_secure(&s.name);
            SiteInfo {
                kind: det.kind.as_str().to_string(),
                php_installed: php::installed(&ctx.layout, &php),
                isolated: st.isolated.contains_key(&s.name),
                url: format!("{}://{}.{TLD}", if secure { "https" } else { "http" }, s.name),
                path: s.path.display().to_string(),
                docroot: det.docroot.display().to_string(),
                linked: st.links.contains_key(&s.name),
                secure,
                php,
                name: s.name,
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub installed: bool,
    pub default: bool,
    /// "apt" o "static" si está instalada.
    pub source: Option<&'static str>,
    pub fpm_bin: Option<String>,
}

pub fn versions(ctx: &Ctx) -> Vec<VersionInfo> {
    let def = ctx.state.default_php();
    php::SUPPORTED
        .iter()
        .map(|v| {
            let installed = php::installed(&ctx.layout, v);
            let bin = php::fpm_bin(&ctx.layout, v);
            VersionInfo {
                version: v.to_string(),
                installed,
                default: *v == def,
                source: installed.then(|| if bin.starts_with("/usr/sbin") { "apt" } else { "static" }),
                fpm_bin: installed.then(|| bin.display().to_string()),
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct Service {
    pub name: String,
    /// Estado de systemd: active, inactive, failed…
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub services: Vec<Service>,
    /// IP a la que resuelve `*.test`, si resuelve.
    pub dns: Option<String>,
    pub daemon: bool,
    pub socket: String,
    pub default_php: String,
}

pub fn php_units() -> Vec<String> {
    Command::new("systemctl")
        .args(["list-units", "--all", "--plain", "--no-legend", "cheka-php@*.service"])
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_host(host: &str) -> Option<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    (host, 0).to_socket_addrs().ok()?.next().map(|a| a.ip())
}

pub fn status(ctx: &Ctx) -> Status {
    let mut units: Vec<String> = ["apache2", "cheka", "cheka-dns", "mariadb"].map(String::from).into();
    // Mientras convivan las versiones: el vigilante de bash, solo si sigue instalado.
    for legacy in ["cheka-watch.path", "cheka-refresh.timer"] {
        if ctx.layout.units.join(legacy).exists() {
            units.push(legacy.to_string());
        }
    }
    units.extend(php_units());
    let services = units
        .into_iter()
        .map(|name| {
            let out = Command::new("systemctl").args(["is-active", &name]).stderr(Stdio::null()).output();
            let state = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            Service { name, state }
        })
        .collect();
    Status {
        services,
        dns: resolve_host(&format!("cheka-check.{TLD}")).map(|ip| ip.to_string()),
        daemon: ipc::call(&ctx.layout.socket(), &ipc::Request::Ping).is_ok_and(|r| r.ok),
        socket: ctx.layout.socket().display().to_string(),
        default_php: ctx.state.default_php(),
    }
}

// ---------------------------------------------------------------- servicios ----

/// Detalles de un servicio para `services` y la UI.
#[derive(Debug, Serialize)]
pub struct ServiceDetail {
    /// Nombre de la unidad sin ".service" (apache2, mariadb, cheka-php@8.5…).
    pub id: String,
    pub label: String,
    /// Estado de systemd: active, inactive, failed…
    pub state: String,
    /// Arranca con el sistema.
    pub enabled: bool,
    pub pid: Option<u32>,
    pub memory_bytes: Option<u64>,
    /// Segundos desde que arrancó (si está activo).
    pub uptime_secs: Option<u64>,
    pub version: Option<String>,
    /// Dónde escucha: "TCP 80", "socket /run/…"
    pub listen: Vec<String>,
    /// Carpeta o archivo de configuración principal.
    pub config: Option<String>,
    /// Archivos de log además del journal.
    pub log_files: Vec<String>,
}

/// Servicios que cheka gestiona: (id, nombre visible).
pub fn service_ids() -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = [
        ("apache2", "Apache"),
        ("mariadb", "MariaDB"),
        ("cheka-dns", "DNS (*.test)"),
        ("cheka", "Daemon de cheka"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    for unit in php_units() {
        let id = unit.trim_end_matches(".service").to_string();
        let v = id.trim_start_matches("cheka-php@").to_string();
        list.push((id, format!("PHP-FPM {v}")));
    }
    list
}

fn systemd_props(unit: &str) -> std::collections::BTreeMap<String, String> {
    Command::new("systemctl")
        .args(["show", unit, "--timestamp=unix", "-p", "ActiveState,MainPID,MemoryCurrent,ActiveEnterTimestamp,UnitFileState"])
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Primera coincidencia de `re` en la salida de un comando (para versiones).
fn version_of(program: &str, args: &[&str], re: &str) -> Option<String> {
    let out = Command::new(program).args(args).env("LC_ALL", "C").stderr(Stdio::piped()).output().ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    regex::Regex::new(re).ok()?.captures(&text).map(|c| c[1].to_string())
}

/// Puertos TCP/UDP en escucha (sin root no se ve el proceso, solo el puerto).
fn listening_ports() -> (std::collections::BTreeSet<u16>, std::collections::BTreeSet<u16>) {
    let ports = |flag: &str| -> std::collections::BTreeSet<u16> {
        Command::new("ss")
            .args([flag, "-H"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.split_whitespace().nth(3)?.rsplit(':').next()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    (ports("-ltn"), ports("-lun"))
}

pub fn services(ctx: &Ctx) -> Vec<ServiceDetail> {
    let (tcp, udp) = listening_ports();
    let port = |p: u16, proto: &str| {
        let open = if proto == "TCP" { tcp.contains(&p) } else { udp.contains(&p) };
        format!("{proto} {p}{}", if open { "" } else { " (cerrado)" })
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    service_ids()
        .into_iter()
        .map(|(id, label)| {
            let props = systemd_props(&id);
            let state = props.get("ActiveState").cloned().unwrap_or_else(|| "desconocido".into());
            let active = state == "active";
            let pid = props.get("MainPID").and_then(|p| p.parse::<u32>().ok()).filter(|p| *p > 0);
            let memory_bytes = props.get("MemoryCurrent").and_then(|m| m.parse::<u64>().ok()).filter(|_| active);
            let uptime_secs = props
                .get("ActiveEnterTimestamp")
                .and_then(|t| t.trim_start_matches('@').parse::<u64>().ok())
                .filter(|_| active)
                .map(|t| now.saturating_sub(t));
            let (version, listen, config, log_files) = match id.as_str() {
                "apache2" => (
                    version_of("apache2ctl", &["-v"], r"Apache/(\S+)"),
                    vec![port(80, "TCP"), port(443, "TCP")],
                    Some("/etc/apache2".to_string()),
                    vec!["/var/log/apache2/error.log".to_string(), ctx.layout.log_dir.display().to_string()],
                ),
                "mariadb" => (
                    version_of("mariadb", &["--version"], r"(\d+\.\d+\.\d+)-MariaDB"),
                    vec![port(3306, "TCP"), "socket /run/mysqld/mysqld.sock".into()],
                    Some("/etc/mysql/mariadb.conf.d".to_string()),
                    vec![],
                ),
                "cheka-dns" => (
                    version_of("/usr/sbin/dnsmasq", &["--version"], r"(?i)dnsmasq \S+ (\d+\.\d+)"),
                    vec![port(crate::layout::DNS_PORT, "UDP"), port(crate::layout::DNS_PORT, "TCP")],
                    Some(ctx.layout.units.join("cheka-dns.service").display().to_string()),
                    vec![],
                ),
                "cheka" => (
                    Some(env!("CARGO_PKG_VERSION").to_string()),
                    vec![format!("socket {}", ctx.layout.socket().display())],
                    Some(ctx.state.conf.join(crate::state::TOML_FILE).display().to_string()),
                    vec![],
                ),
                php_id => {
                    let v = php_id.trim_start_matches("cheka-php@");
                    (
                        version_of(&php::fpm_bin(&ctx.layout, v).display().to_string(), &["-v"], r"PHP (\S+)"),
                        vec![format!("socket {}", php::socket(&ctx.layout, v).display())],
                        Some(php::config_dir(&ctx.layout, v).display().to_string()),
                        vec![
                            ctx.layout.log_dir.join(format!("php-{v}-errors.log")).display().to_string(),
                            ctx.layout.log_dir.join(format!("php-{v}-fpm.log")).display().to_string(),
                        ],
                    )
                }
            };
            ServiceDetail {
                enabled: props.get("UnitFileState").is_some_and(|s| s == "enabled"),
                id,
                label,
                state,
                pid,
                memory_bytes,
                uptime_secs,
                version,
                listen,
                config,
                log_files,
            }
        })
        .collect()
}
