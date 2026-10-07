//! Comandos de configuración de PHP: php:info, php:ini, php:ext, php:update, php:updates.

#[cfg(unix)]
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

#[cfg(unix)]
use super::download_static_php;
use crate::phpconf;
#[cfg(unix)]
use crate::phpconf::is_apt;
use crate::{Ctx, php, refresh, ui};

fn version_arg(args: &[String], usage: &str) -> Result<String> {
    let Some(v) = args.iter().find(|a| !a.starts_with("--")) else { bail!("{usage}") };
    let v = php::normalize(v);
    php::validate(&v)?;
    Ok(v)
}

fn wants_json(args: &[String]) -> bool {
    args.iter().any(|a| a == "--json")
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// `php:info <versión> [--json]`
pub fn info(ctx: &Ctx, args: &[String]) -> Result<()> {
    let v = version_arg(args, "Uso: cheka php:info <versión> [--json]")?;
    let d = phpconf::detail(ctx, &v)?;
    if wants_json(args) {
        return print_json(&d);
    }
    let c = ui::colors();
    let (origin, server) = if cfg!(windows) {
        ("zip de windows.php.net", "CGI:")
    } else if d.source == "apt" {
        ("paquete del sistema", "FPM:")
    } else {
        ("binario estático", "FPM:")
    };
    println!("{}PHP {}{} ({origin})", c.bold, d.full_version, c.reset);
    println!("  {server:<8} {}", d.fpm_bin);
    println!("  CLI:     {}", d.cli_bin);
    println!("  php.ini: {}", d.ini_file);
    println!("\n{}Ajustes{}", c.bold, c.reset);
    for s in &d.settings {
        let mark = if s.custom { "  (cheka.toml)" } else { "" };
        println!("  {:<22} {}{mark}", s.key, s.value);
    }
    let on: Vec<&str> = d.extensions.iter().filter(|e| e.enabled).map(|e| e.name.as_str()).collect();
    let off: Vec<&str> = d.extensions.iter().filter(|e| !e.enabled).map(|e| e.name.as_str()).collect();
    println!("\n{}Extensiones activas ({}){}", c.bold, on.len(), c.reset);
    println!("  {}", on.join(", "));
    if !off.is_empty() {
        println!("{}Desactivadas{}\n  {}", c.bold, c.reset, off.join(", "));
    }
    if !d.can_manage_extensions {
        ui::info("Binario estático: sus extensiones vienen compiladas y no se pueden cambiar.");
    }
    Ok(())
}

/// `php:ini <versión> clave=valor …` (vacío para quitar el ajuste); sin pares, los muestra.
pub fn ini(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let usage = "Uso: cheka php:ini <versión> clave=valor [clave=valor …]   (clave= quita el ajuste)";
    let v = version_arg(args, usage)?;
    let pairs: Vec<&String> = args.iter().skip(1).collect();
    if pairs.is_empty() {
        let current = ctx.state.php.get(&v).map(|s| s.ini.clone()).unwrap_or_default();
        if current.is_empty() {
            ui::info(format!("PHP {v} no tiene ajustes propios en cheka.toml"));
        }
        for (k, val) in current {
            println!("{k} = {val}");
        }
        return Ok(());
    }
    let mut changes = Vec::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else { bail!("'{pair}' no es clave=valor\n{usage}") };
        let (key, value) = (key.trim(), value.trim());
        phpconf::validate_ini(key, value)?;
        changes.push((key.to_string(), value.to_string()));
    }
    let settings = ctx.state.php.entry(v.clone()).or_default();
    for (key, value) in &changes {
        if value.is_empty() {
            settings.ini.remove(key);
        } else {
            settings.ini.insert(key.clone(), value.clone());
        }
    }
    ctx.state.save(&ctx.id)?;
    for (key, value) in changes {
        if value.is_empty() {
            ui::ok(format!("PHP {v}: {key} vuelve al valor por defecto"));
        } else {
            ui::ok(format!("PHP {v}: {key} = {value}"));
        }
    }
    refresh::request(ctx)
}

