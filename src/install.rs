//! `install` / `uninstall`. Ambos son idempotentes. En modo prueba (`CHEKA_PREFIX`) generan
//! todos los archivos bajo el prefijo y omiten lo que toca el sistema (apt, systemctl,
//! a2enmod, MariaDB, mkcert), para poder verificarlos sin root.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::layout::{DB_PASS, DB_USER, DNS_PORT, TLD};
use crate::render::system_file;
use crate::util::{mkdir, which, write, write_mode};
use crate::{Ctx, commands, php, refresh, ui, userfs};

/// Unidades de la versión en bash que el daemon reemplaza.
const LEGACY_UNITS: [&str; 3] = ["cheka-watch.path", "cheka-refresh.timer", "cheka-refresh.service"];
const APACHE_MODULES: [&str; 8] =
    ["mpm_event", "proxy", "proxy_fcgi", "setenvif", "rewrite", "ssl", "headers", "socache_shmcb"];

fn step(title: &str) {
    let c = ui::colors();
    println!();
    println!("{}== {title} =={}", c.bold, c.reset);
}

/// Ejecuta un comando en silencio; devuelve si terminó bien.
fn quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn must(program: &str, args: &[&str]) -> Result<()> {
    let st = Command::new(program).args(args).stdout(Stdio::null()).status()?;
    if !st.success() {
        bail!("{program} {} falló", args.join(" "));
    }
    Ok(())
}

/// Comando como el usuario dueño de los proyectos.
fn as_user(ctx: &Ctx, program: &str) -> Command {
    let mut c = Command::new("sudo");
    c.args(["-u", &ctx.id.user, "-H", "--", program]);
    c
}

fn install_mkcert() -> Result<()> {
    if which("mkcert").is_some() || quiet("apt-get", &["install", "-y", "-qq", "mkcert"]) {
        return Ok(());
    }
    ui::info("mkcert no está en apt; descargando binario oficial…");
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        a => a,
    };
    must(
        "curl",
        &["-fsSL", "-o", "/usr/local/bin/mkcert", &format!("https://dl.filippo.io/mkcert/latest?for=linux/{arch}")],
    )?;
    fs::set_permissions("/usr/local/bin/mkcert", std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(())
}

/// Quita el bloque `# >>> cheka … # <<< cheka` de un texto.
fn without_block(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("# >>> cheka") {
            inside = true;
        } else if line.starts_with("# <<< cheka") {
            inside = false;
        } else if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn addresses(host: &str) -> Vec<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    (host, 0).to_socket_addrs().map(|a| a.map(|a| a.ip()).collect()).unwrap_or_default()
}

