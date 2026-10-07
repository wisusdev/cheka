//! Windows: PHP (zips NTS de windows.php.net) y Apache Lounge con `mod_fcgid`, todo en
//! `%ProgramData%\cheka` (docs/ARQUITECTURA.md §6 y §8.6, hito 3.2).
//!
//! - **PHP:** `php\X.Y\` con el contenido del zip. El `php.ini` de cheka va en esa misma
//!   carpeta: PHP en Windows lo busca junto al ejecutable, así que `php.exe` (CLI) y
//!   `php-cgi.exe` (Apache) usan la misma configuración sin variables de entorno. Los
//!   ajustes del usuario (`conf.d\99-cheka.ini`) se agregan al final del mismo archivo.
//! - **Apache:** `apache\` con el contenido de `Apache24\` y `mod_fcgid.so`; corre como el
//!   servicio `cheka-apache`. Su `httpd.conf` incluye `cheka.conf` y `cheka-sites.conf`.
//!
//! Descargas con `curl.exe` y descompresión con `tar.exe` (bsdtar), ambos incluidos en
//! Windows 10 y 11; los sha256 se verifican con `Get-FileHash`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;

use crate::layout::Layout;
use crate::render::{self, WindowsPhpIni};
use crate::util::{mkdir, write};
use crate::{Ctx, php, phpconf, system, ui};

pub const PHP_RELEASES: &str = "https://windows.php.net/downloads/releases/releases.json";
pub const PHP_DOWNLOADS: &str = "https://windows.php.net/downloads/releases";
pub const APACHE_LOUNGE: &str = "https://www.apachelounge.com";
pub const CACERT_URL: &str = "https://curl.se/ca/cacert.pem";
pub const APACHE_SERVICE: &str = "cheka-apache";

/// Extensiones que se activan si la versión trae su DLL (lo que piden WordPress, Laravel y
/// CodeIgniter). El resto se activa a mano con `cheka php:ini`.
const DEFAULT_EXTENSIONS: [&str; 13] = [
    "curl", "exif", "fileinfo", "gd", "intl", "mbstring", "mysqli", "openssl", "pdo_mysql", "pdo_sqlite", "sodium",
    "sqlite3", "zip",
];

// ---------------------------------------------------------------- utilidades ----

fn system32(exe: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join("System32").join(exe)
}

fn curl() -> Command {
    let mut cmd = Command::new(system32("curl.exe"));
    // Apache Lounge rechaza a veces clientes sin User-Agent.
    cmd.args(["-fL", "-A", concat!("cheka/", env!("CARGO_PKG_VERSION"))]);
    cmd
}

fn fetch_text(url: &str) -> Result<String> {
    let out = curl().args(["-sS", "--max-time", "30", url]).output().context("No pude ejecutar curl.exe")?;
    if !out.status.success() {
        bail!("No pude descargar {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let ok = curl().arg("--progress-bar").arg("-o").arg(dest).arg(url).status()?.success();
    if !ok {
        bail!("No pude descargar {url}");
    }
    Ok(())
}

fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

fn sha256(file: &Path) -> Result<String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("(Get-FileHash -Algorithm SHA256 -LiteralPath {}).Hash", ps_quote(file)))
        .output()?;
    let hash = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
    if !out.status.success() || hash.len() != 64 {
        bail!("No pude calcular el sha256 de {}", file.display());
    }
    Ok(hash)
}

fn verify(file: &Path, expected: &str) -> Result<()> {
    let got = sha256(file)?;
    if !got.eq_ignore_ascii_case(expected.trim()) {
        bail!("El sha256 de {} no coincide (esperado {expected}, obtenido {got})", file.display());
    }
    Ok(())
}

fn unzip(zip: &Path, dest: &Path) -> Result<()> {
    mkdir(dest)?;
    let ok = Command::new(system32("tar.exe")).arg("-xf").arg(zip).arg("-C").arg(dest).status()?.success();
    if !ok {
        bail!("No pude descomprimir {}", zip.display());
    }
    Ok(())
}

/// Reemplaza `dest` por `new` (misma unidad). Si `dest` está en uso (p. ej. un php-cgi.exe
/// corriendo), `rename` falla: el llamador debe detener Apache antes.
fn replace_dir(new: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        let old = dest.with_extension("anterior");
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
        fs::rename(dest, &old).with_context(|| format!("No pude reemplazar {} (¿está en uso?)", dest.display()))?;
        fs::rename(new, dest)?;
        let _ = fs::remove_dir_all(&old);
    } else {
        fs::rename(new, dest)?;
    }
    Ok(())
}

