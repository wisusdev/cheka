//! Configuración por versión de PHP: ajustes de php.ini y extensiones, aplicados desde
//! `cheka.toml` (`[php."X.Y"]`), más la información que muestra `php:info`.
//!
//! - **Ajustes:** se escriben en `/etc/cheka/php/X.Y/conf.d/99-cheka.ini`.
//! - **Extensiones (solo PHP de apt):** el PHP-FPM de cheka no lee la carpeta del sistema
//!   (`/etc/php/X.Y/fpm/conf.d`) sino una copia propia, `/etc/cheka/php/X.Y/ext.d`, que
//!   refleja lo que trae el sistema más los cambios del usuario. Desactivar una extensión
//!   en cheka no toca el PHP del sistema, y lo que se instale con apt aparece solo.
//! - Los binarios estáticos traen sus extensiones compiladas: no se pueden cambiar.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::layout::Layout;
use crate::state::PhpSettings;
use crate::util::is_executable;
use crate::{Ctx, php};

/// Ajustes que la UI y `php:info` muestran siempre (además de los que el usuario cambie).
pub const COMMON_SETTINGS: [&str; 11] = [
    "memory_limit",
    "upload_max_filesize",
    "post_max_size",
    "max_execution_time",
    "max_input_time",
    "max_input_vars",
    "display_errors",
    "error_reporting",
    "date.timezone",
    "opcache.enable",
    "short_open_tag",
];

/// Paquetes `phpX.Y-*` que no son extensiones.
const NOT_EXTENSIONS: [&str; 8] = ["cli", "fpm", "cgi", "common", "dev", "phpdbg", "dbg", "embed"];

pub fn is_apt(v: &str) -> bool {
    is_executable(Path::new(&format!("/usr/sbin/php-fpm{v}")))
}

fn system_conf_d(v: &str) -> PathBuf {
    PathBuf::from(format!("/etc/php/{v}/fpm/conf.d"))
}

fn mods_available(v: &str) -> PathBuf {
    PathBuf::from(format!("/etc/php/{v}/mods-available"))
}

pub fn ext_dir(layout: &Layout, v: &str) -> PathBuf {
    php::config_dir(layout, v).join("ext.d")
}

pub fn user_ini_path(layout: &Layout, v: &str) -> PathBuf {
    php::config_dir(layout, v).join("conf.d/99-cheka.ini")
}

/// `PHP_INI_SCAN_DIR` del PHP-FPM de cheka para esta versión.
pub fn scan_dir_env(layout: &Layout, v: &str) -> String {
    let conf_d = php::config_dir(layout, v).join("conf.d");
    let ext = ext_dir(layout, v);
    if is_apt(v) && ext.is_dir() {
        format!("{}:{}", ext.display(), conf_d.display())
    } else {
        // ":" al inicio: también la carpeta compilada (la de apt, o ninguna en los estáticos)
        format!(":{}", conf_d.display())
    }
}

// ---------------------------------------------------------------- ajustes ----

/// Una directiva de php.ini segura de escribir tal cual.
pub fn validate_ini(key: &str, value: &str) -> Result<()> {
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        bail!("Directiva inválida: '{key}'");
    }
    if value.chars().any(|c| matches!(c, '\n' | '\r' | ';' | '"' | '\'' | '=' | '[' | ']' | '{' | '}' | '$')) {
        bail!("Valor inválido para {key}: no puede contener saltos de línea ni ; \" ' = [ ] {{ }} $");
    }
    Ok(())
}

pub fn render_user_ini(v: &str, settings: &PhpSettings) -> Option<String> {
    if settings.ini.is_empty() {
        return None;
    }
    let mut s = format!("; Generado por cheka desde cheka.toml ([php.\"{v}\".ini]). No editar a mano.\n");
    for (k, val) in &settings.ini {
        s.push_str(&format!("{k} = {val}\n"));
    }
    Some(s)
}

// ------------------------------------------------------------- extensiones ----

/// Nombre de la extensión a partir de un archivo `NN-nombre.ini`.
fn ext_name(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".ini")?;
    Some(match stem.split_once('-') {
        Some((prio, name)) if prio.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
        _ => stem.to_string(),
    })
}

fn priority(ini: &Path) -> String {
    fs::read_to_string(ini)
        .ok()
        .and_then(|t| t.lines().find_map(|l| l.trim().strip_prefix("; priority=").map(|p| p.trim().to_string())))
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| "20".into())
}

