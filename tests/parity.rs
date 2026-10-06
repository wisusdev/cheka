//! Pruebas de paridad: la versión en bash (`./cheka`) y la de Rust (`cheka-rs`) se ejecutan
//! sobre el mismo proyecto de prueba en modo prefijo (sin root, sin servicios) y deben
//! generar exactamente los mismos archivos y la misma salida.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Copy, Debug)]
enum Impl {
    Bash,
    Rust,
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    conf: PathBuf,
}

fn touch(p: &Path, contents: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, contents).unwrap();
}

fn executable(p: &Path) {
    touch(p, "#!/bin/sh\nexit 0\n");
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `mkcert` falso y determinista: escribe en los archivos pedidos los nombres recibidos.
const FAKE_MKCERT: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in
    -cert-file) cert="$2"; shift 2 ;;
    -key-file) key="$2"; shift 2 ;;
    *) names="$names $1"; shift ;;
  esac
done
echo "cert:$names" > "$cert"
echo "key:$names" > "$key"
"#;

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let fx = Fixture { root: base.join("root"), conf: base.join("conf"), base, _tmp: tmp };
        fx.populate();
        fx
    }

    /// (Re)crea desde cero el proyecto de prueba, siempre en la misma ruta.
    fn populate(&self) {
        for e in fs::read_dir(&self.base).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() && !p.is_symlink() { fs::remove_dir_all(&p).unwrap() } else { fs::remove_file(&p).unwrap() }
        }
        let base = &self.base;
        let (root, conf, sites, ext) = (base.join("root"), base.join("conf"), base.join("Sites"), base.join("externo"));
        fs::create_dir_all(base.join("Otros/uno")).unwrap();
        touch(&base.join("bin/mkcert"), FAKE_MKCERT);
        fs::set_permissions(base.join("bin/mkcert"), fs::Permissions::from_mode(0o755)).unwrap();

        // Proyectos
        touch(&sites.join("blog/wp-config.php"), "define( 'MULTISITE', true );\ndefine( 'SUBDOMAIN_INSTALL', true );\n");
        touch(&sites.join("red/wp-config.php"), "define('MULTISITE', true);\n");
        touch(&sites.join("simple/wp-load.php"), "");
        touch(&sites.join("api/artisan"), "");
        fs::create_dir_all(sites.join("api/public")).unwrap();
        touch(&sites.join("ci4/spark"), "");
        fs::create_dir_all(sites.join("ci3/system")).unwrap();
        fs::create_dir_all(sites.join("ci3/application")).unwrap();
        touch(&sites.join("plano/index.php"), "");
        touch(&sites.join("My_Site/public/index.php"), "");
        touch(&sites.join("bedrock/web/wp-config.php"), "");
        fs::create_dir_all(sites.join("legado/htdocs")).unwrap();
        touch(&sites.join("seguro/index.php"), "");
        fs::create_dir_all(sites.join("sincert")).unwrap();
        fs::create_dir_all(sites.join("aislado/sub/dir")).unwrap();
        fs::create_dir_all(sites.join("faltante")).unwrap();
        fs::create_dir_all(sites.join(".oculto")).unwrap();
        touch(&sites.join("archivo.txt"), "");
        touch(&ext.join("proyecto-x/index.php"), "");
        touch(&ext.join("otro/artisan"), "");
        symlink(ext.join("proyecto-x"), sites.join("alias")).unwrap();

        // Estado del usuario (formato de la versión en bash)
        touch(&conf.join("config"), "default_php=8.2\n");
        touch(&conf.join("paths"), &format!("{}\n{}\n", sites.display(), base.join("no-existe").display()));
        fs::create_dir_all(conf.join("links")).unwrap();
        symlink(ext.join("otro"), conf.join("links/px")).unwrap();
        symlink(ext.join("proyecto-x"), conf.join("links/plano")).unwrap(); // pisa al aparcado
        touch(&conf.join("isolated/aislado"), "8.4\n");
        touch(&conf.join("isolated/faltante"), "8.1\n");
        touch(&conf.join("secured/seguro"), "");
        touch(&conf.join("secured/sincert"), ""); // marcado, pero sin certificado
        touch(&conf.join("certs/seguro.test.pem"), "cert");
        touch(&conf.join("certs/seguro.test-key.pem"), "key");
        touch(&conf.join("docroot/legado"), "htdocs\n");

        // PHP "instalados" (binarios falsos)
        for v in ["8.2", "8.4"] {
            executable(&root.join(format!("opt/cheka/php/{v}/php-fpm")));
        }
    }

    fn run(&self, which: Impl, args: &[&str], cwd: &Path) -> Output {
        self.run_input(which, args, cwd, "")
    }

    fn run_input(&self, which: Impl, args: &[&str], cwd: &Path, input: &str) -> Output {
        let mut cmd = match which {
            Impl::Bash => {
                let mut c = Command::new("bash");
                c.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("cheka"));
                c
            }
            Impl::Rust => Command::new(env!("CARGO_BIN_EXE_cheka-rs")),
        };
        cmd.args(args)
            .current_dir(cwd)
            .env("CHEKA_PREFIX", &self.root)
            .env("CHEKA_CONF", &self.conf)
            .env("CHEKA_USER", whoami())
            .env("LC_ALL", "C") // `sort` de bash en orden de bytes, como BTreeMap
            .env("PATH", format!("{}:{}", self.base.join("bin").display(), std::env::var("PATH").unwrap()))
            .env_remove("SUDO_USER")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Todo el árbol de prueba: lo que cheka escribió bajo el prefijo, el estado del
    /// usuario y los proyectos (sin los PHP falsos ni el lock). Los symlinks se registran
    /// con su destino, sin seguirlos.
    fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                if rel.starts_with("root/opt") || rel.ends_with("refresh.lock") || rel == "bin" {
                    continue;
                }
                let meta = fs::symlink_metadata(&p).unwrap();
                if meta.is_symlink() {
                    out.insert(rel, format!("-> {}", fs::read_link(&p).unwrap().display()).into_bytes());
                } else if meta.is_dir() {
                    out.insert(format!("{rel}/"), Vec::new());
                    walk(&p, base, out);
                } else {
                    out.insert(rel, fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.base, &self.base, &mut out);
        out
    }
}

