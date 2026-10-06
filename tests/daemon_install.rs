//! Hito 1.3: `install`/`uninstall` en modo prefijo y el daemon en vivo (sin root).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use cheka::ipc::{self, Request};

struct Env {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    conf: PathBuf,
    sites: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let (root, conf, sites) = (base.join("root"), base.join("conf"), base.join("Sites"));
        fs::create_dir_all(&sites).unwrap();
        fs::create_dir_all(&conf).unwrap();
        fs::write(conf.join("paths"), format!("{}\n", sites.display())).unwrap();
        // envvars de Apache con un bloque viejo de cheka que debe reemplazarse
        fs::create_dir_all(root.join("etc/apache2")).unwrap();
        fs::write(
            root.join("etc/apache2/envvars"),
            "export APACHE_LOG_DIR=/var/log/apache2\n# >>> cheka\nexport VIEJO=1\n# <<< cheka\nexport OTRA=2\n",
        )
        .unwrap();
        Env { _tmp: tmp, base, root, conf, sites }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cheka"));
        c.args(args)
            .current_dir(&self.base)
            .env("CHEKA_PREFIX", &self.root)
            .env("CHEKA_CONF", &self.conf)
            .env("CHEKA_USER", whoami())
            .env_remove("SUDO_USER");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }
}