/// Ejecuta `f` con Apache detenido (si estaba corriendo) y lo vuelve a iniciar.
fn with_apache_stopped<T>(ctx: &Ctx, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let running = !ctx.layout.is_test() && ctx.sys.is_active(APACHE_SERVICE);
    if running {
        ui::info("Deteniendo Apache mientras se reemplazan los archivos…");
        ctx.sys.disable(APACHE_SERVICE, true).ok();
    }
    let result = f();
    if running {
        ctx.sys.enable(APACHE_SERVICE, true)?;
    }
    result
}

// ----------------------------------------------------------------------- PHP ----

#[derive(Debug, Clone)]
pub struct PhpRelease {
    /// X.Y.Z
    pub full: String,
    /// Nombre del zip NTS x64 en windows.php.net.
    pub zip: String,
    pub sha256: String,
}

/// Última versión publicada de cada X.Y (NTS x64), según `releases.json`.
pub fn php_latest() -> Result<BTreeMap<String, PhpRelease>> {
    let json: serde_json::Value = serde_json::from_str(&fetch_text(PHP_RELEASES)?)?;
    let mut out = BTreeMap::new();
    for (minor, rel) in json.as_object().ok_or_else(|| anyhow!("releases.json inesperado"))? {
        let Some(full) = rel["version"].as_str() else { continue };
        let Some(build) = rel
            .as_object()
            .and_then(|o| o.iter().find(|(k, _)| k.starts_with("nts-") && k.ends_with("-x64")).map(|(_, v)| v))
        else {
            continue;
        };
        if let (Some(zip), Some(sha)) = (build["zip"]["path"].as_str(), build["zip"]["sha256"].as_str()) {
            out.insert(minor.clone(), PhpRelease { full: full.into(), zip: zip.into(), sha256: sha.into() });
        }
    }
    Ok(out)
}

pub fn php_dir(layout: &Layout, v: &str) -> PathBuf {
    layout.opt.join("php").join(v)
}

/// Versión X.Y.Z instalada (archivo `VERSION`).
pub fn php_installed_version(layout: &Layout, v: &str) -> Option<String> {
    fs::read_to_string(php_dir(layout, v).join("VERSION")).ok().map(|s| s.trim().to_string())
}

/// Descarga la última X.Y.Z de PHP `v`, verifica su sha256 y la deja en `php\X.Y`.
/// Devuelve la versión completa instalada.
pub fn download_php(ctx: &Ctx, v: &str) -> Result<String> {
    ui::info(format!("Buscando la última versión de PHP {v} en windows.php.net…"));
    let rel = php_latest()?.remove(v).ok_or_else(|| anyhow!("windows.php.net no publica PHP {v} (NTS x64)"))?;
    let parent = ctx.layout.opt.join("php");
    mkdir(&parent)?;
    // En la misma unidad que el destino, para poder renombrar.
    let tmp = tempfile::tempdir_in(&parent)?;
    let zip = tmp.path().join(&rel.zip);
    ui::info(format!("Descargando PHP {} (NTS x64)…", rel.full));
    download(&format!("{PHP_DOWNLOADS}/{}", rel.zip), &zip)?;
    verify(&zip, &rel.sha256)?;
    let staging = tmp.path().join("php");
    unzip(&zip, &staging)?;
    write(&staging.join("VERSION"), &format!("{}\n", rel.full))?;
    let dest = php_dir(&ctx.layout, v);
    with_apache_stopped(ctx, || replace_dir(&staging, &dest))?;
    write_php_ini(ctx, v)?;
    Ok(rel.full)
}

// ---------------------------------------------------------------- extensiones ----

/// Extensiones que se cargan con `zend_extension` en vez de `extension`.
const ZEND_EXTENSIONS: [&str; 2] = ["opcache", "xdebug"];
pub const PECL_RELEASES: &str = "https://downloads.php.net/~windows/pecl/releases";
/// Extensiones de PECL que ofrece `php:info` para instalar (todas publican DLL NTS x64
/// para 8.0–8.5). Cualquier otra de PECL también se puede instalar por nombre.
const PECL_SUGGESTED: [&str; 11] =
    ["apcu", "igbinary", "imagick", "memcache", "mongodb", "msgpack", "pcov", "redis", "ssh2", "xdebug", "yaml"];

fn ext_dir(layout: &Layout, v: &str) -> PathBuf {
    php_dir(layout, v).join("ext")
}