pub fn install(ctx: &mut Ctx) -> Result<()> {
    ctx.ensure_root()?;
    if ctx.id.user == "root" {
        bail!("Ejecuta 'sudo cheka install' desde tu usuario normal (no como root directo).");
    }
    let l = ctx.layout.clone();
    let test = l.is_test();
    let sysphp = php::system_php();
    let (user, group) = (ctx.id.user.clone(), ctx.id.group.clone());
    let sys = |t: &str| system_file(&l, t, &user, &group);

    step("Paquetes");
    if !test {
        if !quiet("apt-get", &["update", "-qq"]) {
            ui::warn("apt-get update falló; continúo");
        }
        must("apt-get", &["install", "-y", "-qq", &format!("php{sysphp}-fpm"), "dnsmasq-base", "libnss3-tools", "curl"])?;
        install_mkcert()?;
    }
    ui::ok(format!("php{sysphp}-fpm, dnsmasq, mkcert"));

    step("Archivos de cheka");
    let exe = std::env::current_exe()?;
    let target = l.bin.join("cheka");
    mkdir(&l.bin)?;
    if fs::canonicalize(&exe).ok() != fs::canonicalize(&target).ok() {
        // Copia a un temporal y renombra: funciona aunque el binario anterior esté en uso.
        let tmp = l.bin.join(".cheka.nuevo");
        fs::copy(&exe, &tmp).context("No pude copiar el binario")?;
        fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        fs::rename(&tmp, &target)?;
    }
    for d in [&l.etc, &l.opt.join("php"), &l.apache_sites, &l.log_dir] {
        mkdir(d)?;
    }
    write(&l.etc.join("user"), &format!("{user}\n"))?;
    userfs::own(&ctx.id, &l.log_dir)?;
    let conf = ctx.id.conf.clone();
    for d in ["links", "isolated", "secured", "certs", "docroot"] {
        userfs::mkdir(&ctx.id, &conf.join(d))?;
    }
    userfs::mkdir(&ctx.id, &ctx.id.home.join("Sites"))?;
    if ctx.state.default_php.is_none() {
        userfs::append_line(&ctx.id, &conf.join("config"), &format!("default_php={sysphp}"))?;
    }
    userfs::touch(&ctx.id, &conf.join("paths"))?;
    if fs::read_to_string(conf.join("paths")).unwrap_or_default().is_empty() {
        userfs::write(&ctx.id, &conf.join("paths"), &format!("{}\n", ctx.id.home.join("Sites").display()))?;
    }
    mkdir(&l.units)?;
    for unit in ["cheka-php@.service", "cheka-dns.service", "cheka.service"] {
        write_mode(&l.units.join(unit), &sys(unit), 0o644)?;
    }
    // El daemon reemplaza al vigilante y al temporizador de la versión en bash.
    for unit in LEGACY_UNITS {
        if !test {
            quiet("systemctl", &["disable", "--now", unit]);
        }
        userfs::remove(&l.units.join(unit))?;
    }
    userfs::remove(&conf.join(".refresh-request"))?;
    ctx.sys.daemon_reload()?;
    ui::ok(format!("{} instalado, sitios en ~/Sites", target.display()));

    step(&format!("DNS para *.{TLD}"));
    mkdir(l.resolved_dropin.parent().unwrap())?;
    write(&l.resolved_dropin, &sys("resolved.conf"))?;
    if !test {
        ctx.sys.enable("cheka-dns", true)?;
        ctx.sys.restart("cheka-dns")?;
        ctx.sys.restart("systemd-resolved")?;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    ui::ok(format!("dnsmasq en 127.0.0.1:{DNS_PORT}"));

    step(&format!("PHP {sysphp} (FPM)"));
    commands::php_install(ctx, &sysphp)?;

    step("Apache");
    if !test {
        quiet("a2dismod", &["-q", "-f", &format!("php{sysphp}"), "mpm_prefork"]);
        let mut args = vec!["-q"];
        args.extend(APACHE_MODULES);
        must("a2enmod", &args)?;
    }
    let envvars = fs::read_to_string(&l.apache_envvars).unwrap_or_default();
    mkdir(l.apache_envvars.parent().unwrap())?;
    write(&l.apache_envvars, &(without_block(&envvars) + &sys("envvars-block")))?;
    mkdir(l.apache_conf.parent().unwrap())?;
    write(&l.apache_conf, &sys("apache-conf.conf"))?;
    // Como sitio (sites-enabled/cheka.conf) para quedar después de 000-default.
    mkdir(l.apache_site_conf.parent().unwrap())?;
    write(&l.apache_site_conf, &sys("apache-site.conf"))?;
    if !test {
        must("a2ensite", &["-q", "cheka"])?;
        quiet("a2disconf", &["-q", &format!("php{sysphp}-fpm")]);
        must("a2enconf", &["-q", "cheka"])?;
    }
    ui::ok(format!("mpm_event + proxy_fcgi, Apache corre como {user}"));

    step("Sitios");
    ctx.reload_state()?;
    refresh::run(ctx)?;
    if !test {
        ctx.sys.restart("apache2")?;
        ctx.sys.enable("cheka", true)?;
        ctx.sys.restart("cheka")?; // por si ya corría una versión anterior
    }
    ui::ok("Daemon activo: las carpetas nuevas en ~/Sites se publican solas");

    step("MariaDB");
    if test {
        ui::info("Omitido en modo prueba");
    } else if ctx.sys.is_active("mariadb") {
        let sql = format!(
            "CREATE USER IF NOT EXISTS '{user}'@'localhost' IDENTIFIED VIA unix_socket;\n\
             GRANT ALL PRIVILEGES ON *.* TO '{user}'@'localhost' WITH GRANT OPTION;\n\
             CREATE USER IF NOT EXISTS '{DB_USER}'@'localhost' IDENTIFIED BY '{DB_PASS}';\n\
             CREATE USER IF NOT EXISTS '{DB_USER}'@'127.0.0.1' IDENTIFIED BY '{DB_PASS}';\n\
             GRANT ALL PRIVILEGES ON *.* TO '{DB_USER}'@'localhost';\n\
             GRANT ALL PRIVILEGES ON *.* TO '{DB_USER}'@'127.0.0.1';\n\
             FLUSH PRIVILEGES;\n"
        );
        let mut child = Command::new("mariadb").stdin(Stdio::piped()).spawn()?;
        child.stdin.take().unwrap().write_all(sql.as_bytes())?;
        if !child.wait()?.success() {
            bail!("No pude crear los usuarios de MariaDB");
        }
        ui::ok(format!("Usuario '{user}' (sin contraseña, por socket) y '{DB_USER}'/'{DB_PASS}' para tus proyectos"));
    } else {
        ui::warn("MariaDB no está activo; omito la creación de usuarios");
    }

    step("HTTPS local (mkcert)");
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        let out = as_user(ctx, "mkcert").arg("-CAROOT").output()?;
        let caroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !Command::new("mkcert")
            .arg("-install")
            .env("CAROOT", &caroot)
            .env("TRUST_STORES", "system")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            ui::warn("No pude instalar la CA en el sistema");
        }
        quiet("chown", &["-R", &format!("{user}:{group}"), &caroot]);
        let nss = as_user(ctx, "env")
            .args(["TRUST_STORES=nss", "mkcert", "-install"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !nss {
            ui::warn("No pude instalar la CA en los navegadores (NSS)");
        }
        ui::ok("CA local instalada");
    }

    step("Verificación");
    if test {
        ui::info("Omitido en modo prueba");
    } else {
        if addresses(&format!("cheka-check.{TLD}")).iter().any(|ip| ip.is_loopback()) {
            ui::ok(format!("*.{TLD} resuelve a 127.0.0.1"));
        } else {
            ui::warn(format!("*.{TLD} no resuelve todavía"));
        }
        if !addresses("ubuntu.com").is_empty() {
            ui::ok("El DNS normal sigue funcionando");
        } else {
            ui::warn(format!("No resuelve ubuntu.com: revisa {}", l.resolved_dropin.display()));
        }
        match crate::ipc::call(&l.socket(), &crate::ipc::Request::Ping) {
            Ok(r) if r.ok => ui::ok("El daemon responde"),
            _ => ui::warn("El daemon no responde todavía: revisa 'systemctl status cheka'"),
        }
    }

    let c = ui::colors();
    println!();
    println!(
        "{}{}cheka está listo.{} Crea o clona un proyecto en ~/Sites y ábrelo en http://<carpeta>.{TLD}",
        c.green, c.bold, c.reset
    );
    println!("Ayuda: cheka --help");
    Ok(())
}

/// Envoltorios `php8.X` que generó cheka (no toca otros archivos).
fn generated_wrappers(bin: &Path) -> Vec<std::path::PathBuf> {
    php::SUPPORTED
        .iter()
        .map(|v| bin.join(format!("php{v}")))
        .filter(|p| fs::read_to_string(p).is_ok_and(|s| s.starts_with("#!/bin/sh\n# Generado por cheka")))
        .collect()
}

pub fn uninstall(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    ctx.ensure_root()?;
    let purge = args.first().is_some_and(|a| a == "--purge");
    let l = ctx.layout.clone();
    let test = l.is_test();
    let sysphp = php::system_php();
    step("Desinstalando cheka");
    if !test {
        for unit in ["cheka", "cheka-dns"].into_iter().chain(LEGACY_UNITS) {
            quiet("systemctl", &["disable", "--now", unit]);
        }
        for unit in commands::php_units() {
            quiet("systemctl", &["disable", "--now", &unit]);
        }
    }
    for unit in ["cheka-php@.service", "cheka-dns.service", "cheka.service"].into_iter().chain(LEGACY_UNITS) {
        userfs::remove(&l.units.join(unit))?;
    }
    ctx.sys.daemon_reload()?;
    userfs::remove(&l.resolved_dropin)?;
    if !test {
        quiet("systemctl", &["restart", "systemd-resolved"]);
        quiet("a2disconf", &["-q", "cheka"]);
        quiet("a2dissite", &["-q", "cheka"]);
    }
    userfs::remove(&l.apache_conf)?;
    userfs::remove(&l.apache_site_conf)?;
    let _ = fs::remove_dir_all(l.apache_sites.parent().unwrap());
    if let Ok(envvars) = fs::read_to_string(&l.apache_envvars) {
        write(&l.apache_envvars, &without_block(&envvars))?;
    }
    if !test {
        quiet("a2dismod", &["-q", "-f", "mpm_event", "proxy_fcgi"]);
        quiet("a2enmod", &["-q", "mpm_prefork", &format!("php{sysphp}")]);
        if !quiet("systemctl", &["restart", "apache2"]) {
            ui::warn("Apache no arrancó; revisa 'apache2ctl -t'");
        }
    }
    for w in generated_wrappers(&l.bin) {
        userfs::remove(&w)?;
    }
    let _ = fs::remove_file(l.socket());
    if purge {
        for d in [&l.opt, &l.etc, &l.log_dir, &ctx.id.conf] {
            let _ = fs::remove_dir_all(d);
        }
        ui::ok("Eliminados también los binarios de PHP y la configuración de usuario");
    } else {
        ui::ok(format!(
            "Se conservan {}, {} y {} (usa --purge para borrarlos)",
            l.opt.display(),
            l.etc.display(),
            ctx.id.conf.display()
        ));
    }
    userfs::remove(&l.bin.join("cheka"))?;
    ui::ok("Apache vuelve a mod_php + prefork. Tus proyectos en ~/Sites no se tocaron.");
    Ok(())
}
