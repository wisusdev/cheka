//! Implementación de los comandos. La CLI (`main.rs`) solo traduce argumentos a estas funciones.

pub mod db;
pub mod new;
pub mod phpcmd;
pub mod service;
pub mod site;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::layout::TLD;
use crate::util::{is_executable, mkdir, which, write, write_mode};
use crate::{Ctx, php, refresh, render, report, sites, ui};

pub fn refresh(ctx: &Ctx, quiet: bool) -> Result<()> {
    ui::set_quiet(quiet);
    ctx.ensure_root()?;
    refresh::run(ctx).map(drop)
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn sites(ctx: &Ctx, json: bool) -> Result<()> {
    let all = report::sites(ctx);
    if json {
        return print_json(&all);
    }
    let c = ui::colors();
    println!("{}{:<24} {:<26} {:<5} {:<34} RUTA{}", c.bold, "SITIO", "TIPO", "PHP", "URL", c.reset);
    for s in &all {
        let php = if s.php_installed { s.php.clone() } else { format!("{}!", s.php) };
        println!("{:<24} {:<26} {:<5} {:<34} {}", s.name, s.kind, php, s.url, s.path);
    }
    if all.is_empty() {
        ui::info("No hay sitios todavía. Crea una carpeta en ~/Sites o usa 'cheka link'.");
    }
    Ok(())
}

pub fn paths(ctx: &Ctx) -> Result<()> {
    let paths: Vec<&String> = ctx.state.paths.iter().filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        ui::info("No hay carpetas aparcadas (usa: cheka park)");
    }
    for p in paths {
        println!("{p}");
    }
    Ok(())
}

pub fn versions(ctx: &Ctx, json: bool) -> Result<()> {
    let all = report::versions(ctx);
    if json {
        return print_json(&all);
    }
    for v in &all {
        let mark = if v.default { "*" } else { " " };
        match &v.fpm_bin {
            Some(bin) => println!("{mark} {}  instalada  {bin}", v.version),
            None => println!("{mark} {}  -", v.version),
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

pub use crate::report::php_units;


pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let st = report::status(ctx);
    if json {
        return print_json(&st);
    }
    let c = ui::colors();
    for svc in &st.services {
        if svc.state == "active" {
            println!("{}●{} {}", c.green, c.reset, svc.name);
        } else {
            println!("{}●{} {} ({})", c.red, c.reset, svc.name, svc.state);
        }
    }
    match &st.dns {
        Some(ip) => println!("{}●{} DNS: *.{TLD} → {ip}", c.green, c.reset),
        None => println!("{}●{} DNS: *.{TLD} no resuelve", c.red, c.reset),
    }
    if st.daemon {
        println!("{}●{} API del daemon ({})", c.green, c.reset, st.socket);
    } else {
        println!("{}●{} API del daemon: no responde", c.red, c.reset);
    }
    println!("PHP por defecto: {}", st.default_php);
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
/// Extrae en un temporal y reemplaza con `rename`: es seguro aunque esa versión esté
/// corriendo (sirve también para actualizar). Devuelve la versión completa instalada.
pub fn download_static_php(ctx: &Ctx, v: &str) -> Result<String> {
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
    let staging = tmp.path().join("x");
    mkdir(&staging)?;
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
        if !Command::new("tar").arg("xzf").arg(&tgz).arg("-C").arg(&staging).status()?.success() {
            bail!("No pude descomprimir PHP {full} ({kind})");
        }
    }
    let _ = Command::new("chown").args(["-R", "root:root"]).arg(&staging).stderr(Stdio::null()).status();
    for e in std::fs::read_dir(&staging)?.flatten() {
        // mismo sistema de archivos que /opt no está garantizado: copiar y renombrar dentro de dest
        let part = dest.join(format!(".{}.nuevo", e.file_name().to_string_lossy()));
        std::fs::copy(e.path(), &part)?;
        std::fs::set_permissions(&part, std::fs::metadata(e.path())?.permissions())?;
        std::fs::rename(&part, dest.join(e.file_name()))?;
    }
    write(&dest.join("VERSION"), &format!("{full}\n"))?;
    Ok(full)
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
    crate::phpconf::apply(ctx, &v)?;
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
        .env("PHP_INI_SCAN_DIR", crate::phpconf::scan_dir_env(&ctx.layout, version))
        .arg("--nodaemonize")
        .arg("--fpm-config")
        .arg(dir.join("php-fpm.conf"))
        .arg("-c")
        .arg(dir.join("php.ini"))
        .exec();
    Err(anyhow!("No pude ejecutar {}: {err}", bin.display()))
}

/// `migrate`: pasa el estado del formato de bash a `cheka.toml`.
/// `--dry-run` solo muestra el resultado; `--legacy` hace el camino inverso (para volver
/// a la versión en bash).
pub fn migrate(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    use crate::state::Format;
    let toml = ctx.state.conf.join(crate::state::TOML_FILE);
    match args.first().map(String::as_str) {
        Some("--dry-run") => print!("{}", ctx.state.to_toml()?),
        Some("--legacy") => {
            ctx.state.save_legacy(&ctx.id)?;
            ui::ok(format!(
                "Estado escrito en el formato de la versión en bash en {} ({} quedó como {}.bak)",
                ctx.state.conf.display(),
                crate::state::TOML_FILE,
                crate::state::TOML_FILE
            ));
        }
        None if ctx.state.format == Format::Toml && !ctx.state.has_legacy_files() => {
            ui::info(format!("El estado ya está en {}", toml.display()));
        }
        None => {
            ctx.state.save(&ctx.id)?;
            ui::ok(format!("Estado migrado a {} (lo anterior quedó en legacy/)", toml.display()));
        }
        Some(other) => bail!("Opción desconocida: {other} (usa: cheka migrate [--dry-run | --legacy])"),
    }
    Ok(())
}
