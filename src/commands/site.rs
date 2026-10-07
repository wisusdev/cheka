//! Comandos que modifican el estado de los sitios: park, link, isolate, secure…

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
    if ctx.state.paths.contains(&dir) {
        ui::info(format!("{dir} ya estaba aparcado"));
    } else {
        ctx.state.paths.push(dir.clone());
        ctx.state.save(&ctx.id)?;
        ui::ok(format!("Aparcado: cada carpeta dentro de {dir} será <carpeta>.{TLD}"));
    }
    refresh::request(ctx)
}

pub fn forget(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let dir = dir_arg(ctx, args)?;
    if !ctx.state.paths.contains(&dir) {
        bail!("{dir} no está aparcado");
    }
    ctx.state.paths.retain(|p| *p != dir);
    ctx.state.save(&ctx.id)?;
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
    ctx.state.links.insert(name.clone(), target);
    ctx.state.save(&ctx.id)?;
    ui::ok(format!("Enlazado: http://{name}.{TLD} → {}", cwd.display()));
    refresh::request(ctx)
}

pub fn unlink(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let raw = match arg(args, 0) {
        Some(a) => a.to_string(),
        None => cwd_basename(ctx)?,
    };
    let name = sites::normalize(&raw);
    if ctx.state.links.remove(&name).is_none() {
        bail!("No hay un enlace llamado '{name}'");
    }
    ctx.state.save(&ctx.id)?;
    ui::ok(format!("Enlace eliminado: {name}"));
    refresh::request(ctx)
}

pub fn isolate(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let Some(v) = arg(args, 0) else { bail!("Uso: cheka isolate <versión> [--site=nombre]") };
    let v = php::normalize(v);
    let site = resolve(ctx, site_flag(&args[1..]))?;
    ensure_php(ctx, &v)?;
    ctx.state.isolated.insert(site.name.clone(), v.clone());
    ctx.state.save(&ctx.id)?;
    ui::ok(format!("{} usará PHP {v}", site.name));
    refresh::request(ctx)
}

pub fn unisolate(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, site_flag(args))?;
    ctx.state.isolated.remove(&site.name);
    ctx.state.save(&ctx.id)?;
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
    ctx.state.default_php = Some(v.clone());
    ctx.state.save(&ctx.id)?;
    ui::ok(format!("PHP por defecto: {v}"));
    refresh::request(ctx)
}

pub fn docroot(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, None)?;
    match args.first() {
        None => {
            ctx.state.docroot.remove(&site.name);
            ctx.state.save(&ctx.id)?;
            ui::ok(format!("{} vuelve a la detección automática", site.name));
        }
        Some(sub) => {
            if !Path::new(&format!("{}/{sub}", site.path.display())).is_dir() {
                bail!("No existe {}/{sub}", site.path.display());
            }
            let sub = sub.strip_suffix('/').unwrap_or(sub);
            ctx.state.docroot.insert(site.name.clone(), sub.to_string());
            ctx.state.save(&ctx.id)?;
            ui::ok(format!("{} servirá desde {}/{sub}", site.name, site.path.display()));
        }
    }
    refresh::request(ctx)
}

/// Certificado local para `sitio.test` y `*.sitio.test`, y marca el sitio como seguro.
pub fn make_cert(ctx: &mut Ctx, name: &str) -> Result<()> {
    let hint = if cfg!(windows) { "cheka install" } else { "sudo cheka install" };
    let mkcert = which("mkcert").ok_or_else(|| anyhow!("mkcert no está instalado (ejecuta: {hint})"))?;
    userfs::mkdir(&ctx.id, &ctx.state.conf.join("certs"))?;
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
    ctx.state.secured.insert(name.to_string());
    ctx.state.save(&ctx.id)?;
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
    for f in [ctx.state.cert(&site.name), ctx.state.cert_key(&site.name)] {
        userfs::remove(&f)?;
    }
    ctx.state.secured.remove(&site.name);
    ctx.state.save(&ctx.id)?;
    ui::ok(format!("{} vuelve a http", site.name));
    refresh::request(ctx)
}

pub fn open(ctx: &Ctx, args: &[String]) -> Result<()> {
    let site = resolve(ctx, arg(args, 0))?;
    let scheme = if ctx.state.is_secure(&site.name) { "https" } else { "http" };
    let url = format!("{scheme}://{}.{TLD}", site.name);
    let _ = crate::platform::open_url_command(&url)
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
    let logs = [l.join(format!("{}-error.log", site.name)), l.join(format!("php-{v}-errors.log"))];
    let err = crate::platform::follow_logs(&logs);
    Err(anyhow!("No pude seguir los logs: {err}"))
}
