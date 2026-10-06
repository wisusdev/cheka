//! Comandos que modifican el estado de los sitios: park, link, isolate, secure…

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};

use super::ensure_php;
use crate::layout::TLD;
use crate::sites::{self, Site};
use crate::util::{canonicalize_lenient, which};
use crate::{Ctx, php, refresh, ui, userfs};

fn arg(args: &[String], i: usize) -> Option<&str> {
    args.get(i).map(String::as_str).filter(|s| !s.is_empty())
}

/// `readlink -f "${1:-$PWD}"` (vacío si ni el padre existe, como en bash).
fn dir_arg(ctx: &Ctx, args: &[String]) -> Result<String> {
    let raw = match arg(args, 0) {
        Some(a) => PathBuf::from(a),
        None => ctx.cwd()?,
    };
    Ok(canonicalize_lenient(&raw).map(|p| p.display().to_string()).unwrap_or_default())
}

fn resolve(ctx: &Ctx, want: Option<&str>) -> Result<Site> {
    sites::resolve(&sites::list(&ctx.state), want, &ctx.cwd()?)
}

fn site_flag(args: &[String]) -> Option<&str> {
    args.first().and_then(|a| a.strip_prefix("--site="))
}

fn cwd_basename(ctx: &Ctx) -> Result<String> {
    Ok(ctx.cwd()?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

pub fn park(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let dir = dir_arg(ctx, args)?;
    if !Path::new(&dir).is_dir() {
        bail!("No existe el directorio: {dir}");
    }
    let paths = ctx.state.conf.join("paths");
    userfs::mkdir(&ctx.id, &ctx.state.conf)?;
    userfs::touch(&ctx.id, &paths)?;
    if ctx.state.paths.contains(&dir) {
        ui::info(format!("{dir} ya estaba aparcado"));
    } else {
        userfs::append_line(&ctx.id, &paths, &dir)?;
        ui::ok(format!("Aparcado: cada carpeta dentro de {dir} será <carpeta>.{TLD}"));
    }
    refresh::request(ctx)
}

pub fn forget(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let dir = dir_arg(ctx, args)?;
    if !ctx.state.paths.contains(&dir) {
        bail!("{dir} no está aparcado");
    }
    let rest: String = ctx.state.paths.iter().filter(|p| **p != dir).map(|p| format!("{p}\n")).collect();
    userfs::write(&ctx.id, &ctx.state.conf.join("paths"), &rest)?;
    ui::ok(format!("Olvidado: {dir}"));
    refresh::request(ctx)
}

pub fn link(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let raw = match arg(args, 0) {
        Some(a) => a.to_string(),
        None => cwd_basename(ctx)?,
    };
    let name = sites::normalize(&raw);
    if name.is_empty() {
        bail!("Nombre de sitio inválido");
    }
    let cwd = ctx.cwd()?;
    let target = canonicalize_lenient(&cwd).unwrap_or_else(|| cwd.clone());
    userfs::symlink_force(&ctx.id, &target, &ctx.state.conf.join("links").join(&name))?;
    ui::ok(format!("Enlazado: http://{name}.{TLD} → {}", cwd.display()));
    refresh::request(ctx)
}

pub fn unlink(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let raw = match arg(args, 0) {
        Some(a) => a.to_string(),
        None => cwd_basename(ctx)?,
    };
    let name = sites::normalize(&raw);
    let link = ctx.state.conf.join("links").join(&name);
    if !link.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("No hay un enlace llamado '{name}'");
    }
    userfs::remove(&link)?;
    ui::ok(format!("Enlace eliminado: {name}"));
    refresh::request(ctx)
}

pub fn isolate(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let Some(v) = arg(args, 0) else { bail!("Uso: cheka isolate <versión> [--site=nombre]") };
    let v = php::normalize(v);
    let site = resolve(ctx, site_flag(&args[1..]))?;
    ensure_php(ctx, &v)?;
    userfs::write(&ctx.id, &ctx.state.conf.join("isolated").join(&site.name), &format!("{v}\n"))?;
    ui::ok(format!("{} usará PHP {v}", site.name));
    refresh::request(ctx)
}

pub fn unisolate(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, site_flag(args))?;
    userfs::remove(&ctx.state.conf.join("isolated").join(&site.name))?;
    ui::ok(format!("{} vuelve a la versión por defecto (PHP {})", site.name, ctx.state.default_php()));
    refresh::request(ctx)
}