/// Extensiones que trae (o a las que se agregó) esta versión: `ext\php_*.dll`.
pub fn available_extensions(layout: &Layout, v: &str) -> std::collections::BTreeSet<String> {
    fs::read_dir(ext_dir(layout, v))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            Some(n.strip_prefix("php_")?.strip_suffix(".dll")?.to_string())
        })
        .collect()
}

/// ¿La activa cheka por defecto? (el equivalente a lo que activa el paquete de apt)
pub fn default_enabled(layout: &Layout, v: &str, name: &str) -> bool {
    (DEFAULT_EXTENSIONS.contains(&name) || name == "opcache")
        && ext_dir(layout, v).join(format!("php_{name}.dll")).is_file()
}

/// Activa con los cambios de `cheka.toml` aplicados.
pub fn extension_enabled(layout: &Layout, v: &str, name: &str, overrides: &BTreeMap<String, bool>) -> bool {
    ext_dir(layout, v).join(format!("php_{name}.dll")).is_file()
        && overrides.get(name).copied().unwrap_or_else(|| default_enabled(layout, v, name))
}

/// Extensiones sugeridas de PECL que esta versión aún no tiene.
pub fn pecl_installable(layout: &Layout, v: &str) -> Vec<String> {
    let have = available_extensions(layout, v);
    PECL_SUGGESTED.iter().filter(|e| !have.contains(**e)).map(|e| e.to_string()).collect()
}

/// `php:ext <v> install <ext>` en Windows: la DLL de PECL para esta versión de PHP (NTS
/// x64), el equivalente a `apt install phpX.Y-<ext>`. Busca la versión estable más nueva
/// que tenga compilación para esta versión de PHP. Devuelve la versión instalada.
pub fn install_pecl(ctx: &Ctx, v: &str, name: &str) -> Result<String> {
    let l = &ctx.layout;
    ui::info(format!("Buscando {name} en PECL para PHP {v}…"));
    let listing = fetch_text(&format!("{PECL_RELEASES}/{name}/"))
        .map_err(|_| anyhow!("PECL no publica DLL de Windows para '{name}'"))?;
    let mut versions: Vec<String> = Regex::new(r#"href="([0-9][0-9.]*)/""#)?
        .captures_iter(&listing)
        .map(|c| c[1].to_string())
        .collect();
    let key = |s: &String| s.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    versions.sort_by_key(key);
    let zip_re = |ver: &str| {
        Regex::new(&format!(
            r"php_{}-{}-{}-nts-v[sc]\d+-x64\.zip",
            regex::escape(name),
            regex::escape(ver),
            regex::escape(v)
        ))
    };
    let mut found = None;
    for ver in versions.iter().rev().take(6) {
        let dir = fetch_text(&format!("{PECL_RELEASES}/{name}/{ver}/")).unwrap_or_default();
        if let Some(m) = zip_re(ver)?.find(&dir) {
            found = Some((ver.clone(), m.as_str().to_string()));
            break;
        }
    }
    let (ver, file) = found.ok_or_else(|| anyhow!("No hay una DLL de {name} para PHP {v} (NTS x64) en PECL"))?;
    let tmp = tempfile::tempdir()?;
    let zip = tmp.path().join(&file);
    ui::info(format!("Descargando {name} {ver}…"));
    download(&format!("{PECL_RELEASES}/{name}/{ver}/{file}"), &zip)?;
    let x = tmp.path().join("x");
    unzip(&zip, &x)?;
    let dll = x.join(format!("php_{name}.dll"));
    if !dll.is_file() {
        bail!("El zip de {name} no trae php_{name}.dll");
    }
    // La extensión va a ext\; las DLL que necesita (p. ej. las de ImageMagick), junto a
    // php.exe, que es donde Windows las busca.
    let dir = php_dir(l, v);
    with_apache_stopped(ctx, || {
        fs::copy(&dll, ext_dir(l, v).join(format!("php_{name}.dll")))?;
        for e in fs::read_dir(&x)?.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".dll") && !n.starts_with("php_") {
                fs::copy(e.path(), dir.join(e.file_name()))?;
            }
        }
        Ok(())
    })?;
    Ok(ver)
}

/// Certificados raíz para cURL y OpenSSL de PHP (Windows no expone los suyos a OpenSSL).
pub fn cacert(layout: &Layout) -> PathBuf {
    layout.etc.join("cacert.pem")
}

fn ensure_cacert(layout: &Layout) -> Result<()> {
    let path = cacert(layout);
    if !path.is_file() {
        ui::info("Descargando certificados raíz (cacert.pem de curl.se)…");
        mkdir(&layout.etc)?;
        download(CACERT_URL, &path)?;
    }
    Ok(())
}

