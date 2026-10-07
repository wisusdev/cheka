//! `cheka new <tipo> <nombre>`: crea un proyecto listo para usar (docs/ARQUITECTURA.md §3.4).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use regex::{NoExpand, Regex};

use super::{db, ensure_php, ensure_wpcli, site::make_cert};
use crate::layout::TLD;
use crate::util::is_executable;
use crate::{Ctx, Reported, php, refresh, sites, ui};

const USAGE: &str = "Uso: cheka new <wordpress|laravel|codeigniter|php> <nombre> [--php=8.2] [--secure] [--multisite[=subdominios]] [--locale=es_MX]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Type {
    Wordpress,
    Laravel,
    Codeigniter,
    Php,
}

struct Plan {
    dir: PathBuf,
    name: String,
    db: String,
    url: String,
    phpbin: PathBuf,
    multisite: Option<&'static str>,
    locale: String,
    db_user: String,
    db_pass: String,
}

/// Lo que se muestra al final.
struct Outcome {
    notes: String,
    has_db: bool,
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let st = cmd.status().with_context(|| format!("No pude ejecutar {what}"))?;
    if !st.success() {
        bail!("{what} falló");
    }
    Ok(())
}

pub fn new(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    if crate::platform::is_root() {
        bail!("Ejecuta 'cheka new' con tu usuario, sin sudo");
    }
    if args.len() < 2 {
        bail!("{USAGE}");
    }
    let (kind, raw) = (&args[0], &args[1]);
    let (mut php_v, mut secure, mut multisite, mut locale) = (None, false, None, "es_MX".to_string());
    for opt in &args[2..] {
        match opt.as_str() {
            o if o.starts_with("--php=") => php_v = Some(php::normalize(&o["--php=".len()..])),
            "--secure" => secure = true,
            "--multisite" | "--multisite=subdirectorios" | "--multisite=subdirectories" => {
                multisite = Some("subdirectorios")
            }
            "--multisite=subdominios" | "--multisite=subdomains" => multisite = Some("subdominios"),
            o if o.starts_with("--locale=") => locale = o["--locale=".len()..].to_string(),
            o => bail!("Opción desconocida: {o}\n{USAGE}"),
        }
    }
    let ty = match kind.as_str() {
        "wp" | "wordpress" => Type::Wordpress,
        "laravel" => Type::Laravel,
        "ci" | "ci4" | "codeigniter" => Type::Codeigniter,
        "php" => Type::Php,
        k => bail!("Tipo desconocido: {k}\n{USAGE}"),
    };
    if multisite.is_some() && ty != Type::Wordpress {
        bail!("--multisite solo aplica a WordPress");
    }

    let name = sites::normalize(raw);
    if name.is_empty() {
        bail!("Nombre inválido: {raw}");
    }
    let base = ctx
        .state
        .paths
        .first()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.id.home.join("Sites"));
    let dir = PathBuf::from(format!("{}/{name}", base.display()));
    if dir.exists() {
        bail!("Ya existe {}", dir.display());
    }
    if sites::list(&ctx.state).iter().any(|s| s.name == name) {
        bail!("Ya existe un sitio llamado '{name}'");
    }
    let db_name = name.replace('-', "_");
    let def = ctx.state.default_php();
    let php_v = match php_v {
        Some(v) => {
            ensure_php(ctx, &v)?;
            v
        }
        None => def.clone(),
    };
    let phpbin = php::cli_bin(&ctx.layout, &php_v);
    if !is_executable(&phpbin) {
        bail!("No encuentro {}", phpbin.display());
    }

    fs::create_dir_all(&dir)?;
    let mut plan = Plan {
        dir: dir.clone(),
        name: name.clone(),
        db: db_name.clone(),
        url: format!("http://{name}.{TLD}"),
        phpbin,
        multisite,
        locale,
        db_user: ctx.state.db_user.clone(),
        db_pass: ctx.state.db_password.clone(),
    };
    // Equivalente al `trap … EXIT` de bash: si algo falla de aquí en adelante, avisar.
    let created = (|| -> Result<Outcome> {
        if php_v != def {
            ctx.state.isolated.insert(name.clone(), php_v.clone());
            ctx.state.save(&ctx.id)?;
        }
        if secure {
            make_cert(ctx, &name)?;
            plan.url = format!("https://{name}.{TLD}");
        }
        match ty {
            Type::Wordpress => wordpress(ctx, &plan),
            Type::Laravel => laravel(ctx, &plan),
            Type::Codeigniter => codeigniter(ctx, &plan),
            Type::Php => plain(&plan),
        }
    })();
    let outcome = match created {
        Ok(o) => o,
        Err(e) => {
            if e.downcast_ref::<Reported>().is_none() {
                ui::error(format!("{e:#}"));
            }
            ui::warn(format!(
                "La creación falló a medias. Revisa {} (bórralo para reintentar) y la base `{db_name}`.",
                dir.display()
            ));
            return Err(Reported.into());
        }
    };
    refresh::request(ctx)?;

    let c = ui::colors();
    println!();
    println!("{}{}{name} listo:{} {}", c.green, c.bold, c.reset, plan.url);
    if let Some(m) = plan.multisite {
        println!("  Multisite por {m}");
    }
    if outcome.has_db {
        println!("  PHP {php_v} · Base de datos '{db_name}' (usuario {} / {})", plan.db_user, plan.db_pass);
    } else {
        println!("  PHP {php_v}");
    }
    if !outcome.notes.is_empty() {
        println!("  {}", outcome.notes);
    }
    Ok(())
}

