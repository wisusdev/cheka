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