/// Extensiones activas en el PHP-FPM del sistema: nombre → (archivo, destino).
fn system_extensions(v: &str) -> BTreeMap<String, (String, PathBuf)> {
    let mut out = BTreeMap::new();
    if let Ok(rd) = fs::read_dir(system_conf_d(v)) {
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            if let Some(name) = ext_name(&file) {
                let target = fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
                out.insert(name, (file, target));
            }
        }
    }
    out
}

/// Extensiones que el sistema tiene instaladas (activas o no).
pub fn available_extensions(v: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(rd) = fs::read_dir(mods_available(v)) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_string_lossy().strip_suffix(".ini") {
                set.insert(n.to_string());
            }
        }
    }
    set
}

/// Enlaces que debe tener `ext.d`: lo activo en el sistema, menos lo desactivado en cheka,
/// más lo activado en cheka que el sistema no activa.
fn desired_ext_links(v: &str, overrides: &BTreeMap<String, bool>) -> BTreeMap<String, PathBuf> {
    let mut links = BTreeMap::new();
    let system = system_extensions(v);
    for (name, (file, target)) in &system {
        if overrides.get(name) != Some(&false) {
            links.insert(file.clone(), target.clone());
        }
    }
    for (name, on) in overrides {
        let ini = mods_available(v).join(format!("{name}.ini"));
        if *on && !system.contains_key(name) && ini.is_file() {
            links.insert(format!("{}-{name}.ini", priority(&ini)), ini);
        }
    }
    links
}

fn current_ext_links(dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut links = BTreeMap::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(t) = fs::read_link(e.path()) {
                links.insert(e.file_name().to_string_lossy().into_owned(), t);
            }
        }
    }
    links
}

/// ¿Activa esta extensión el PHP de cheka? (con los cambios del usuario aplicados)
pub fn extension_enabled(v: &str, name: &str, overrides: &BTreeMap<String, bool>) -> bool {
    desired_ext_links(v, overrides).keys().any(|f| ext_name(f).as_deref() == Some(name))
}

/// ¿La activa el sistema por su cuenta? (sirve para no guardar cambios redundantes)
pub fn system_enables(v: &str, name: &str) -> bool {
    system_extensions(v).contains_key(name)
}

// ----------------------------------------------------------------- aplicar ----

/// Aplica los ajustes y extensiones de una versión (root). Devuelve si cambió algo, para
/// reiniciar ese PHP-FPM. Solo actúa sobre versiones ya configuradas por cheka.
pub fn apply(ctx: &Ctx, v: &str) -> Result<bool> {
    let l = &ctx.layout;
    if !php::config_dir(l, v).is_dir() {
        return Ok(false);
    }
    let settings = ctx.state.php.get(v).cloned().unwrap_or_default();
    let mut changed = false;

    let ini_path = user_ini_path(l, v);
    match render_user_ini(v, &settings) {
        Some(text) => {
            if fs::read_to_string(&ini_path).ok().as_deref() != Some(text.as_str()) {
                fs::create_dir_all(ini_path.parent().unwrap())?;
                fs::write(&ini_path, text)?;
                changed = true;
            }
        }
        None => {
            if ini_path.exists() {
                fs::remove_file(&ini_path)?;
                changed = true;
            }
        }
    }

    if is_apt(v) {
        let dir = ext_dir(l, v);
        let desired = desired_ext_links(v, &settings.extensions);
        if current_ext_links(&dir) != desired || !dir.is_dir() {
            if dir.is_dir() {
                fs::remove_dir_all(&dir)?;
            }
            fs::create_dir_all(&dir)?;
            for (file, target) in &desired {
                symlink(target, dir.join(file))?;
            }
            changed = true;
        }
    }
    Ok(changed)
}

// ------------------------------------------------------------- información ----

#[derive(Debug, Serialize)]
pub struct ExtensionInfo {
    pub name: String,
    /// Cargada en el PHP-FPM ahora mismo.
    pub loaded: bool,
    /// Activa según la configuración de cheka (puede diferir de `loaded` hasta reiniciar).
    pub enabled: bool,
    /// Se puede activar o desactivar (solo PHP de apt).
    pub toggleable: bool,
}

#[derive(Debug, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    /// Cambiado por el usuario en cheka.toml.
    pub custom: bool,
}

#[derive(Debug, Serialize)]
pub struct PhpDetail {
    pub version: String,
    pub full_version: String,
    pub source: &'static str,
    pub fpm_bin: String,
    pub cli_bin: String,
    pub ini_file: String,
    pub ini_files: Vec<String>,
    pub can_manage_extensions: bool,
    pub extensions: Vec<ExtensionInfo>,
    /// Paquetes de extensiones que se pueden instalar con apt (solo PHP de apt).
    pub installable: Vec<String>,
    pub settings: Vec<Setting>,
}

