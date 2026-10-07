//! `install` / `uninstall` en Windows (administrador). Idempotentes. Descargan Apache
//! Lounge (+ mod_fcgid), PHP NTS y mkcert a `%ProgramData%\cheka`, instalan MariaDB con
//! winget y registran los servicios `cheka` (daemon) y `cheka-apache`.
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

/// Detiene un servicio si existe y está corriendo (espera a que termine).
fn stop_service(name: &str) {
    if crate::system::windows_service_state(name).is_some_and(|s| s != "inactive") {
        let _ = powershell(&format!("Stop-Service -Name '{name}' -Force"));
    }
}

fn addresses(host: &str) -> Vec<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    (host, 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default()
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
    // Un daemon de una versión anterior podría no entender el estado: se detiene antes de
    // reemplazar el binario y se reinicia al final.
    if !test {
        stop_service(crate::daemon::SERVICE_NAME);
    }
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
    if !test {
        // Para la ACL de la named pipe: solo el dueño de los proyectos (y administradores).
        write(&l.etc.join("user-sid"), &format!("{}\n", windows_setup::user_sid(&ctx.id.user)?))?;
    }
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

    step("HTTPS local (mkcert)");
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        windows_setup::install_mkcert(ctx)?;
    }

    step("Apache (Apache Lounge + mod_fcgid)");
    windows_setup::install_apache(ctx)?;
    ui::ok(format!("Apache listo como servicio {APACHE_SERVICE}"));

    step(&format!("DNS para *.{TLD}"));
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        windows_setup::install_nrpt()?;
        ui::ok(format!("Regla NRPT: *.{TLD} → DNS de cheka en 127.0.0.1:53 (más el archivo hosts de respaldo)"));
    }

    step("Sitios");
    ctx.reload_state()?;
    refresh::run(ctx)?;
    if !test {
        windows_setup::register_daemon(&target)?;
        ctx.sys.enable(crate::daemon::SERVICE_NAME, true)?;
    }
    ui::ok("Daemon activo: las carpetas nuevas se publican solas, sin pedir permisos");

    step("MariaDB");
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        match windows_setup::install_mariadb(ctx) {
            Ok(()) => {
                // Como en Linux, `mariadb` y `mariadb-dump` quedan a mano en la terminal.
                if let Some(bin) = windows_setup::mariadb_bin("mariadb").parent() {
                    ensure_in_path(bin)?;
                }
            }
            // No es motivo para dejar a medias lo demás: se puede repetir `cheka install`.
            Err(e) => {
                ui::warn(format!("{e:#}"));
                ui::warn("¿Otro MySQL/MariaDB (Laragon, XAMPP) usa el puerto 3306? Detenlo y repite 'cheka install'");
            }
        }
    }

    step("Verificación");
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        // El DNS lo abre el daemon: puede tardar un momento en responder.
        let mut resolved = false;
        for _ in 0..20 {
            if addresses(&format!("cheka-check.{TLD}")).iter().any(|ip| ip.is_loopback()) {
                resolved = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if resolved {
            ui::ok(format!("*.{TLD} resuelve a 127.0.0.1"));
        } else {
            ui::warn(format!(
                "*.{TLD} no resuelve todavía: revisa {} (¿otro programa usa el puerto 53?)",
                l.log_dir.join("cheka-daemon.log").display()
            ));
        }
        if !addresses("windows.com").is_empty() {
            ui::ok("El DNS normal sigue funcionando");
        } else {
            ui::warn("No resuelve windows.com: revisa la regla con Get-DnsClientNrptRule");
        }
        // El servicio tarda un momento en abrir la pipe.
        let mut ok = false;
        for _ in 0..20 {
            if crate::ipc::call(&l.socket(), &crate::ipc::Request::Ping).is_ok_and(|r| r.ok) {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if ok {
            ui::ok("El daemon responde");
        } else {
            ui::warn(format!("El daemon no responde todavía: revisa {}", l.log_dir.join("cheka-daemon.log").display()));
        }
    }

    let c = ui::colors();
    println!();
    println!(
        "{}{}cheka está listo.{} Crea o clona un proyecto en {} y ábrelo en http://<carpeta>.{TLD}",
        c.green,
        c.bold,
        c.reset,
        sites_dir.display()
    );
    println!("Ayuda: cheka --help");
    Ok(())
}

pub fn uninstall(ctx: &mut Ctx, args: &[String]) -> anyhow::Result<()> {
    ctx.ensure_root()?;
    let purge = args.first().is_some_and(|a| a == "--purge");
    let l = ctx.layout.clone();
    step("Desinstalando cheka");
    if !l.is_test() {
        stop_service(crate::daemon::SERVICE_NAME);
        windows_setup::unregister_daemon()?;
        windows_setup::remove_nrpt()?;
    }
    windows_setup::uninstall_apache(ctx)?;
    windows_setup::remove_hosts_block(&l)?;
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
    ui::ok("Servicios de cheka y Apache, regla DNS y bloque del archivo hosts eliminados. MariaDB y la CA de mkcert se conservan.");
    ui::ok("Tus proyectos no se tocaron.");
    Ok(())
}
