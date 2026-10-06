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
    let has = |name: &str| ext_dir.join(format!("php_{name}.dll")).is_file();
    let extensions: Vec<String> = DEFAULT_EXTENSIONS.iter().filter(|e| has(e)).map(|e| e.to_string()).collect();
    let mut text = render::windows_php_ini(
        l,
        &WindowsPhpIni {
            v,
            ext_dir: &ext_dir,
            extensions: &extensions,
            // Desde PHP 8.5 OPcache viene incluido y no se carga aparte.
            opcache: has("opcache"),
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
        let st = Command::new(httpd(l)).args(["-k", "install", "-n", APACHE_SERVICE]).stdout(Stdio::null()).status()?;
        if !st.success() {
            bail!("No pude registrar el servicio {APACHE_SERVICE}");
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