pub fn use_php(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let Some(v) = args.first() else {
        println!("PHP por defecto: {}", ctx.state.default_php());
        return Ok(());
    };
    let v = php::normalize(v);
    ensure_php(ctx, &v)?;
    let cfg = ctx.state.conf.join("config");
    let mut text: String = std::fs::read_to_string(&cfg)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with("default_php="))
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str(&format!("default_php={v}\n"));
    userfs::write(&ctx.id, &cfg, &text)?;
    ui::ok(format!("PHP por defecto: {v}"));
    refresh::request(ctx)
}

pub fn docroot(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, None)?;
    let file = ctx.state.conf.join("docroot").join(&site.name);
    match args.first() {
        None => {
            userfs::remove(&file)?;
            ui::ok(format!("{} vuelve a la detección automática", site.name));
        }
        Some(sub) => {
            if !Path::new(&format!("{}/{sub}", site.path.display())).is_dir() {
                bail!("No existe {}/{sub}", site.path.display());
            }
            let sub = sub.strip_suffix('/').unwrap_or(sub);
            userfs::write(&ctx.id, &file, &format!("{sub}\n"))?;
            ui::ok(format!("{} servirá desde {}/{sub}", site.name, site.path.display()));
        }
    }
    refresh::request(ctx)
}

/// Certificado local para `sitio.test` y `*.sitio.test`, y marca el sitio como seguro.
pub fn make_cert(ctx: &Ctx, name: &str) -> Result<()> {
    let mkcert = which("mkcert").ok_or_else(|| anyhow!("mkcert no está instalado (ejecuta: sudo cheka install)"))?;
    userfs::mkdir(&ctx.id, &ctx.state.conf.join("certs"))?;
    userfs::mkdir(&ctx.id, &ctx.state.conf.join("secured"))?;
    let (cert, key) = (ctx.state.cert(name), ctx.state.cert_key(name));
    let ok = Command::new(mkcert)
        .arg("-cert-file")
        .arg(&cert)
        .arg("-key-file")
        .arg(&key)
        .arg(format!("{name}.{TLD}"))
        .arg(format!("*.{name}.{TLD}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        bail!("mkcert falló");
    }
    for f in [&cert, &key] {
        userfs::own(&ctx.id, f)?;
    }
    userfs::touch(&ctx.id, &ctx.state.conf.join("secured").join(name))?;
    ui::ok(format!("https://{name}.{TLD} listo"));
    Ok(())
}

pub fn secure(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, arg(args, 0))?;
    make_cert(ctx, &site.name)?;
    refresh::request(ctx)
}

pub fn unsecure(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, arg(args, 0))?;
    for f in [
        ctx.state.conf.join("secured").join(&site.name),
        ctx.state.cert(&site.name),
        ctx.state.cert_key(&site.name),
    ] {
        userfs::remove(&f)?;
    }
    ui::ok(format!("{} vuelve a http", site.name));
    refresh::request(ctx)
}

pub fn open(ctx: &Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, arg(args, 0))?;
    let scheme = if ctx.state.is_secure(&site.name) { "https" } else { "http" };
    let url = format!("{scheme}://{}.{TLD}", site.name);
    let _ = Command::new("xdg-open")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    println!("{url}");
    Ok(())
}

pub fn log(ctx: &Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, arg(args, 0))?;
    let v = ctx.state.site_php(&site.name);
    let l = &ctx.layout.log_dir;
    let err = Command::new("tail")
        .args(["-n", "50", "-F"])
        .arg(l.join(format!("{}-error.log", site.name)))
        .arg(l.join(format!("php-{v}-errors.log")))
        .exec();
    Err(anyhow!("No pude ejecutar tail: {err}"))
}