// ----------------------------------------------------------------- WordPress ----

fn htaccess(mode: Option<&str>) -> String {
    let mut s = String::from(
        "# BEGIN WordPress\n<IfModule mod_rewrite.c>\nRewriteEngine On\n\
         RewriteRule .* - [E=HTTP_AUTHORIZATION:%{HTTP:Authorization}]\nRewriteBase /\n\
         RewriteRule ^index\\.php$ - [L]\n",
    );
    s.push_str(match mode {
        Some("subdirectorios") => {
            "RewriteRule ^([_0-9a-zA-Z-]+/)?wp-admin$ $1wp-admin/ [R=301,L]\n\
             RewriteCond %{REQUEST_FILENAME} -f [OR]\nRewriteCond %{REQUEST_FILENAME} -d\nRewriteRule ^ - [L]\n\
             RewriteRule ^([_0-9a-zA-Z-]+/)?(wp-(content|admin|includes).*) $2 [L]\n\
             RewriteRule ^([_0-9a-zA-Z-]+/)?(.*\\.php)$ $2 [L]\nRewriteRule . index.php [L]\n"
        }
        Some(_) => {
            "RewriteRule ^wp-admin$ wp-admin/ [R=301,L]\n\
             RewriteCond %{REQUEST_FILENAME} -f [OR]\nRewriteCond %{REQUEST_FILENAME} -d\nRewriteRule ^ - [L]\n\
             RewriteRule ^(wp-(content|admin|includes).*) $1 [L]\nRewriteRule ^(.*\\.php)$ $1 [L]\n\
             RewriteRule . index.php [L]\n"
        }
        None => "RewriteCond %{REQUEST_FILENAME} !-f\nRewriteCond %{REQUEST_FILENAME} !-d\nRewriteRule . /index.php [L]\n",
    });
    s.push_str("</IfModule>\n# END WordPress\n");
    s
}

