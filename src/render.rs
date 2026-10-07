//! Plantillas (minijinja). Producen exactamente el mismo texto que la versión en bash;
//! lo verifican las pruebas de paridad en `tests/parity.rs`.

use std::path::Path;
use std::sync::OnceLock;

use minijinja::{AutoEscape, Environment, UndefinedBehavior, context};

use crate::layout::{Layout, TLD};
use crate::php;

fn env() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        env.set_keep_trailing_newline(true);
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_auto_escape_callback(|_| AutoEscape::None);
        for (name, src) in [
            ("linux/vhost.conf.j2", include_str!("../templates/linux/vhost.conf.j2")),
            ("linux/vhost-body.j2", include_str!("../templates/linux/vhost-body.j2")),
            ("linux/php-fpm.conf.j2", include_str!("../templates/linux/php-fpm.conf.j2")),
            ("linux/php.ini.j2", include_str!("../templates/linux/php.ini.j2")),
            ("linux/php-cli.sh.j2", include_str!("../templates/linux/php-cli.sh.j2")),
            ("linux/cheka-php@.service.j2", include_str!("../templates/linux/cheka-php@.service.j2")),
            ("linux/cheka-dns.service.j2", include_str!("../templates/linux/cheka-dns.service.j2")),
            ("linux/cheka.service.j2", include_str!("../templates/linux/cheka.service.j2")),
            ("linux/resolved.conf.j2", include_str!("../templates/linux/resolved.conf.j2")),
            ("linux/apache-conf.conf.j2", include_str!("../templates/linux/apache-conf.conf.j2")),
            ("linux/apache-site.conf.j2", include_str!("../templates/linux/apache-site.conf.j2")),
            ("linux/envvars-block.j2", include_str!("../templates/linux/envvars-block.j2")),
            ("windows/vhost.conf.j2", include_str!("../templates/windows/vhost.conf.j2")),
            ("windows/vhost-body.j2", include_str!("../templates/windows/vhost-body.j2")),
            ("windows/php.ini.j2", include_str!("../templates/windows/php.ini.j2")),
            ("windows/apache-conf.conf.j2", include_str!("../templates/windows/apache-conf.conf.j2")),
            ("windows/apache-site.conf.j2", include_str!("../templates/windows/apache-site.conf.j2")),
        ] {
            env.add_template(name, src).expect("plantilla inválida");
        }
        env
    })
}

/// Archivos de sistema que genera `install` (unidades, DNS, Apache).
pub fn system_file(layout: &Layout, template: &str, user: &str, group: &str) -> String {
    render(
        &format!("linux/{template}.j2"),
        context! {
            bin => layout.bin.display().to_string(),
            apache_sites => layout.apache_sites.display().to_string(),
            tld => TLD,
            dns_port => crate::layout::DNS_PORT,
            user, group,
        },
    )
}

fn render(name: &str, ctx: minijinja::Value) -> String {
    env().get_template(name).and_then(|t| t.render(ctx)).unwrap_or_else(|e| panic!("{name}: {e:#}"))
}

pub struct Vhost<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub path: &'a Path,
    pub docroot: &'a Path,
    pub php: &'a str,
    /// (certificado, llave) si el sitio usa HTTPS.
    pub tls: Option<(&'a Path, &'a Path)>,
}

#[cfg(unix)]
pub fn vhost(layout: &Layout, v: &Vhost) -> String {
    let (cert, key) = v.tls.map(|(c, k)| (c.display().to_string(), k.display().to_string())).unzip();
    render(
        "linux/vhost.conf.j2",
        context! {
            name => v.name,
            tld => TLD,
            kind => v.kind,
            php => v.php,
            path => v.path.display().to_string(),
            docroot => v.docroot.display().to_string(),
            socket => php::socket(layout, v.php).display().to_string(),
            log_dir => layout.log_dir.display().to_string(),
            secure => v.tls.is_some(),
            cert => cert.unwrap_or_default(),
            key => key.unwrap_or_default(),
        },
    )
}

/// Windows: PHP con `mod_fcgid` (`FcgidWrapper` al `php-cgi.exe` de la versión del sitio).
#[cfg(windows)]
pub fn vhost(layout: &Layout, v: &Vhost) -> String {
    let (cert, key) = v.tls.map(|(c, k)| (slash(c), slash(k))).unzip();
    render(
        "windows/vhost.conf.j2",
        context! {
            name => v.name,
            tld => TLD,
            kind => v.kind,
            php => v.php,
            path => slash(v.path),
            docroot => slash(v.docroot),
            php_cgi => slash(&php::fpm_bin(layout, v.php)),
            log_dir => slash(&layout.log_dir),
            secure => v.tls.is_some(),
            cert => cert.unwrap_or_default(),
            key => key.unwrap_or_default(),
        },
    )
}

/// Apache en Windows prefiere `/` (la `\` escapa dentro de comillas).
pub fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// php.ini de una versión en Windows (va junto a `php.exe`; ver `windows_setup`).
pub struct WindowsPhpIni<'a> {
    pub v: &'a str,
    pub ext_dir: &'a Path,
    pub extensions: &'a [String],
    /// Las que van con `zend_extension` (OPcache, Xdebug).
    pub zend_extensions: &'a [String],
    pub cacert: &'a Path,
    pub tz: &'a str,
}

pub fn windows_php_ini(layout: &Layout, ini: &WindowsPhpIni) -> String {
    render(
        "windows/php.ini.j2",
        context! {
            v => ini.v,
            ext_dir => slash(ini.ext_dir),
            extensions => ini.extensions,
            zend_extensions => ini.zend_extensions,
            cacert => slash(ini.cacert),
            tz => ini.tz,
            log_dir => slash(&layout.log_dir),
        },
    )
}

/// `cheka.conf` de Apache en Windows: mod_fcgid y el entorno que reciben los php-cgi.
pub fn windows_apache_conf(env: &[(String, String)]) -> String {
    render("windows/apache-conf.conf.j2", context! { env })
}

/// `cheka-sites.conf`: incluye los vhosts y el comodín para dominios desconocidos.
pub fn windows_apache_site(layout: &Layout) -> String {
    render("windows/apache-site.conf.j2", context! { apache_sites => slash(&layout.apache_sites), tld => TLD })
}

pub fn php_fpm_conf(layout: &Layout, v: &str, user: &str, group: &str) -> String {
    render(
        "linux/php-fpm.conf.j2",
        context! {
            v, user, group,
            run_dir => layout.run_dir.display().to_string(),
            log_dir => layout.log_dir.display().to_string(),
            socket => php::socket(layout, v).display().to_string(),
        },
    )
}

pub fn php_ini(tz: &str) -> String {
    render("linux/php.ini.j2", context! { tz })
}

pub fn php_cli_wrapper(layout: &Layout, v: &str) -> String {
    render(
        "linux/php-cli.sh.j2",
        context! {
            v,
            etc => layout.etc.display().to_string(),
            opt => layout.opt.display().to_string(),
        },
    )
}
