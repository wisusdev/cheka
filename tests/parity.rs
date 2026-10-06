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

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let (root, conf, sites, ext) = (base.join("root"), base.join("conf"), base.join("Sites"), base.join("externo"));

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
        Fixture { _tmp: tmp, base, root, conf }
    }

    fn run(&self, which: Impl, args: &[&str], cwd: &Path) -> Output {
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
            .env_remove("SUDO_USER");
        cmd.output().unwrap()
    }

    /// Todo lo que cheka escribió bajo el prefijo (sin los PHP falsos ni el lock).
    fn snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().display().to_string();
                    if !rel.starts_with("opt/") && !rel.ends_with("refresh.lock") {
                        out.insert(rel, fs::read(&p).unwrap());
                    }
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &self.root, &mut out);
        out
    }

    /// Borra lo generado, conservando los PHP falsos.
    fn reset(&self) {
        for d in ["etc", "run", "var", "usr"] {
            let _ = fs::remove_dir_all(self.root.join(d));
        }
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
        fx.reset();
        let outs: Vec<Output> = steps.iter().map(|(args, cwd)| fx.run(which, args, cwd)).collect();
        results.push((outs, fx.snapshot()));
    }
    let (rust, bash) = (results.pop().unwrap(), results.pop().unwrap());
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
    fx.reset();
    fx.run(Impl::Rust, &["refresh"], &b);
    let snap = fx.snapshot();
    let site = |n: &str| text(&snap[&format!("etc/apache2/cheka/sites/{n}.conf")]);
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
    assert!(!snap.keys().any(|k| k.contains("oculto") || k.contains("archivo")));
}

#[test]
fn refresh_detecta_cambios() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    let mut results = Vec::new();
    for which in [Impl::Bash, Impl::Rust] {
        fx.reset();
        let _ = fs::remove_file(fx.conf.join("certs/sincert.test.pem"));
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
    assert!(snap.contains_key("etc/cheka/php/8.4/php-fpm.conf"));
    assert!(snap.contains_key("usr/local/bin/php8.4"));
}

#[test]
fn version_no_soportada() {
    let fx = Fixture::new();
    let b = fx.base.clone();
    compare("versión inválida", &fx, &[(&["php:install", "7.4"], &b)]);
}
