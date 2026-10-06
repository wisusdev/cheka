//! Implementación de los comandos. La CLI (`main.rs`) solo traduce argumentos a estas funciones.

pub mod db;
pub mod new;
pub mod site;

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::detect::detect;
use crate::layout::TLD;
use crate::util::{is_executable, mkdir, which, write, write_mode};
use crate::{Ctx, php, refresh, render, sites, ui};

pub fn refresh(ctx: &Ctx, quiet: bool) -> Result<()> {
    ui::set_quiet(quiet);
    ctx.ensure_root()?;
    refresh::run(ctx).map(drop)
}

pub fn sites(ctx: &Ctx) -> Result<()> {
    let c = ui::colors();
    println!("{}{:<24} {:<26} {:<5} {:<34} RUTA{}", c.bold, "SITIO", "TIPO", "PHP", "URL", c.reset);
    let all = sites::list(&ctx.state);
    for s in &all {
        let det = detect(&s.path, ctx.state.docroot.get(&s.name).map(String::as_str));
        let mut v = ctx.state.site_php(&s.name);
        let scheme = if ctx.state.is_secure(&s.name) { "https" } else { "http" };
        if !php::installed(&ctx.layout, &v) {
            v.push('!');
        }
        let url = format!("{scheme}://{}.{TLD}", s.name);
        println!("{:<24} {:<26} {:<5} {:<34} {}", s.name, det.kind.as_str(), v, url, s.path.display());
    }
    if all.is_empty() {
        ui::info("No hay sitios todavía. Crea una carpeta en ~/Sites o usa 'cheka link'.");
    }
    Ok(())
}

pub fn paths(ctx: &Ctx) -> Result<()> {
    match std::fs::read(ctx.state.conf.join("paths")) {
        Ok(b) if !b.is_empty() => std::io::stdout().write_all(&b)?,
        _ => ui::info("No hay carpetas aparcadas (usa: cheka park)"),
    }
    Ok(())
}

pub fn versions(ctx: &Ctx) -> Result<()> {
    let def = ctx.state.default_php();
    for v in php::SUPPORTED {
        let mark = if v == def { "*" } else { " " };
        if php::installed(&ctx.layout, v) {
            println!("{mark} {v}  instalada  {}", php::fpm_bin(&ctx.layout, v).display());
        } else {
            println!("{mark} {v}  -");
        }
    }
    println!("(* = por defecto)");
    Ok(())
}

/// Versión de PHP del directorio actual: la del sitio, o la por defecto.
pub fn current_php(ctx: &Ctx) -> Result<String> {
    let cwd = std::env::current_dir()?;
    Ok(match sites::resolve(&sites::list(&ctx.state), None, &cwd) {
        Ok(s) => ctx.state.site_php(&s.name),
        Err(_) => ctx.state.default_php(),
    })
}

fn current_cli(ctx: &Ctx) -> Result<PathBuf> {
    Ok(php::cli_bin(&ctx.layout, &current_php(ctx)?))
}

pub fn which_php(ctx: &Ctx) -> Result<()> {
    println!("{}", current_cli(ctx)?.display());
    Ok(())
}

pub fn php(ctx: &Ctx, args: Vec<OsString>) -> Result<()> {
    let bin = current_cli(ctx)?;
    if !is_executable(&bin) {
        bail!("No encuentro {}", bin.display());
    }
    Err(anyhow!("No pude ejecutar {}: {}", bin.display(), Command::new(&bin).args(args).exec()))
}