fn wordpress(ctx: &Ctx, p: &Plan) -> Result<Outcome> {
    ensure_wpcli(ctx)?;
    let wp = |args: &[&str]| {
        let mut c = Command::new(&p.phpbin);
        c.arg(&ctx.id.wpcli).arg(format!("--path={}", p.dir.display())).arg("--quiet").args(args);
        c
    };
    ui::info(format!("Descargando WordPress ({})…", p.locale));
    run(&mut wp(&["core", "download", &format!("--locale={}", p.locale)]), "wp core download")?;
    db::create(ctx, &p.db)?;

    let mut cfg = wp(&[
        "config",
        "create",
        &format!("--dbname={}", p.db),
        &format!("--dbuser={}", p.db_user),
        &format!("--dbpass={}", p.db_pass),
        "--dbhost=localhost",
        &format!("--locale={}", p.locale),
        "--extra-php",
    ])
    .stdin(Stdio::piped())
    .spawn()?;
    cfg.stdin.take().unwrap().write_all(
        b"define( 'WP_DEBUG', true );\ndefine( 'WP_DEBUG_LOG', true );\ndefine( 'WP_ENVIRONMENT_TYPE', 'local' );\n",
    )?;
    if !cfg.wait()?.success() {
        bail!("wp config create falló");
    }

    let mut install: Vec<String> = match p.multisite {
        None => vec!["core".into(), "install".into()],
        Some(m) => {
            let mut v = vec!["core".into(), "multisite-install".into()];
            if m == "subdominios" {
                v.push("--subdomains".into());
            }
            v
        }
    };
    install.extend([
        format!("--url={}", p.url),
        format!("--title={}", p.name),
        "--admin_user=admin".into(),
        "--admin_password=admin".into(),
        format!("--admin_email=admin@{}.{TLD}", p.name),
        "--skip-email".into(),
    ]);
    ui::info("Instalando WordPress…");
    let install: Vec<&str> = install.iter().map(String::as_str).collect();
    run(&mut wp(&install), "wp core install")?;
    run(&mut wp(&["rewrite", "structure", "/%postname%/"]), "wp rewrite structure")?;
    fs::write(p.dir.join(".htaccess"), htaccess(p.multisite))?;
    Ok(Outcome { notes: format!("Admin: {}/wp-admin  (usuario: admin, contraseña: admin)", p.url), has_db: true })
}

// ------------------------------------------------------ Laravel / CodeIgniter ----

fn composer() -> Result<PathBuf> {
    super::composer_script().ok_or_else(|| anyhow!("Composer no está instalado"))
}

fn create_project(p: &Plan, package: &str) -> Result<()> {
    run(
        Command::new(&p.phpbin)
            .arg(composer()?)
            .args(["create-project", "--no-interaction", "--prefer-dist", package])
            .arg(&p.dir),
        "composer create-project",
    )
}

/// Cambia (o agrega) una línea de configuración, aunque esté comentada con `#`.
fn set_line(file: &Path, pattern: &str, line: &str) -> Result<()> {
    let text = fs::read_to_string(file)?;
    let re = Regex::new(&format!("(?m)^#? *{pattern}.*$"))?;
    let new = if re.is_match(&text) {
        re.replace_all(&text, NoExpand(line)).into_owned()
    } else {
        format!("{text}{line}\n")
    };
    fs::write(file, new)?;
    Ok(())
}

/// `.env` de Laravel: `CLAVE=valor`.
fn set_env(file: &Path, key: &str, val: &str) -> Result<()> {
    set_line(file, &format!("{}=", regex::escape(key)), &format!("{key}={val}"))
}

/// `.env` de CodeIgniter 4: `clave = valor`.
fn set_ci_env(file: &Path, key: &str, val: &str) -> Result<()> {
    set_line(file, &format!("{} *=", regex::escape(key)), &format!("{key} = {val}"))
}