fn whoami() -> String {
    String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap().trim().to_string()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[track_caller]
fn assert_same_output(what: &str, bash: &Output, rust: &Output) {
    assert_eq!(text(&bash.stdout), text(&rust.stdout), "{what}: stdout distinto");
    assert_eq!(text(&bash.stderr), text(&rust.stderr), "{what}: stderr distinto");
    assert_eq!(bash.status.code(), rust.status.code(), "{what}: código de salida distinto");
}

#[track_caller]
fn assert_same_files(what: &str, bash: &BTreeMap<String, Vec<u8>>, rust: &BTreeMap<String, Vec<u8>>) {
    let names = |m: &BTreeMap<String, Vec<u8>>| m.keys().cloned().collect::<Vec<_>>();
    assert_eq!(names(bash), names(rust), "{what}: archivos distintos");
    for (k, v) in bash {
        assert_eq!(text(v), text(&rust[k]), "{what}: contenido distinto en {k}");
    }
}

/// Ejecuta la misma secuencia de comandos con cada implementación desde cero y compara
/// salidas y archivos.
fn compare(what: &str, fx: &Fixture, steps: &[(&[&str], &Path)]) {
    let mut results = Vec::new();
    for which in [Impl::Bash, Impl::Rust] {
        fx.populate();
        let outs: Vec<Output> = steps.iter().map(|(args, cwd)| fx.run(which, args, cwd)).collect();
        results.push((outs, fx.snapshot()));
    }
    let (rust, bash) = (results.pop().unwrap(), results.pop().unwrap());
    if std::env::var_os("PARITY_SHOW").is_some() {
        for (i, o) in rust.0.iter().enumerate() {
            eprintln!("[{i}] {:?} → {:?}\n{}{}", steps[i].0, o.status.code(), text(&o.stdout), text(&o.stderr));
        }
    }
    for (i, (b, r)) in bash.0.iter().zip(&rust.0).enumerate() {
        assert_same_output(&format!("{what}, paso {i} ({:?})", steps[i].0), b, r);
    }
    assert_same_files(what, &bash.1, &rust.1);
}

#[test]
fn refresh_genera_lo_mismo() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    compare("refresh", &fx, &[(&["refresh"], &b), (&["refresh"], &b)]);

    // La salida tiene que haber detectado de verdad cada caso (no solo coincidir).
    fx.populate();
    fx.run(Impl::Rust, &["refresh"], &b);
    let snap = fx.snapshot();
    let site = |n: &str| text(&snap[&format!("root/etc/apache2/cheka/sites/{n}.conf")]);
    assert!(site("blog").contains("(wp-multisite-subdominios, PHP 8.2)"));
    assert!(site("red").contains("(wp-multisite, PHP 8.2)"));
    assert!(site("my-site").contains("/My_Site/public\""));
    assert!(site("legado").contains("(personalizado,"));
    assert!(site("aislado").contains("php-8.4/fpm.sock"));
    assert!(site("faltante").contains("php-8.2/fpm.sock"), "PHP faltante debe caer en el de por defecto");
    assert!(site("seguro").contains("<VirtualHost *:443>"));
    assert!(!site("sincert").contains("<VirtualHost *:443>"), "sin certificado no hay HTTPS");
    assert!(site("plano").contains("externo/proyecto-x"), "el enlace pisa a la carpeta aparcada");
    assert!(site("proyecto-x").contains("externo/proyecto-x"), "symlink en Sites usa el nombre real");
    assert!(site("px").contains("(laravel,"));
    assert!(!snap.keys().any(|k| k.starts_with("root/") && (k.contains("oculto") || k.contains("archivo"))));
}