/// Ejecuta el PHP-FPM de la versión con su misma configuración (lo que ven los sitios).
fn fpm_output(ctx: &Ctx, v: &str, flag: &str) -> Result<String> {
    let dir = php::config_dir(&ctx.layout, v);
    let out = Command::new(php::fpm_bin(&ctx.layout, v))
        .env("PHP_INI_SCAN_DIR", scan_dir_env(&ctx.layout, v))
        .env("LC_ALL", "C")
        .arg("-c")
        .arg(dir.join("php.ini"))
        .arg(flag)
        .stderr(Stdio::null())
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn apt_installable(v: &str) -> Vec<String> {
    let names = |cmd: &mut Command| -> BTreeSet<String> {
        cmd.env("LC_ALL", "C")
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let prefix = format!("php{v}-");
    let all = names(Command::new("apt-cache").args(["pkgnames", &prefix]));
    let installed = names(Command::new("dpkg-query").args(["-W", "-f=${Package}\\n", &format!("{prefix}*")]));
    all.difference(&installed)
        .filter_map(|p| p.strip_prefix(&prefix).map(str::to_string))
        .filter(|s| !NOT_EXTENSIONS.contains(&s.as_str()))
        .collect()
}

pub fn detail(ctx: &Ctx, v: &str) -> Result<PhpDetail> {
    php::validate(v)?;
    if !php::installed(&ctx.layout, v) {
        bail!("PHP {v} no está instalado (instálalo con: cheka php:install {v})");
    }
    let info = fpm_output(ctx, v, "-i")?;
    let modules = fpm_output(ctx, v, "-m")?;
    let loaded: BTreeSet<String> = modules
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('['))
        .map(|l| l.to_lowercase().replace("zend opcache", "opcache"))
        .collect();

    // phpinfo en texto: "clave => valor local => valor maestro"
    let mut values = BTreeMap::new();
    let (mut full_version, mut ini_file, mut ini_files) = (String::new(), String::new(), Vec::new());
    let mut lines = info.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(val) = line.strip_prefix("PHP Version => ") {
            // phpinfo repite "PHP Version"; vale la primera
            if full_version.is_empty() {
                full_version = val.trim().to_string();
            }
        } else if let Some(val) = line.strip_prefix("Loaded Configuration File => ") {
            ini_file = val.to_string();
        } else if let Some(first) = line.strip_prefix("Additional .ini files parsed => ") {
            let mut add = |s: &str| {
                let s = s.trim().trim_end_matches(',');
                if !s.is_empty() && s != "(none)" {
                    ini_files.push(s.to_string());
                }
            };
            add(first);
            while let Some(next) = lines.peek().filter(|l| !l.contains(" => ") && !l.trim().is_empty()) {
                add(next);
                lines.next();
            }
        } else if let Some((k, rest)) = line.split_once(" => ")
            && let Some((local, _master)) = rest.split_once(" => ")
        {
            values.entry(k.trim().to_string()).or_insert_with(|| local.trim().to_string());
        }
    }

    let st = ctx.state.php.get(v).cloned().unwrap_or_default();
    let apt = is_apt(v);
    let extensions = if apt {
        available_extensions(v)
            .into_iter()
            .map(|name| ExtensionInfo {
                loaded: loaded.contains(&name),
                enabled: extension_enabled(v, &name, &st.extensions),
                toggleable: true,
                name,
            })
            .collect()
    } else {
        loaded
            .iter()
            .filter(|m| !["core", "standard", "date", "hash", "json", "pcre", "random", "reflection", "spl", "cgi-fcgi"].contains(&m.as_str()))
            .map(|name| ExtensionInfo { name: name.clone(), loaded: true, enabled: true, toggleable: false })
            .collect()
    };

    let mut keys: Vec<String> = COMMON_SETTINGS.iter().map(|s| s.to_string()).collect();
    keys.extend(st.ini.keys().filter(|k| !COMMON_SETTINGS.contains(&k.as_str())).cloned());
    let settings = keys
        .into_iter()
        .map(|key| Setting {
            value: values.get(&key).cloned().unwrap_or_else(|| "(sin definir)".into()),
            custom: st.ini.contains_key(&key),
            key,
        })
        .collect();

    Ok(PhpDetail {
        version: v.to_string(),
        full_version: if full_version.is_empty() { v.to_string() } else { full_version },
        source: if apt { "apt" } else { "static" },
        fpm_bin: php::fpm_bin(&ctx.layout, v).display().to_string(),
        cli_bin: php::cli_bin(&ctx.layout, v).display().to_string(),
        ini_file,
        ini_files,
        can_manage_extensions: apt,
        extensions,
        installable: if apt { apt_installable(v) } else { Vec::new() },
        settings,
    })
}

