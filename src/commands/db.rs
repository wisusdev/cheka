//! `cheka db …` sobre MariaDB (como el usuario, por socket).

use std::fs::File;
use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use regex::Regex;

use crate::{Ctx, Reported, sites, ui};

fn valid(name: &str) -> Result<()> {
    if !Regex::new(r"^[A-Za-z0-9_]+$").unwrap().is_match(name) {
        bail!("Nombre de base de datos inválido: {name}");
    }
    Ok(())
}

/// Ejecuta `mariadb -e SQL`; si falla, mariadb ya mostró el error.
fn sql(statement: &str) -> Result<()> {
    let st = Command::new("mariadb").arg("-e").arg(statement).status().context("No pude ejecutar mariadb")?;
    if !st.success() {
        return Err(Reported.into());
    }
    Ok(())
}

fn create_sql(name: &str) -> String {
    format!("CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci")
}

pub fn create(name: &str) -> Result<()> {
    valid(name)?;
    sql(&create_sql(name))?;
    ui::ok(format!("Base de datos '{name}' lista"));
    Ok(())
}

/// Nombre de base por defecto: el del sitio actual con `-` → `_`.
fn default_name(ctx: &Ctx) -> Result<String> {
    let site = sites::resolve(&sites::list(&ctx.state), None, &ctx.cwd()?)?;
    Ok(site.name.replace('-', "_"))
}

fn name_arg(ctx: &Ctx, args: &[String], i: usize) -> Result<String> {
    match args.get(i).filter(|s| !s.is_empty()) {
        Some(n) => Ok(n.clone()),
        None => default_name(ctx),
    }
}

fn wait_ok(child: &mut std::process::Child, what: &str) -> Result<()> {
    if !child.wait()?.success() {
        bail!("{what} falló");
    }
    Ok(())
}

pub fn db(ctx: &Ctx, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("help");
    let (db_user, db_pass) = (&ctx.state.db_user, &ctx.state.db_password);
    let rest = args.get(1..).unwrap_or_default();
    match sub {
        "create" => {
            create(&name_arg(ctx, rest, 0)?)?;
            println!("  host: localhost (o 127.0.0.1)   usuario: {db_user}   contraseña: {db_pass}");
        }
        "drop" => {
            let name = name_arg(ctx, rest, 0)?;
            valid(&name)?;
            // Como `read -p`: el aviso solo se muestra si la entrada es una terminal.
            if std::io::stdin().is_terminal() {
                eprint!("¿Borrar la base de datos '{name}'? Escribe su nombre para confirmar: ");
                std::io::stderr().flush()?;
            }
            let mut confirm = String::new();
            std::io::stdin().lock().read_line(&mut confirm)?;
            if confirm.trim_end_matches(['\n', '\r']) != name {
                bail!("Cancelado");
            }
            sql(&format!("DROP DATABASE IF EXISTS `{name}`"))?;
            ui::ok(format!("Base de datos '{name}' borrada"));
        }
        "list" => {
            let out = Command::new("mariadb").args(["-N", "-e", "SHOW DATABASES"]).output()?;
            if !out.status.success() {
                std::io::stderr().write_all(&out.stderr)?;
                return Err(Reported.into());
            }
            for db in String::from_utf8_lossy(&out.stdout).lines() {
                if !["information_schema", "performance_schema", "mysql", "sys"].contains(&db) {
                    println!("{db}");
                }
            }
        }
        "import" => {
            let Some(file) = rest.first() else { bail!("Uso: cheka db import <archivo.sql[.gz]> [base]") };
            let name = name_arg(ctx, rest, 1)?;
            valid(&name)?;
            if !std::path::Path::new(file).is_file() {
                bail!("No existe {file}");
            }
            sql(&create_sql(&name))?;
            let mut mariadb = Command::new("mariadb");
            mariadb.arg(&name);
            if file.ends_with(".gz") {
                let mut zcat = Command::new("zcat").arg(file).stdout(Stdio::piped()).spawn()?;
                let mut m = mariadb.stdin(zcat.stdout.take().unwrap()).spawn()?;
                wait_ok(&mut zcat, "zcat")?;
                wait_ok(&mut m, "mariadb")?;
            } else {
                let mut m = mariadb.stdin(File::open(file)?).spawn()?;
                wait_ok(&mut m, "mariadb")?;
            }
            ui::ok(format!("Importado {file} en '{name}'"));
        }
        "export" => {
            let name = name_arg(ctx, rest, 0)?;
            valid(&name)?;
            let out = match rest.get(1) {
                Some(o) => o.clone(),
                None => {
                    let d = Command::new("date").arg("+%Y%m%d-%H%M%S").output()?;
                    format!("{name}-{}.sql.gz", String::from_utf8_lossy(&d.stdout).trim())
                }
            };
            let mut dump = Command::new("mariadb-dump")
                .args(["--single-transaction", "--routines", &name])
                .stdout(Stdio::piped())
                .spawn()?;
            let mut gzip =
                Command::new("gzip").stdin(dump.stdout.take().unwrap()).stdout(File::create(&out)?).spawn()?;
            wait_ok(&mut dump, "mariadb-dump")?;
            wait_ok(&mut gzip, "gzip")?;
            ui::ok(format!("Exportado a {out}"));
        }
        _ => print!(
            "Uso: cheka db <create|drop|list|import|export> [...]
  create [base]                   Crea la base (por defecto: nombre del sitio actual)
  drop [base]                     Borra la base (pide confirmación)
  list                            Lista las bases
  import <archivo.sql[.gz]> [base]
  export [base] [archivo.sql.gz]
Credenciales para tus proyectos: usuario '{db_user}', contraseña '{db_pass}', host localhost.
"
        ),
    }
    Ok(())
}