pub fn composer(ctx: &Ctx, args: Vec<OsString>) -> Result<()> {
    let bin = current_cli(ctx)?;
    let composer = which("composer").ok_or_else(|| anyhow!("Composer no está instalado"))?;
    Err(anyhow!("No pude ejecutar composer: {}", Command::new(&bin).arg(composer).args(args).exec()))
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

pub fn status(ctx: &Ctx) -> Result<()> {
    let c = ui::colors();
    let mut units: Vec<String> = ["apache2", "cheka", "cheka-dns", "mariadb"].map(String::from).into();
    // Mientras convivan las versiones: el vigilante de bash, solo si sigue instalado.
    for legacy in ["cheka-watch.path", "cheka-refresh.timer"] {
        if ctx.layout.units.join(legacy).exists() {
            units.push(legacy.to_string());
        }
    }
    units.extend(php_units());
    for u in units {
        let out = Command::new("systemctl").args(["is-active", &u]).stderr(Stdio::null()).output()?;
        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if state == "active" {
            println!("{}●{} {u}", c.green, c.reset);
        } else {
            println!("{}●{} {u} ({state})", c.red, c.reset);
        }
    }
    let probe = format!("cheka-check.{TLD}");
    match resolve_host(&probe) {
        Some(ip) => println!("{}●{} DNS: *.{TLD} → {ip}", c.green, c.reset),
        None => println!("{}●{} DNS: *.{TLD} no resuelve", c.red, c.reset),
    }
    match crate::ipc::call(&ctx.layout.socket(), &crate::ipc::Request::Ping) {
        Ok(r) if r.ok => println!("{}●{} API del daemon ({})", c.green, c.reset, ctx.layout.socket().display()),
        _ => println!("{}●{} API del daemon: no responde", c.red, c.reset),
    }
    println!("PHP por defecto: {}", ctx.state.default_php());
    Ok(())
}

pub const STATIC_URL: &str = "https://dl.static-php.dev/static-php-cli/bulk";
pub const WPCLI_URL: &str = "https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar";

/// Valida la versión y, si no está instalada, la instala (pide sudo).
pub fn ensure_php(ctx: &Ctx, v: &str) -> Result<()> {
    php::validate(v)?;
    if !php::installed(&ctx.layout, v) {
        ctx.run_as_root(&["php:install", v])?;
    }
    Ok(())
}

pub fn ensure_wpcli(ctx: &Ctx) -> Result<()> {
    if ctx.id.wpcli.is_file() {
        return Ok(());
    }
    ui::info("Descargando WP-CLI…");
    crate::userfs::mkdir(&ctx.id, ctx.id.wpcli.parent().unwrap())?;
    let ok = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&ctx.id.wpcli)
        .arg(WPCLI_URL)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!("No pude descargar WP-CLI");
    }
    crate::userfs::own(&ctx.id, &ctx.id.wpcli)?;
    Ok(())
}

pub fn wp(ctx: &Ctx, args: Vec<OsString>) -> Result<()> {
    ensure_wpcli(ctx)?;
    let bin = current_cli(ctx)?;
    Err(anyhow!("No pude ejecutar WP-CLI: {}", Command::new(&bin).arg(&ctx.id.wpcli).args(args).exec()))
}

/// start | stop | restart de todos los servicios de cheka.
pub fn services(ctx: &Ctx, action: &str) -> Result<()> {
    ctx.ensure_root()?;
    let mut units: Vec<String> = ["cheka-dns", "apache2", "mariadb"].map(String::from).into();
    units.extend(php_units());
    for u in units {
        if Command::new("systemctl").args([action, &u]).status().is_ok_and(|s| s.success()) {
            ui::ok(format!("{action} {u}"));
        } else {
            ui::warn(format!("{action} {u} falló"));
        }
    }
    Ok(())
}