// ----------------------------------------------------------- actualizaciones ----

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub source: &'static str,
    pub current: String,
    pub latest: Option<String>,
    pub available: bool,
}

/// Última versión X.Y.Z publicada de cada X.Y en static-php-cli (una sola descarga del índice).
pub fn static_latest() -> BTreeMap<String, String> {
    let listing = Command::new("curl")
        .args(["-fsSL", "--max-time", "15", &format!("{}/", crate::commands::STATIC_URL)])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let re = regex::Regex::new(&format!(r"php-(\d+\.\d+)\.(\d+)-fpm-linux-{}\.tar\.gz", std::env::consts::ARCH)).unwrap();
    let mut best: BTreeMap<String, u32> = BTreeMap::new();
    for c in re.captures_iter(&listing) {
        if let Ok(p) = c[2].parse::<u32>() {
            let e = best.entry(c[1].to_string()).or_insert(p);
            *e = (*e).max(p);
        }
    }
    best.into_iter().map(|(v, p)| (v.clone(), format!("{v}.{p}"))).collect()
}

fn apt_policy(v: &str) -> (String, Option<String>) {
    let out = Command::new("apt-cache")
        .args(["policy", &format!("php{v}-fpm")])
        .env("LC_ALL", "C")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let field = |name: &str| {
        out.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim().to_string())).filter(|v| v != "(none)")
    };
    (field("Installed:").unwrap_or_default(), field("Candidate:"))
}

pub fn updates(ctx: &Ctx) -> Vec<UpdateInfo> {
    let installed: Vec<&str> = php::SUPPORTED.iter().copied().filter(|v| php::installed(&ctx.layout, v)).collect();
    let latest = if installed.iter().any(|v| !is_apt(v)) { static_latest() } else { BTreeMap::new() };
    installed
        .into_iter()
        .map(|v| {
            if is_apt(v) {
                let (current, candidate) = apt_policy(v);
                let available = candidate.as_ref().is_some_and(|c| *c != current);
                UpdateInfo { version: v.into(), source: "apt", current, latest: candidate, available }
            } else {
                let current = fs::read_to_string(ctx.layout.opt.join(format!("php/{v}/VERSION")))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let newest = latest.get(v).cloned();
                let available = newest.as_ref().is_some_and(|n| version_newer(n, &current));
                UpdateInfo { version: v.into(), source: "static", current, latest: newest, available }
            }
        })
        .collect()
}

/// ¿`a` es más nueva que `b`? (X.Y.Z numérico)
fn version_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_de_extension() {
        assert_eq!(ext_name("20-xdebug.ini").as_deref(), Some("xdebug"));
        assert_eq!(ext_name("10-pdo_mysql.ini").as_deref(), Some("pdo_mysql"));
        assert_eq!(ext_name("opcache.ini").as_deref(), Some("opcache"));
        assert_eq!(ext_name("leeme.txt"), None);
    }

    #[test]
    fn valida_directivas() {
        assert!(validate_ini("upload_max_filesize", "512M").is_ok());
        assert!(validate_ini("error_reporting", "E_ALL & ~E_DEPRECATED").is_ok());
        assert!(validate_ini("date.timezone", "America/Mexico_City").is_ok());
        assert!(validate_ini("memory_limit", "1G\nextension=evil.so").is_err());
        assert!(validate_ini("x;y", "1").is_err());
        assert!(validate_ini("a", "b; comentario").is_err());
    }

    #[test]
    fn compara_versiones() {
        assert!(version_newer("8.2.33", "8.2.32"));
        assert!(version_newer("8.2.100", "8.2.99"));
        assert!(!version_newer("8.2.32", "8.2.32"));
    }

    #[test]
    fn ini_generado() {
        let mut s = PhpSettings::default();
        assert!(render_user_ini("8.5", &s).is_none());
        s.ini.insert("upload_max_filesize".into(), "512M".into());
        s.ini.insert("memory_limit".into(), "1G".into());
        let text = render_user_ini("8.5", &s).unwrap();
        assert!(text.ends_with("memory_limit = 1G\nupload_max_filesize = 512M\n"), "{text}");
    }
}