fn laravel(ctx: &Ctx, p: &Plan) -> Result<Outcome> {
    ui::info("Creando proyecto Laravel con Composer…");
    create_project(p, "laravel/laravel")?;
    db::create(ctx, &p.db)?;
    let env = p.dir.join(".env");
    for (k, v) in [
        ("APP_URL", p.url.as_str()),
        ("DB_CONNECTION", "mysql"),
        ("DB_HOST", "127.0.0.1"),
        ("DB_PORT", "3306"),
        ("DB_DATABASE", &p.db),
        ("DB_USERNAME", &p.db_user),
        ("DB_PASSWORD", &p.db_pass),
    ] {
        set_env(&env, k, v)?;
    }
    let _ = fs::remove_file(p.dir.join("database/database.sqlite"));
    ui::info("Ejecutando migraciones en MariaDB…");
    run(
        Command::new(&p.phpbin).current_dir(&p.dir).args(["artisan", "migrate", "--force", "--no-interaction"]),
        "artisan migrate",
    )?;
    Ok(Outcome { notes: format!("Proyecto en {} (.env apuntando a MariaDB '{}')", p.dir.display(), p.db), has_db: true })
}

fn codeigniter(ctx: &Ctx, p: &Plan) -> Result<Outcome> {
    ui::info("Creando proyecto CodeIgniter 4 con Composer…");
    create_project(p, "codeigniter4/appstarter")?;
    db::create(ctx, &p.db)?;
    let env = p.dir.join(".env");
    fs::copy(p.dir.join("env"), &env)?;
    let base_url = format!("'{}/'", p.url);
    for (k, v) in [
        ("CI_ENVIRONMENT", "development"),
        ("app.baseURL", base_url.as_str()),
        ("database.default.hostname", "localhost"),
        ("database.default.database", &p.db),
        ("database.default.username", &p.db_user),
        ("database.default.password", &p.db_pass),
        ("database.default.DBDriver", "MySQLi"),
        ("database.default.port", "3306"),
    ] {
        set_ci_env(&env, k, v)?;
    }
    Ok(Outcome {
        notes: format!("Proyecto en {} (.env en modo development, base '{}')", p.dir.display(), p.db),
        has_db: true,
    })
}

fn plain(p: &Plan) -> Result<Outcome> {
    fs::write(
        p.dir.join("index.php"),
        "<?php\necho '<h1>' . htmlspecialchars($_SERVER['HTTP_HOST']) . '</h1>';\n\
         echo '<p>PHP ' . PHP_VERSION . ' — edita ' . __FILE__ . '</p>';\n",
    )?;
    Ok(Outcome {
        notes: format!("Proyecto en {} (sin base de datos; créala con: cheka db create)", p.dir.display()),
        has_db: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_descomenta_y_agrega() {
        let t = tempfile::NamedTempFile::new().unwrap();
        fs::write(t.path(), "DB_CONNECTION=sqlite\n# DB_HOST=127.0.0.1\n#DB_PORT=3306\n").unwrap();
        set_env(t.path(), "DB_CONNECTION", "mysql").unwrap();
        set_env(t.path(), "DB_HOST", "127.0.0.1").unwrap();
        set_env(t.path(), "DB_PORT", "3306").unwrap();
        set_env(t.path(), "APP_URL", "http://x.test").unwrap();
        assert_eq!(
            fs::read_to_string(t.path()).unwrap(),
            "DB_CONNECTION=mysql\nDB_HOST=127.0.0.1\nDB_PORT=3306\nAPP_URL=http://x.test\n"
        );
    }

    #[test]
    fn ci_env_respeta_puntos() {
        let t = tempfile::NamedTempFile::new().unwrap();
        fs::write(t.path(), "# app.baseURL = ''\n# appXbaseURL = 'no'\n").unwrap();
        set_ci_env(t.path(), "app.baseURL", "'http://x.test/'").unwrap();
        assert_eq!(fs::read_to_string(t.path()).unwrap(), "app.baseURL = 'http://x.test/'\n# appXbaseURL = 'no'\n");
    }

    #[test]
    fn htaccess_por_modo() {
        assert!(htaccess(None).contains("RewriteRule . /index.php [L]"));
        assert!(htaccess(Some("subdirectorios")).contains("$1wp-admin/"));
        assert!(htaccess(Some("subdominios")).contains("RewriteRule ^(.*\\.php)$ $1 [L]"));
    }
}