/// Ruta del php.ini que usan `php.exe` y `php-cgi.exe` de la versión `v`.
pub fn php_ini_path(layout: &Layout, v: &str) -> PathBuf {
    php_dir(layout, v).join("php.ini")
}

/// Escribe el php.ini de la versión: el de cheka más los ajustes del usuario
/// (`99-cheka.ini`). Devuelve si cambió.
pub fn write_php_ini(ctx: &Ctx, v: &str) -> Result<bool> {
    let l = &ctx.layout;
    let dir = php_dir(l, v);
    let ext_dir = dir.join("ext");
    // Lo que activa cheka por defecto, con los cambios de `cheka.toml` (`php:ext`). Desde
    // PHP 8.5 OPcache viene incluido: no hay DLL y no se carga aparte.
    let overrides = ctx.state.php.get(v).map(|s| s.extensions.clone()).unwrap_or_default();
    let (zend, extensions): (Vec<String>, Vec<String>) = available_extensions(l, v)
        .into_iter()
        .filter(|e| extension_enabled(l, v, e, &overrides))
        .partition(|e| ZEND_EXTENSIONS.contains(&e.as_str()));
    let mut text = render::windows_php_ini(
        l,
        &WindowsPhpIni {
            v,
            ext_dir: &ext_dir,
            extensions: &extensions,
            zend_extensions: &zend,
            cacert: &cacert(l),
            tz: &php::timezone(),
        },
    );
    if let Ok(user) = fs::read_to_string(phpconf::user_ini_path(l, v)) {
        text.push_str("\n; Ajustes de cheka.toml ([php.\"");
        text.push_str(v);
        text.push_str("\"])\n");
        text.push_str(&user);
    }
    let path = php_ini_path(l, v);
    if fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    write(&path, &text)?;
    Ok(true)
}

/// `php:install <versión>` en Windows (administrador): descarga si hace falta y
/// (re)genera la configuración.
pub fn php_install(ctx: &Ctx, v: &str) -> Result<()> {
    let l = &ctx.layout;
    if !php::installed(l, v) {
        download_php(ctx, v)?;
    }
    ensure_cacert(l)?;
    mkdir(&l.log_dir)?;
    mkdir(&php::config_dir(l, v).join("conf.d"))?;
    phpconf::apply(ctx, v)?;
    write_php_ini(ctx, v)?;
    ui::ok(format!("PHP {v} listo ({})", php::fpm_bin(l, v).display()));
    Ok(())
}

// ------------------------------------------------------------------- MariaDB ----

pub const MARIADB_SERVICE: &str = "MariaDB";

/// `mariadb.exe` / `mariadb-dump.exe` de la instalación más reciente (`C:\Program Files\
/// MariaDB X.Y\bin`), o el nombre a secas para buscarlo en el PATH.
pub fn mariadb_bin(program: &str) -> PathBuf {
    let exe = format!("{program}.exe");
    let base = PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into()));
    let mut dirs: Vec<PathBuf> = fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("MariaDB ")))
        .filter(|p| p.join("bin").join(&exe).is_file())
        .collect();
    // "MariaDB 13.0" > "MariaDB 11.8": comparar la versión numéricamente.
    dirs.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().trim_start_matches("MariaDB ").split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    dirs.last().map(|d| d.join("bin").join(&exe)).unwrap_or_else(|| PathBuf::from(exe))
}