/// `php:ext <versión> enable|disable|install <extensión>`
pub fn ext(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let usage = "Uso: cheka php:ext <versión> enable|disable|install <extensión>";
    let [v, action, name] = args else { bail!("{usage}") };
    let v = php::normalize(v);
    php::validate(&v)?;
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') || name.is_empty() {
        bail!("Nombre de extensión inválido: '{name}'");
    }
    if !phpconf::can_manage_extensions(&v) {
        bail!(
            "PHP {v} es un binario estático: sus extensiones vienen compiladas y no se pueden cambiar. \
             Solo el PHP del sistema (apt) permite activar, desactivar o instalar extensiones."
        );
    }
    match action.as_str() {
        "enable" | "disable" => {
            let on = action == "enable";
            if !phpconf::available_extensions(&v).contains(name.as_str()) {
                bail!("PHP {v} no tiene la extensión '{name}'. Instálala con: cheka php:ext {v} install {name}");
            }
            let settings = ctx.state.php.entry(v.clone()).or_default();
            // Solo se guarda lo que difiere de lo que ya hace el sistema.
            if phpconf::system_enables(&v, name) == on {
                settings.extensions.remove(name);
            } else {
                settings.extensions.insert(name.clone(), on);
            }
            ctx.state.save(&ctx.id)?;
            ui::ok(format!("PHP {v}: {name} {}", if on { "activada" } else { "desactivada" }));
            refresh::request(ctx)
        }
        #[cfg(windows)]
        "install" => {
            ctx.ensure_root()?;
            if !php::installed(&ctx.layout, &v) {
                bail!("PHP {v} no está instalado (instálalo con: cheka php:install {v})");
            }
            let ver = crate::windows_setup::install_pecl(ctx, &v, name)?;
            // Como el paquete de apt: queda activa al instalarla.
            ctx.state.php.entry(v.clone()).or_default().extensions.insert(name.clone(), true);
            ctx.state.save(&ctx.id)?;
            ui::ok(format!("{name} {ver} instalada y activada en PHP {v}"));
            refresh::run(ctx).map(drop)
        }
        #[cfg(unix)]
        "install" => {
            ctx.ensure_root()?;
            let pkg = format!("php{v}-{name}");
            ui::info(format!("Instalando {pkg}…"));
            let st = Command::new("apt-get").args(["install", "-y", "-qq", &pkg]).stdout(Stdio::null()).status()?;
            if !st.success() {
                bail!("apt-get install {pkg} falló (¿existe el paquete? búscalo con: apt-cache search php{v}-)");
            }
            ui::ok(format!("{pkg} instalado"));
            refresh::run(ctx).map(drop)
        }
        _ => bail!("{usage}"),
    }
}

/// `php:update <versión>` (root): última versión de parche.
pub fn update(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    let v = version_arg(args, "Uso: cheka php:update <versión>")?;
    if !php::installed(&ctx.layout, &v) {
        bail!("PHP {v} no está instalado (instálalo con: cheka php:install {v})");
    }
    ctx.ensure_root()?;
    #[cfg(windows)]
    return update_windows(ctx, &v);
    #[cfg(unix)]
    if is_apt(&v) {
        let out = Command::new("dpkg-query").args(["-W", "-f=${Package}\\n", &format!("php{v}-*")]).output()?;
        let pkgs: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
        ui::info(format!("Actualizando {} paquetes de PHP {v} con apt…", pkgs.len()));
        let mut cmd = Command::new("apt-get");
        cmd.args(["install", "--only-upgrade", "-y", "-qq"]).args(&pkgs).stdout(Stdio::null());
        if !cmd.status()?.success() {
            bail!("apt-get falló al actualizar PHP {v}");
        }
    } else {
        let current = std::fs::read_to_string(ctx.layout.opt.join(format!("php/{v}/VERSION"))).unwrap_or_default();
        let latest = phpconf::static_latest().get(&v).cloned();
        match latest {
            Some(l) if l != current.trim() => {
                let full = download_static_php(ctx, &v)?;
                ui::ok(format!("PHP {v}: {} → {full}", current.trim()));
            }
            Some(_) => {
                ui::info(format!("PHP {v} ya está en la última versión ({})", current.trim()));
                return Ok(());
            }
            None => bail!("No pude consultar las versiones publicadas de PHP {v}"),
        }
    }
    #[cfg(unix)]
    {
        ctx.sys.restart(&php::unit(&v))?;
        ui::ok(format!("PHP {v} actualizado y reiniciado"));
        Ok(())
    }
}

/// Windows: descarga el último parche de windows.php.net (detiene Apache mientras
/// reemplaza los archivos, porque Windows no deja renombrar un php-cgi.exe en uso).
#[cfg(windows)]
fn update_windows(ctx: &Ctx, v: &str) -> Result<()> {
    use crate::windows_setup::{download_php, php_installed_version, php_latest};
    let current = php_installed_version(&ctx.layout, v).unwrap_or_default();
    match php_latest()?.get(v) {
        Some(rel) if rel.full != current => {
            let full = download_php(ctx, v)?;
            ui::ok(format!("PHP {v}: {current} → {full}"));
        }
        Some(_) => ui::info(format!("PHP {v} ya está en la última versión ({current})")),
        None => bail!("No pude consultar las versiones publicadas de PHP {v}"),
    }
    Ok(())
}

/// `php:updates [--json]`: qué versiones instaladas tienen una versión más nueva.
pub fn updates(ctx: &Ctx, args: &[String]) -> Result<()> {
    let list = phpconf::updates(ctx);
    if wants_json(args) {
        return print_json(&list);
    }
    for u in &list {
        let latest = u.latest.as_deref().unwrap_or("?");
        if u.available {
            println!("PHP {}: {} → {latest} disponible (cheka php:update {})", u.version, u.current, u.version);
        } else {
            println!("PHP {}: {} (al día)", u.version, u.current);
        }
    }
    Ok(())
}