/// Descarga los binarios estáticos (fpm + cli) de la última versión X.Y.* disponible.
fn download_static_php(ctx: &Ctx, v: &str) -> Result<()> {
    let arch = std::env::consts::ARCH;
    ui::info(format!("Buscando la última versión de PHP {v}…"));
    let listing = Command::new("curl").args(["-fsSL", &format!("{STATIC_URL}/")]).output()?;
    let re = regex::Regex::new(&format!(r"php-{}\.([0-9]+)-fpm-linux-{arch}\.tar\.gz", regex::escape(v)))?;
    let patch = re
        .captures_iter(&String::from_utf8_lossy(&listing.stdout))
        .filter_map(|c| c[1].parse::<u32>().ok())
        .max()
        .ok_or_else(|| anyhow!("No encontré binarios de PHP {v} para {arch} en {STATIC_URL}"))?;
    let full = format!("{v}.{patch}");
    let tmp = tempfile::tempdir()?;
    ui::info(format!("Descargando PHP {full} (fpm + cli)…"));
    let dest = ctx.layout.opt.join(format!("php/{v}"));
    mkdir(&dest)?;
    for kind in ["fpm", "cli"] {
        let tgz = tmp.path().join(format!("{kind}.tgz"));
        let ok = Command::new("curl")
            .args(["-fL", "--progress-bar", "-o"])
            .arg(&tgz)
            .arg(format!("{STATIC_URL}/php-{full}-{kind}-linux-{arch}.tar.gz"))
            .status()?
            .success();
        if !ok {
            bail!("No pude descargar PHP {full} ({kind})");
        }
        if !Command::new("tar").arg("xzf").arg(&tgz).arg("-C").arg(&dest).status()?.success() {
            bail!("No pude descomprimir PHP {full} ({kind})");
        }
    }
    let _ = Command::new("chown").args(["-R", "root:root"]).arg(&dest).stderr(Stdio::null()).status();
    write(&dest.join("VERSION"), &format!("{full}\n"))?;
    Ok(())
}

/// php:install <versión> (root): instala si hace falta y (re)genera su configuración.
pub fn php_install(ctx: &Ctx, version: &str) -> Result<()> {
    ctx.ensure_root()?;
    let l = &ctx.layout;
    let v = php::normalize(version);
    php::validate(&v)?;
    if !php::installed(l, &v) {
        let apt_ok = !l.is_test()
            && is_executable(&php::apt_cli(&v))
            && Command::new("apt-cache")
                .args(["show", &format!("php{v}-fpm")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        if apt_ok {
            ui::info(format!("Instalando php{v}-fpm desde apt…"));
            let st = Command::new("apt-get")
                .args(["install", "-y", "-qq", &format!("php{v}-fpm")])
                .stdout(Stdio::null())
                .status()?;
            if !st.success() {
                bail!("apt-get install php{v}-fpm falló");
            }
            let _ = ctx.sys.disable(&format!("php{v}-fpm"), true);
        } else {
            download_static_php(ctx, &v)?;
        }
    }
    mkdir(&l.log_dir)?;
    mkdir(&l.run_dir.join(format!("php-{v}")))?;
    let dir = php::config_dir(l, &v);
    mkdir(&dir)?;
    write(&dir.join("php-fpm.conf"), &render::php_fpm_conf(l, &v, &ctx.id.user, &ctx.id.group))?;
    mkdir(&dir.join("conf.d"))?;
    write(&dir.join("php.ini"), &render::php_ini(&php::timezone()))?;
    if !is_executable(&php::apt_cli(&v)) {
        mkdir(&l.bin)?;
        write_mode(&l.bin.join(format!("php{v}")), &render::php_cli_wrapper(l, &v), 0o755)?;
    }
    ctx.sys.enable(&php::unit(&v), false).context("systemctl enable")?;
    ctx.sys.restart(&php::unit(&v))?;
    ui::ok(format!("PHP {v} listo ({})", php::fpm_bin(l, &v).display()));
    Ok(())
}

/// `_fpm <versión>`: lo usa `cheka-php@.service` para arrancar el PHP-FPM correcto.
pub fn fpm(ctx: &Ctx, version: &str) -> Result<()> {
    php::validate(version)?;
    let dir = php::config_dir(&ctx.layout, version);
    let bin = php::fpm_bin(&ctx.layout, version);
    let err = Command::new(&bin)
        .env("PHP_INI_SCAN_DIR", format!(":{}", dir.join("conf.d").display()))
        .arg("--nodaemonize")
        .arg("--fpm-config")
        .arg(dir.join("php-fpm.conf"))
        .arg("-c")
        .arg(dir.join("php.ini"))
        .exec();
    Err(anyhow!("No pude ejecutar {}: {err}", bin.display()))
}

/// Muestra el estado actual en el formato TOML futuro (no escribe nada todavía).
pub fn migrate(ctx: &Ctx) -> Result<()> {
    print!("{}", ctx.state.to_toml()?);
    Ok(())
}