/// Instala MariaDB con winget si no hay un servicio `MariaDB`, y crea el usuario de `[db]`
/// para los proyectos. El MSI deja a `root` sin contraseña y solo local.
pub fn install_mariadb(ctx: &Ctx) -> Result<()> {
    if system::windows_service_state(MARIADB_SERVICE).is_none() {
        ui::info("Instalando MariaDB con winget (puede tardar unos minutos)…");
        let ok = Command::new("winget")
            .args(["install", "-e", "--id", "MariaDB.Server", "--silent"])
            .args(["--accept-source-agreements", "--accept-package-agreements"])
            .args(["--custom", &format!("SERVICENAME={MARIADB_SERVICE} UTF8=1")])
            .status()
            .is_ok_and(|s| s.success());
        if !ok || system::windows_service_state(MARIADB_SERVICE).is_none() {
            bail!("No pude instalar MariaDB con winget (prueba: winget install MariaDB.Server)");
        }
    }
    if !ctx.sys.is_active(MARIADB_SERVICE) {
        ctx.sys.enable(MARIADB_SERVICE, true)?;
    }
    let (user, pass) = (&ctx.state.db_user, &ctx.state.db_password);
    if [user, pass].iter().any(|v| v.contains(['\'', '\\']) || v.is_empty()) {
        bail!("[db] en cheka.toml: el usuario y la contraseña no pueden estar vacíos ni tener ' o \\");
    }
    let sql = format!(
        "CREATE USER IF NOT EXISTS '{user}'@'localhost' IDENTIFIED BY '{pass}';\n\
         CREATE USER IF NOT EXISTS '{user}'@'127.0.0.1' IDENTIFIED BY '{pass}';\n\
         GRANT ALL PRIVILEGES ON *.* TO '{user}'@'localhost' WITH GRANT OPTION;\n\
         GRANT ALL PRIVILEGES ON *.* TO '{user}'@'127.0.0.1' WITH GRANT OPTION;\n\
         FLUSH PRIVILEGES;\n"
    );
    let ok = Command::new(mariadb_bin("mariadb"))
        .args(["-u", "root", "-h", "127.0.0.1", "-e", &sql])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!(
            "No pude crear el usuario '{user}' en MariaDB como root sin contraseña. Si tu root tiene \
             contraseña, créalo a mano:\n  mariadb -u root -p -e \"CREATE USER '{user}'@'localhost' IDENTIFIED BY '{pass}'; \
             GRANT ALL ON *.* TO '{user}'@'localhost' WITH GRANT OPTION;\""
        );
    }
    ui::ok(format!("MariaDB listo; usuario '{user}'/'{pass}' para tus proyectos"));
    Ok(())
}

// -------------------------------------------------------------------- mkcert ----

pub const MKCERT_URL: &str = "https://dl.filippo.io/mkcert/latest?for=windows/amd64";

/// CA de mkcert del usuario (`%LOCALAPPDATA%\mkcert`); la misma que usa `cheka secure`.
fn caroot(ctx: &Ctx) -> PathBuf {
    ctx.id.home.join(r"AppData\Local\mkcert")
}

/// Descarga mkcert, crea la CA del usuario y la instala en el almacén de la **máquina**
/// (`certutil`, como administrador): así no aparece el diálogo de confirmación del almacén
/// del usuario, y Chrome, Edge y Firefox (que usa el almacén de Windows) la aceptan.
pub fn install_mkcert(ctx: &Ctx) -> Result<()> {
    let l = &ctx.layout;
    let exe = l.bin.join("mkcert.exe");
    if !exe.is_file() {
        ui::info("Descargando mkcert…");
        download(MKCERT_URL, &exe)?;
    }
    let root = caroot(ctx);
    mkdir(&root)?;
    // TRUST_STORES=nss: crea la CA sin tocar el almacén del usuario (no hay NSS en Windows).
    let ok = Command::new(&exe)
        .arg("-install")
        .env("CAROOT", &root)
        .env("TRUST_STORES", "nss")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let ca = root.join("rootCA.pem");
    if !ok || !ca.is_file() {
        bail!("mkcert no pudo crear la CA local en {}", root.display());
    }
    let out = Command::new(system32("certutil.exe")).args(["-addstore", "-f", "Root"]).arg(&ca).output()?;
    if !out.status.success() {
        bail!("certutil no pudo instalar la CA: {}", String::from_utf8_lossy(&out.stdout).trim());
    }
    ui::ok("CA local de mkcert instalada en Windows (Chrome, Edge y Firefox)");
    Ok(())
}

// ------------------------------------------------------------ daemon (servicio) ----

/// Registra (o actualiza) el servicio `cheka`: `cheka.exe daemon` como LocalSystem, con
/// arranque automático. No lo inicia.
pub fn register_daemon(exe: &Path) -> Result<()> {
    use windows_service::service::{ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let name = crate::daemon::SERVICE_NAME;
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: name.into(),
        display_name: "cheka".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.to_path_buf(),
        launch_arguments: vec!["daemon".into()],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::STOP;
    let service = match manager.open_service(name, access) {
        Ok(s) => {
            s.change_config(&info)?;
            s
        }
        Err(_) => manager.create_service(&info, access)?,
    };
    service.set_description("cheka: publica los sitios de ~/Sites y atiende a la CLI")?;
    Ok(())
}

/// Elimina el servicio `cheka` (detenido antes por el llamador).
pub fn unregister_daemon() -> Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    if let Ok(s) = manager.open_service(crate::daemon::SERVICE_NAME, ServiceAccess::DELETE) {
        s.delete()?;
    }
    Ok(())
}

