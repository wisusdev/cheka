//! `install` / `uninstall` en Windows (administrador). Idempotentes. Descargan Apache
//! Lounge (+ mod_fcgid) y PHP NTS a `%ProgramData%\cheka` y registran el servicio
//! `cheka-apache`. Falta (hito 3.3): archivo `hosts`, mkcert, MariaDB y el daemon.
//! En modo prueba (`CHEKA_PREFIX`) no descargan nada ni tocan servicios ni el PATH.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::layout::TLD;
use crate::util::{mkdir, write};
use crate::windows_setup::{self, APACHE_SERVICE};
use crate::{Ctx, php, refresh, ui, userfs};

fn step(title: &str) {
    let c = ui::colors();
    println!();
    println!("{}== {title} =={}", c.bold, c.reset);
}

fn powershell(script: &str) -> Result<String> {
    let out = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]).output()?;
    if !out.status.success() {
        anyhow::bail!("PowerShell falló: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Apache Lounge y PHP necesitan el runtime de Visual C++ 2015-2022 (x64).
fn ensure_vc_runtime() -> Result<()> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let sys32 = Path::new(&root).join("System32");
    if ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"].iter().all(|f| sys32.join(f).is_file()) {
        ui::ok("Runtime de Visual C++ presente");
        return Ok(());
    }
    ui::info("Instalando el runtime de Visual C++ 2015-2022 con winget…");
    let ok = Command::new("winget")
        .args(["install", "-e", "--id", "Microsoft.VCRedist.2015+.x64", "--silent"])
        .args(["--accept-source-agreements", "--accept-package-agreements"])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        anyhow::bail!("No pude instalar el runtime de Visual C++; descárgalo de https://aka.ms/vs/17/release/vc_redist.x64.exe");
    }
    Ok(())
}

/// Agrega `dir` al PATH del sistema si no está (lo verán las terminales nuevas).
fn ensure_in_path(dir: &Path) -> Result<bool> {
    let dir = dir.display().to_string();
    let current = powershell("[Environment]::GetEnvironmentVariable('Path','Machine')")?;
    if current.split(';').any(|p| p.trim_end_matches('\\').eq_ignore_ascii_case(dir.trim_end_matches('\\'))) {
        return Ok(false);
    }
    let new = if current.is_empty() { dir } else { format!("{};{dir}", current.trim_end_matches(';')) };
    powershell(&format!("[Environment]::SetEnvironmentVariable('Path', '{}', 'Machine')", new.replace('\'', "''")))?;
    Ok(true)
}

fn remove_from_path(dir: &Path) -> Result<()> {
    let dir = dir.display().to_string();
    let current = powershell("[Environment]::GetEnvironmentVariable('Path','Machine')")?;
    let kept: Vec<&str> = current
        .split(';')
        .filter(|p| !p.is_empty() && !p.trim_end_matches('\\').eq_ignore_ascii_case(dir.trim_end_matches('\\')))
        .collect();
    let new = kept.join(";");
    if new != current {
        powershell(&format!("[Environment]::SetEnvironmentVariable('Path', '{}', 'Machine')", new.replace('\'', "''")))?;
    }
    Ok(())
}

pub fn install(ctx: &mut Ctx) -> anyhow::Result<()> {
    ctx.ensure_root()?;
    let l = ctx.layout.clone();
    let test = l.is_test();

    if !test {
        step("Requisitos");
        ensure_vc_runtime()?;
    }

    step("Archivos de cheka");
    let exe = std::env::current_exe()?;
    let target = l.bin.join("cheka.exe");
    mkdir(&l.bin)?;
    if fs::canonicalize(&exe).ok() != fs::canonicalize(&target).ok() {
        // Windows deja renombrar un .exe en uso, pero no sobrescribirlo.
        let old = l.bin.join("cheka.exe.anterior");
        let _ = fs::remove_file(&old);
        if target.exists() {
            fs::rename(&target, &old).context("No pude apartar el cheka.exe anterior")?;
        }
        fs::copy(&exe, &target).context("No pude copiar el binario")?;
        let _ = fs::remove_file(&old);
    }
    for d in [&l.etc, &l.opt.join("php"), &l.apache_sites, &l.log_dir] {
        mkdir(d)?;
    }
    write(&l.etc.join("user"), &format!("{}\n", ctx.id.user))?;
    userfs::mkdir(&ctx.id, &ctx.id.conf.join("certs"))?;
    let sites_dir = ctx.id.home.join("Sites");
    userfs::mkdir(&ctx.id, &sites_dir)?;
    let default_php = ctx.state.default_php();
    if ctx.state.default_php.is_none() {
        ctx.state.default_php = Some(default_php.clone());
    }
    if ctx.state.paths.iter().all(|p| p.is_empty()) {
        ctx.state.paths = vec![sites_dir.display().to_string()];
    }
    ctx.state.save(&ctx.id)?;
    if !test && ensure_in_path(&l.bin)? {
        ui::ok(format!("{} agregado al PATH (abre una terminal nueva para usar 'cheka')", l.bin.display()));
    }
    ui::ok(format!("cheka en {}", target.display()));

    step(&format!("PHP {default_php}"));
    if test {
        mkdir(&php::config_dir(&l, &default_php).join("conf.d"))?;
    } else {
        windows_setup::php_install(ctx, &default_php)?;
    }

    step("Apache (Apache Lounge + mod_fcgid)");
    windows_setup::install_apache(ctx)?;
    ui::ok(format!("Apache listo como servicio {APACHE_SERVICE}"));

    step("Sitios");
    ctx.reload_state()?;
    refresh::run(ctx)?;

    let c = ui::colors();
    println!();
    println!("{}{}cheka está listo.{} Crea o clona un proyecto en {}", c.green, c.bold, c.reset, sites_dir.display());
    ui::warn(format!(
        "Todavía falta que *.{TLD} resuelva solo (hito 3.3). Mientras tanto, agrega cada sitio a \
         C:\\Windows\\System32\\drivers\\etc\\hosts, p. ej.: 127.0.0.1 blog.{TLD}"
    ));
    Ok(())
}

pub fn uninstall(ctx: &mut Ctx, args: &[String]) -> anyhow::Result<()> {
    ctx.ensure_root()?;
    let purge = args.first().is_some_and(|a| a == "--purge");
    let l = ctx.layout.clone();
    step("Desinstalando cheka");
    windows_setup::uninstall_apache(ctx)?;
    if !l.is_test() {
        remove_from_path(&l.bin)?;
    }
    if purge {
        // cheka.exe puede ser este mismo proceso: lo que no se pueda borrar queda para después.
        let _ = fs::remove_dir_all(&l.opt);
        let _ = fs::remove_dir_all(&ctx.id.conf);
        ui::ok("Eliminados Apache, PHP, la configuración del sistema y la de usuario");
    } else {
        ui::ok(format!(
            "Se conservan {} y {} (usa --purge para borrarlos)",
            l.opt.display(),
            ctx.id.conf.display()
        ));
    }
    ui::ok("Servicio de Apache eliminado. Tus proyectos no se tocaron.");
    Ok(())
}