fn whoami() -> String {
    String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap().trim().to_string()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[track_caller]
fn assert_ok(out: &Output) {
    assert!(out.status.success(), "falló:\n{}{}", text(&out.stdout), text(&out.stderr));
}

fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "tiempo agotado esperando: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Genera las unidades con la función `write_units` del script de bash, sin ejecutar su
/// `main`, para comparar contra las de Rust.
fn bash_units(env: &Env, out: &Path) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("legacy/cheka.sh");
    let st = Command::new("bash")
        .arg("-c")
        .arg(r#"source <(sed '/^# -* main -*$/,$d' "$1"); write_units"#)
        .arg("_")
        .arg(&script)
        .env("CHEKA_PREFIX", out)
        .env("CHEKA_CONF", &env.conf)
        .env("CHEKA_USER", whoami())
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn install_en_modo_prefijo() {
    let env = Env::new();
    fs::create_dir_all(env.sites.join("demo")).unwrap();
    fs::write(env.sites.join("demo/index.php"), "").unwrap();
    // Unidades viejas de la versión en bash: install debe retirarlas.
    fs::create_dir_all(env.root.join("etc/systemd/system")).unwrap();
    for u in ["cheka-watch.path", "cheka-refresh.timer", "cheka-refresh.service"] {
        fs::write(env.root.join("etc/systemd/system").join(u), "viejo").unwrap();
    }
    fs::write(env.conf.join(".refresh-request"), "1").unwrap();

    let out = env.run(&["install"]);
    assert_ok(&out);
    let stdout = text(&out.stdout);
    for step in ["== Paquetes ==", "== Apache ==", "== Sitios ==", "cheka está listo."] {
        assert!(stdout.contains(step), "falta '{step}' en:\n{stdout}");
    }

    // Unidades: las de PHP y DNS idénticas a las de bash; la del daemon es nueva.
    let bash = tempfile::tempdir().unwrap();
    bash_units(&env, bash.path());
    for u in ["cheka-php@.service", "cheka-dns.service"] {
        // bash usa su propio prefijo en ExecStart; se normaliza antes de comparar
        let theirs = fs::read_to_string(bash.path().join("etc/systemd/system").join(u))
            .unwrap()
            .replace(&bash.path().display().to_string(), &env.root.display().to_string());
        assert_eq!(env.read(&format!("etc/systemd/system/{u}")), theirs, "{u} distinta de bash");
    }
    let daemon = env.read("etc/systemd/system/cheka.service");
    assert!(daemon.contains(&format!("ExecStart={}/usr/local/bin/cheka daemon", env.root.display())));
    for u in ["cheka-watch.path", "cheka-refresh.timer", "cheka-refresh.service"] {
        assert!(!env.root.join("etc/systemd/system").join(u).exists(), "{u} debió retirarse");
    }
    assert!(!env.conf.join(".refresh-request").exists());
    // install migra el estado: cheka.toml nuevo, lo anterior en legacy/
    let toml = fs::read_to_string(env.conf.join("cheka.toml")).unwrap();
    assert!(toml.contains(&format!("paths = [\"{}\"]", env.sites.display())), "{toml}");
    assert!(env.conf.join("legacy/paths").exists() && !env.conf.join("paths").exists());

    // DNS, Apache y estado
    assert_eq!(
        env.read("etc/systemd/resolved.conf.d/cheka.conf"),
        "# Generado por cheka: solo las consultas *.test van al dnsmasq local.\n[Resolve]\nDNS=127.0.0.1:5300\nDomains=~test\n"
    );
    let envvars = env.read("etc/apache2/envvars");
    assert!(envvars.starts_with("export APACHE_LOG_DIR=/var/log/apache2\nexport OTRA=2\n# >>> cheka\n"));
    assert!(envvars.contains(&format!("export APACHE_RUN_USER={}\n", whoami())));
    assert!(!envvars.contains("VIEJO"));
    assert!(env.read("etc/apache2/sites-available/cheka.conf").contains(&format!(
        "IncludeOptional {}/etc/apache2/cheka/sites/*.conf",
        env.root.display()
    )));
    assert!(env.read("etc/apache2/conf-available/cheka.conf").contains("ProxyTimeout 600"));
    assert!(env.read("etc/apache2/cheka/sites/demo.conf").contains("ServerName demo.test"));
    assert_eq!(env.read("etc/cheka/user"), format!("{}\n", whoami()));
    assert!(env.root.join("usr/local/bin/cheka").is_file());
    // Con el daemon instalado, refresh ya no genera el vigilante de bash.
    assert!(!env.root.join("etc/systemd/system/cheka-watch.path").exists());

    // Idempotente: una segunda instalación deja exactamente los mismos archivos.
    let snapshot = || {
        let mut v = Vec::new();
        for e in walk(&env.root) {
            if !e.ends_with("refresh.lock") && !e.ends_with("last-refresh") {
                v.push((e.clone(), fs::read(&e).unwrap()));
            }
        }
        v
    };
    let before = snapshot();
    assert_ok(&env.run(&["install"]));
    assert!(before == snapshot(), "la segunda instalación cambió archivos");

    // uninstall: revierte lo de cheka y conserva lo ajeno
    let out = env.run(&["uninstall"]);
    assert_ok(&out);
    for gone in [
        "etc/systemd/system/cheka.service",
        "etc/systemd/system/cheka-php@.service",
        "etc/systemd/resolved.conf.d/cheka.conf",
        "etc/apache2/sites-available/cheka.conf",
        "etc/apache2/conf-available/cheka.conf",
        "etc/apache2/cheka",
        "usr/local/bin/cheka",
    ] {
        assert!(!env.root.join(gone).exists(), "{gone} debió eliminarse");
    }
    assert_eq!(env.read("etc/apache2/envvars"), "export APACHE_LOG_DIR=/var/log/apache2\nexport OTRA=2\n");
    assert!(env.conf.join("cheka.toml").exists(), "sin --purge se conserva el estado del usuario");
    assert!(env.sites.join("demo/index.php").exists(), "nunca se tocan los proyectos");
    assert_ok(&env.run(&["uninstall", "--purge"]));
    assert!(!env.conf.exists());
    assert!(env.sites.join("demo/index.php").exists());
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && !p.is_symlink() {
                out.extend(walk(&p));
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn daemon_atiende_y_vigila() {
    let env = Env::new();
    // PHP "instalado" para que los vhosts no dependan del PHP del sistema
    let fpm = env.root.join("opt/cheka/php/8.2/php-fpm");
    fs::create_dir_all(fpm.parent().unwrap()).unwrap();
    fs::write(&fpm, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&fpm, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    fs::write(env.conf.join("config"), "default_php=8.2\n").unwrap();

    let socket = env.root.join("run/cheka/cheka.sock");
    let sites_dir = env.root.join("etc/apache2/cheka/sites");
    let _daemon = Daemon(env.cmd(&["daemon"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    wait_for("socket del daemon", Duration::from_secs(10), || socket.exists());

    // 1. API: ping
    let pong = ipc::call(&socket, &Request::Ping).unwrap();
    assert!(pong.ok && pong.message == "pong");

    // 2. La CLI pide el refresh al daemon (no lo hace ella misma)
    fs::create_dir_all(env.base.join("externo/app")).unwrap();
    let out = env.cmd(&["link", "app"]).current_dir(env.base.join("externo/app")).output().unwrap();
    assert_ok(&out);
    assert!(text(&out.stdout).contains("Apache actualizado"), "{}", text(&out.stdout));
    assert!(sites_dir.join("app.conf").exists());

    // 3. Una carpeta nueva en ~/Sites se publica sola
    fs::create_dir_all(env.sites.join("nuevo")).unwrap();
    wait_for("vhost de carpeta nueva", Duration::from_secs(10), || sites_dir.join("nuevo.conf").exists());
    fs::remove_dir_all(env.sites.join("nuevo")).unwrap();
    wait_for("retiro de carpeta borrada", Duration::from_secs(10), || !sites_dir.join("nuevo.conf").exists());

    // 4. Una ruta aparcada después de arrancar también se vigila
    let otra = env.base.join("Otra");
    fs::create_dir_all(&otra).unwrap();
    assert_ok(&env.run(&["park", &otra.display().to_string()]));
    fs::create_dir_all(otra.join("tardio")).unwrap();
    wait_for("vhost en ruta aparcada tarde", Duration::from_secs(10), || sites_dir.join("tardio.conf").exists());

    // 5. Petición inválida: responde con error, no se cae
    {
        use std::io::{BufRead, BufReader, Write};
        let mut s = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        s.write_all(b"esto no es json\n").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        assert!(line.contains(r#""ok":false"#), "{line}");
    }
    assert!(ipc::call(&socket, &Request::Ping).unwrap().ok, "el daemon debe seguir vivo");
}

/// Ajustes y extensiones por versión (solo si el equipo tiene el PHP de apt).
#[test]
fn ajustes_y_extensiones_de_php() {
    let v = ["8.5", "8.4", "8.3"].into_iter().find(|v| Path::new(&format!("/usr/sbin/php-fpm{v}")).exists());
    let Some(v) = v else {
        eprintln!("sin PHP de apt; se omite");
        return;
    };
    let env = Env::new();
    fs::write(env.conf.join("cheka.toml"), "version = 1\n").unwrap();
    assert_ok(&env.run(&["php:install", v]));
    let dir = env.root.join(format!("etc/cheka/php/{v}"));
    let system = fs::read_dir(format!("/etc/php/{v}/fpm/conf.d")).unwrap().count();
    assert_eq!(fs::read_dir(dir.join("ext.d")).unwrap().count(), system, "ext.d refleja al sistema");

    // ajustes: se guardan en cheka.toml y PHP-FPM los ve
    assert_ok(&env.run(&["php:ini", v, "upload_max_filesize=321M"]));
    assert!(fs::read_to_string(dir.join("conf.d/99-cheka.ini")).unwrap().contains("upload_max_filesize = 321M"));
    let info: serde_json::Value = serde_json::from_slice(&env.run(&["php:info", v, "--json"]).stdout).unwrap();
    let upload = info["settings"].as_array().unwrap().iter().find(|s| s["key"] == "upload_max_filesize").unwrap();
    assert_eq!((upload["value"].as_str(), upload["custom"].as_bool()), (Some("321M"), Some(true)));
    assert!(!env.run(&["php:ini", v, "memory_limit=1G\nextension=x.so"]).status.success());

    // extensiones: desactivar una que el sistema activa, y volver a activarla
    let ext = fs::read_dir(dir.join("ext.d")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).find(|f| f.contains("zip")).map(|_| "zip").unwrap_or("ctype");
    assert_ok(&env.run(&["php:ext", v, "disable", ext]));
    let loaded = |env: &Env| -> bool {
        let info: serde_json::Value = serde_json::from_slice(&env.run(&["php:info", v, "--json"]).stdout).unwrap();
        info["extensions"].as_array().unwrap().iter().any(|e| e["name"] == ext && e["loaded"] == true)
    };
    assert!(!loaded(&env), "PHP-FPM no debe cargar {ext} tras desactivarla");
    assert_ok(&env.run(&["php:ext", v, "enable", ext]));
    assert!(loaded(&env));
    assert!(!fs::read_to_string(env.conf.join("cheka.toml")).unwrap().contains(".extensions]"), "sin ajustes redundantes");
    assert!(!env.run(&["php:ext", v, "enable", "no-existe"]).status.success());
}