#[test]
fn refresh_detecta_cambios() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    let mut results = Vec::new();
    for which in [Impl::Bash, Impl::Rust] {
        fx.populate();
        fx.run(which, &["refresh"], &b);
        touch(&fx.conf.join("certs/sincert.test.pem"), "cert");
        let out = fx.run(which, &["refresh"], &b);
        results.push((out, fx.snapshot()));
    }
    let (rust, bash) = (results.pop().unwrap(), results.pop().unwrap());
    assert_same_output("refresh tras cambio", &bash.0, &rust.0);
    assert!(text(&rust.0.stdout).contains("Apache actualizado"));
    assert_same_files("refresh tras cambio", &bash.1, &rust.1);
}

#[test]
fn comandos_de_lectura() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    let inside = fx.base.join("Sites/aislado/sub/dir");
    let outside = std::env::temp_dir();
    compare(
        "lectura",
        &fx,
        &[
            (&["sites"], &b),
            (&["paths"], &b),
            (&["versions"], &b),
            (&["which-php"], &inside),
            (&["which-php"], &outside),
        ],
    );
}

#[test]
fn php_install_configura_lo_mismo() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    compare("php:install", &fx, &[(&["php:install", "8.4"], &b), (&["php:install", "php@8.2"], &b)]);
    let snap = fx.snapshot();
    assert!(snap.contains_key("root/etc/cheka/php/8.4/php-fpm.conf"));
    assert!(snap.contains_key("root/usr/local/bin/php8.4"));
}

#[test]
fn version_no_soportada() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    compare("versión inválida", &fx, &[(&["php:install", "7.4"], &b)]);
}