/// SID del usuario (para la ACL de la named pipe del daemon).
pub fn user_sid(user: &str) -> Result<String> {
    let script = format!(
        "(New-Object System.Security.Principal.NTAccount('{}')).Translate([System.Security.Principal.SecurityIdentifier]).Value",
        user.replace('\'', "''")
    );
    let out = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output()?;
    let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || !sid.starts_with("S-1-") {
        bail!("No pude obtener el SID de '{user}'");
    }
    Ok(sid)
}

// ----------------------------------------------------------------------- DNS ----

const NRPT_COMMENT: &str = "cheka";

/// Regla NRPT `.test → 127.0.0.1`: Windows pregunta solo esos nombres al DNS del daemon
/// (equivale al `~test` de systemd-resolved en Linux). Idempotente.
pub fn install_nrpt() -> Result<()> {
    let tld = crate::layout::TLD;
    let script = format!(
        "Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{NRPT_COMMENT}' -or $_.Namespace -contains '.{tld}' }} | \
         Remove-DnsClientNrptRule -Force; \
         Add-DnsClientNrptRule -Namespace '.{tld}' -NameServers '127.0.0.1' -Comment '{NRPT_COMMENT}' | Out-Null; \
         Clear-DnsClientCache"
    );
    let out = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output()?;
    if !out.status.success() {
        bail!("No pude crear la regla DNS para .{tld}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn remove_nrpt() -> Result<()> {
    let script = format!(
        "Get-DnsClientNrptRule | Where-Object {{ $_.Comment -eq '{NRPT_COMMENT}' }} | Remove-DnsClientNrptRule -Force; \
         Clear-DnsClientCache"
    );
    let _ = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output()?;
    Ok(())
}

// --------------------------------------------------------------------- hosts ----

const HOSTS_BEGIN: &str = "# >>> cheka (generado por cheka, no editar este bloque)";
const HOSTS_END: &str = "# <<< cheka";

/// El archivo `hosts` sin el bloque de cheka.
fn hosts_without_block(text: &str) -> String {
    match (text.find(HOSTS_BEGIN), text.find(HOSTS_END)) {
        (Some(a), Some(b)) if b > a => {
            let mut s = text[..a].trim_end().to_string();
            let rest = text[b + HOSTS_END.len()..].trim_start_matches(['\r', '\n']);
            if !rest.is_empty() {
                s.push_str("\r\n");
                s.push_str(rest);
            }
            s
        }
        _ => text.trim_end().to_string(),
    }
}

/// Respaldo del DNS de cheka: cada sitio también va al archivo `hosts`, por si un navegador
/// usa DNS cifrado (DoH) y no respeta la regla NRPT. Los subdominios (`*.sitio.test`) los
/// resuelve el DNS del daemon. Devuelve si cambió el archivo.
pub fn sync_hosts(l: &Layout, sites: &[String]) -> Result<bool> {
    let path = l.hosts_file();
    let current = match fs::read(&path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| anyhow!("{} no está en UTF-8; no lo modifico", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("No pude leer {}", path.display())),
    };
    let mut text = hosts_without_block(&current);
    let names: Vec<String> = sites.iter().map(|s| format!("{s}.{}", crate::layout::TLD)).collect();
    if !text.is_empty() {
        text.push_str("\r\n\r\n");
    }
    text.push_str(HOSTS_BEGIN);
    text.push_str("\r\n");
    for n in &names {
        text.push_str(&format!("127.0.0.1 {n}\r\n"));
    }
    text.push_str(HOSTS_END);
    text.push_str("\r\n");
    if text == current {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        mkdir(dir)?;
    }
    write(&path, &text)?;
    Ok(true)
}

/// Quita el bloque de cheka del archivo `hosts` (`uninstall`).
pub fn remove_hosts_block(l: &Layout) -> Result<()> {
    let path = l.hosts_file();
    if let Ok(current) = fs::read_to_string(&path) {
        let text = hosts_without_block(&current) + "\r\n";
        if text != current {
            write(&path, &text)?;
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- Apache ----

pub fn apache_dir(layout: &Layout) -> PathBuf {
    layout.opt.join("apache")
}

pub fn httpd(layout: &Layout) -> PathBuf {
    apache_dir(layout).join(r"bin\httpd.exe")
}

/// Enlaces de la página de descargas de Apache Lounge: (httpd Win64, mod_fcgid win64).
fn apache_lounge_links() -> Result<(String, String)> {
    let page = fetch_text(&format!("{APACHE_LOUNGE}/download/"))?;
    let find = |re: &str| -> Option<String> {
        Regex::new(re).ok()?.find(&page).map(|m| format!("{APACHE_LOUNGE}{}", m.as_str()))
    };
    let httpd = find(r"/download/VS\d+/binaries/httpd-[0-9.]+-[0-9]+-Win64-VS\d+\.zip")
        .ok_or_else(|| anyhow!("No encontré Apache (Win64) en {APACHE_LOUNGE}/download/"))?;
    let fcgid = find(r"(?i)/download/VS\d+/modules/mod_fcgid-[0-9.]+-win64-VS\d+\.zip")
        .ok_or_else(|| anyhow!("No encontré mod_fcgid (win64) en {APACHE_LOUNGE}/download/"))?;
    Ok((httpd, fcgid))
}

/// Descarga Apache Lounge y mod_fcgid a `apache\` (si no están).
fn download_apache(ctx: &Ctx) -> Result<()> {
    let l = &ctx.layout;
    let (httpd_url, fcgid_url) = apache_lounge_links()?;
    let tmp = tempfile::tempdir_in(&l.opt)?;
    let name = httpd_url.rsplit('/').next().unwrap_or("httpd.zip").to_string();
    let zip = tmp.path().join(&name);
    ui::info(format!("Descargando Apache ({name})…"));
    download(&httpd_url, &zip)?;
    // Apache Lounge publica los hashes en un .txt junto al zip.
    let sums = fetch_text(&format!("{httpd_url}.txt"))?;
    let sha = Regex::new(r"SHA256-Checksum for:[^\n]*\n\s*([0-9A-Fa-f]{64})")?
        .captures(&sums)
        .map(|c| c[1].to_string())
        .ok_or_else(|| anyhow!("No encontré el sha256 de {name}"))?;
    verify(&zip, &sha)?;
    let staging = tmp.path().join("x");
    unzip(&zip, &staging)?;

    ui::info("Descargando mod_fcgid…");
    let fzip = tmp.path().join("mod_fcgid.zip");
    download(&fcgid_url, &fzip)?;
    let fdir = tmp.path().join("fcgid");
    unzip(&fzip, &fdir)?;
    let so = fdir.join("mod_fcgid.so");
    if !so.is_file() {
        bail!("El zip de mod_fcgid no trae mod_fcgid.so");
    }
    let apache = staging.join("Apache24");
    fs::copy(&so, apache.join(r"modules\mod_fcgid.so"))?;
    write(&apache.join("VERSION"), &format!("{name}\n"))?;
    with_apache_stopped(ctx, || replace_dir(&apache, &apache_dir(l)))
}

const BLOCK_BEGIN: &str = "# >>> cheka (generado, no editar)";
const BLOCK_END: &str = "# <<< cheka";
/// Módulos de Apache Lounge que vienen comentados en `httpd.conf`.
const APACHE_MODULES: [&str; 4] = ["rewrite", "headers", "ssl", "socache_shmcb"];

/// Ajusta `httpd.conf`: ruta de instalación, módulos e inclusión de la config de cheka.
fn configure_httpd_conf(l: &Layout) -> Result<()> {
    let path = apache_dir(l).join(r"conf\httpd.conf");
    let original = fs::read_to_string(&path).with_context(|| format!("No pude leer {}", path.display()))?;
    let mut text = original.clone();
    let root = render::slash(&apache_dir(l));
    text = Regex::new(r#"(?m)^Define SRVROOT .*$"#)?.replace(&text, format!("Define SRVROOT \"{root}\"").as_str()).into_owned();
    for m in APACHE_MODULES {
        let re = Regex::new(&format!(r"(?m)^#\s*(LoadModule {m}_module modules/mod_{m}\.so)"))?;
        text = re.replace(&text, "$1").into_owned();
    }
    if let (Some(a), Some(b)) = (text.find(BLOCK_BEGIN), text.find(BLOCK_END)) {
        text.replace_range(a..b + BLOCK_END.len(), "");
        text = text.trim_end().to_string();
        text.push('\n');
    }
    text.push_str(&format!(
        "\n{BLOCK_BEGIN}\nInclude \"{}\"\nInclude \"{}\"\n{BLOCK_END}\n",
        render::slash(&l.apache_conf),
        render::slash(&l.apache_site_conf)
    ));
    if text != original {
        write(&path, &text)?;
    }
    Ok(())
}

/// Entorno de los php-cgi.exe: mod_fcgid no hereda el del servicio.
fn fcgid_env(l: &Layout) -> Vec<(String, String)> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let tmp = render::slash(&l.opt.join("tmp"));
    let path = format!("{root}\\System32;{root};{root}\\System32\\Wbem").replace('\\', "/");
    vec![
        ("SystemRoot".into(), root.replace('\\', "/")),
        ("SystemDrive".into(), std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())),
        ("PATH".into(), path),
        ("TEMP".into(), tmp.clone()),
        ("TMP".into(), tmp),
    ]
}

/// Escribe `cheka.conf` y `cheka-sites.conf` de Apache.
pub fn write_apache_conf(l: &Layout) -> Result<()> {
    mkdir(&l.opt.join("tmp"))?;
    mkdir(&l.apache_sites)?;
    mkdir(&l.log_dir)?;
    if let Some(dir) = l.apache_conf.parent() {
        mkdir(dir)?;
    }
    write(&l.apache_conf, &render::windows_apache_conf(&fcgid_env(l)))?;
    write(&l.apache_site_conf, &render::windows_apache_site(l))
}

/// Instala (o reconfigura) Apache y su servicio. Idempotente.
pub fn install_apache(ctx: &Ctx) -> Result<()> {
    let l = &ctx.layout;
    if !httpd(l).is_file() {
        download_apache(ctx)?;
    }
    configure_httpd_conf(l)?;
    write_apache_conf(l)?;
    if l.is_test() {
        return Ok(());
    }
    if let Err(e) = ctx.sys.apache_test() {
        bail!("La configuración de Apache no es válida:\n{e}");
    }
    if system::windows_service_state(APACHE_SERVICE).is_none() {
        ui::info(format!("Registrando el servicio {APACHE_SERVICE}…"));
        // httpd imprime avisos genéricos ("Errors reported here must be corrected…") aunque
        // todo esté bien: solo se muestran si falla.
        let out = Command::new(httpd(l)).args(["-k", "install", "-n", APACHE_SERVICE]).output()?;
        if !out.status.success() {
            bail!(
                "No pude registrar el servicio {APACHE_SERVICE}:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    ctx.sys.enable(APACHE_SERVICE, false)?;
    if ctx.sys.is_active(APACHE_SERVICE) {
        ctx.sys.restart(APACHE_SERVICE)
    } else {
        ctx.sys.enable(APACHE_SERVICE, true).context(
            "Apache no arrancó. ¿Otro programa usa el puerto 80 (Laragon, XAMPP, IIS, Skype)? Revisa: netstat -ano | findstr :80",
        )
    }
}

/// Detiene y elimina el servicio de Apache (los archivos se quedan, salvo `--purge`).
pub fn uninstall_apache(ctx: &Ctx) -> Result<()> {
    if ctx.layout.is_test() || system::windows_service_state(APACHE_SERVICE).is_none() {
        return Ok(());
    }
    let _ = ctx.sys.disable(APACHE_SERVICE, true);
    let st = Command::new(httpd(&ctx.layout)).args(["-k", "uninstall", "-n", APACHE_SERVICE]).stdout(Stdio::null()).status();
    if !st.is_ok_and(|s| s.success()) && Command::new("sc.exe").args(["delete", APACHE_SERVICE]).stdout(Stdio::null()).status().is_err() {
        bail!("No pude eliminar el servicio {APACHE_SERVICE}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_solo_toca_su_bloque() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Layout::with_prefix(tmp.path().display().to_string());
        let path = l.hosts_file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# hosts de Windows\r\n127.0.0.1 mi-cosa.local\r\n";
        fs::write(&path, original).unwrap();

        assert!(sync_hosts(&l, &["blog".into(), "tienda".into()]).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original.trim_end()));
        assert!(text.contains("no editar este bloque)\r\n127.0.0.1 blog.test\r\n127.0.0.1 tienda.test\r\n# <<< cheka"));
        // idempotente
        assert!(!sync_hosts(&l, &["blog".into(), "tienda".into()]).unwrap());
        // un sitio menos
        assert!(sync_hosts(&l, &["blog".into()]).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("tienda.test"));
        assert_eq!(text.matches(HOSTS_BEGIN).count(), 1);
        // lo que el usuario agregue después del bloque se conserva
        fs::write(&path, format!("{text}127.0.0.1 despues.local\r\n")).unwrap();
        sync_hosts(&l, &[]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("despues.local") && !text.contains("blog.test"));
        // uninstall deja lo del usuario
        remove_hosts_block(&l).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "# hosts de Windows\r\n127.0.0.1 mi-cosa.local\r\n127.0.0.1 despues.local\r\n");
    }
}