/// Hito 1.2: comandos que modifican el estado. Una sola secuencia larga, porque cada paso
/// depende de los anteriores; se comparan la salida de cada paso y el árbol final.
#[test]
fn comandos_que_modifican_el_estado() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    let (sites, ext) = (b.join("Sites"), b.join("externo"));
    let (api, legado, px, otros) = (sites.join("api"), sites.join("legado"), ext.join("proyecto-x"), b.join("Otros"));
    let no_existe = b.join("no-existe-x");
    let (otros_s, no_existe_s) = (otros.display().to_string(), no_existe.display().to_string());
    compare(
        "mutaciones",
        &fx,
        &[
            // park / forget
            (&["park"], &otros),
            (&["park"], &otros),
            (&["park", &no_existe_s], &b),
            (&["paths"], &b),
            (&["forget", &otros_s], &b),
            (&["forget", &otros_s], &b),
            // link / unlink
            (&["link"], &px),
            (&["link", "Mi Link"], &px),
            (&["unlink", "mi-link"], &b),
            (&["unlink", "nada"], &b),
            // isolate / unisolate
            (&["isolate"], &api),
            (&["isolate", "9.9"], &api),
            (&["isolate", "8.4"], &api),
            (&["isolate", "php@8.4", "--site=blog"], &b),
            (&["isolate", "8.4"], &b),
            (&["unisolate"], &api),
            (&["unisolate", "--site=nope"], &b),
            // use
            (&["use"], &b),
            (&["use", "8.4"], &b),
            (&["use", "7.4"], &b),
            (&["use", "php8.2"], &b),
            // docroot
            (&["docroot"], &legado),
            (&["docroot", "nope"], &legado),
            (&["docroot", "htdocs/"], &legado),
            // secure / unsecure
            (&["secure"], &api),
            (&["secure", "ci4"], &b),
            (&["unsecure", "ci4"], &b),
            (&["secure", "nope"], &b),
            // db (sin tocar MariaDB)
            (&["db"], &b),
            (&["db", "otra-cosa"], &b),
            // new: validaciones y el tipo php (sin red)
            (&["new"], &b),
            (&["new", "rails", "x"], &b),
            (&["new", "php", "x", "--multisite"], &b),
            (&["new", "php", "x", "--foo"], &b),
            (&["new", "php", "plano"], &b),
            (&["new", "php", "px"], &b),
            (&["new", "php", "!!"], &b),
            (&["php:install", "8.2"], &b),
            (&["new", "php", "Nuevo Sitio"], &b),
            (&["php:install", "8.4"], &b),
            (&["new", "php", "otra", "--php=8.4", "--secure"], &b),
            (&["sites"], &b),
        ],
    );
}

fn mariadb_disponible() -> bool {
    Command::new("mariadb").args(["-e", "SELECT 1"]).output().is_ok_and(|o| o.status.success())
}

/// `cheka db` contra el MariaDB real (se omite si el usuario no tiene acceso).
#[test]
fn db_contra_mariadb_real() {
    if !mariadb_disponible() {
        eprintln!("MariaDB no disponible; se omite");
        return;
    }
    let fx = Fixture::new();
    let b = fx.base.clone();
    let name = "cheka_parity_test";
    let dump = b.join("dump.sql.gz").display().to_string();
    let mut results = Vec::new();
    for which in [Impl::Bash, Impl::Rust] {
        fx.populate();
        let mut outs = vec![
            fx.run(which, &["db", "create", name], &b),
            fx.run(which, &["db", "create", "mal-nombre"], &b),
        ];
        Command::new("mariadb").args(["-e", &format!("CREATE TABLE `{name}`.t (x INT); INSERT INTO `{name}`.t VALUES (7);")]).status().unwrap();
        let list = fx.run(which, &["db", "list"], &b);
        assert!(text(&list.stdout).lines().any(|l| l == name), "{which:?}: db list no muestra la base");
        outs.push(fx.run(which, &["db", "export", name, &dump], &b));
        outs.push(fx.run_input(which, &["db", "drop", name], &b, "otro\n"));
        outs.push(fx.run_input(which, &["db", "drop", name], &b, &format!("{name}\n")));
        outs.push(fx.run(which, &["db", "import", &dump, name], &b));
        let check = Command::new("mariadb").args(["-N", "-e", &format!("SELECT x FROM `{name}`.t")]).output().unwrap();
        assert_eq!(text(&check.stdout).trim(), "7", "{which:?}: el import no restauró los datos");
        outs.push(fx.run_input(which, &["db", "drop", name], &b, &format!("{name}\n")));
        results.push(outs);
    }
    let (rust, bash) = (results.pop().unwrap(), results.pop().unwrap());
    for (i, (bo, ro)) in bash.iter().zip(&rust).enumerate() {
        assert_same_output(&format!("db paso {i}"), bo, ro);
    }
}
