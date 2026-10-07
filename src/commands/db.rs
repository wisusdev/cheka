//! `cheka db …` sobre MariaDB, como el usuario y sin contraseña: por socket (`unix_socket`)
//! en Linux y por named pipe (`named_pipe`, con tu usuario de Windows) en Windows.

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

/// Cliente de MariaDB (`mariadb` o `mariadb-dump`) ya autenticado.
#[cfg(unix)]
fn client(_ctx: &Ctx, program: &str) -> Command {
    Command::new(program)
}

/// Windows: el `[client]` del my.ini de MariaDB (lo deja `cheka install`) dice pipe y tu
/// usuario; se pasa explícito por si el cliente no lo busca ahí.
#[cfg(windows)]
fn client(_ctx: &Ctx, program: &str) -> Command {
    let mut cmd = Command::new(crate::windows_setup::mariadb_bin(program));
    cmd.arg(format!("--defaults-extra-file={}", crate::windows_setup::mariadb_ini().display()));
    cmd
}

/// Ejecuta `mariadb -e SQL`; si falla, mariadb ya mostró el error.
fn sql(ctx: &Ctx, statement: &str) -> Result<()> {
    let st = client(ctx, "mariadb").arg("-e").arg(statement).status().context("No pude ejecutar mariadb")?;
    if !st.success() {
        return Err(Reported.into());
    }
    Ok(())
}

fn create_sql(name: &str) -> String {
    format!("CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci")
}

pub fn create(ctx: &Ctx, name: &str) -> Result<()> {
    valid(name)?;
    sql(ctx, &create_sql(name))?;
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

/// Fecha y hora local para el nombre del volcado (AAAAMMDD-HHMMSS).
fn timestamp() -> Result<String> {
    let out = if cfg!(windows) {
        Command::new("powershell.exe").args(["-NoProfile", "-Command", "Get-Date -Format yyyyMMdd-HHmmss"]).output()?
    } else {
        Command::new("date").arg("+%Y%m%d-%H%M%S").output()?
    };
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn db(ctx: &Ctx, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("help");
    let (db_user, db_pass) = (&ctx.state.db_user, &ctx.state.db_password);
    let rest = args.get(1..).unwrap_or_default();
    match sub {
        "create" => {
            create(ctx, &name_arg(ctx, rest, 0)?)?;
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
            sql(ctx, &format!("DROP DATABASE IF EXISTS `{name}`"))?;
            ui::ok(format!("Base de datos '{name}' borrada"));
        }
        "list" => {
            let out = client(ctx, "mariadb").args(["-N", "-e", "SHOW DATABASES"]).output()?;
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
            sql(ctx, &create_sql(&name))?;
            let mut mariadb = client(ctx, "mariadb");
            mariadb.arg(&name);
            // Windows no trae zcat: se descomprime aquí mismo.
            #[cfg(windows)]
            if file.ends_with(".gz") {
                let mut m = mariadb.stdin(Stdio::piped()).spawn()?;
                let mut stdin = m.stdin.take().unwrap();
                std::io::copy(&mut flate2::read::GzDecoder::new(File::open(file)?), &mut stdin)
                    .context("No pude descomprimir el archivo")?;
                drop(stdin);
                wait_ok(&mut m, "mariadb")?;
                ui::ok(format!("Importado {file} en '{name}'"));
                return Ok(());
            }
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
                None => format!("{name}-{}.sql.gz", timestamp()?),
            };
            // Windows no trae gzip: se comprime aquí mismo.
            #[cfg(windows)]
            {
                let mut dump = client(ctx, "mariadb-dump")
                    .args(["--single-transaction", "--routines", &name])
                    .stdout(Stdio::piped())
                    .spawn()?;
                let mut gz = flate2::write::GzEncoder::new(File::create(&out)?, flate2::Compression::default());
                std::io::copy(&mut dump.stdout.take().unwrap(), &mut gz)?;
                gz.finish()?;
                wait_ok(&mut dump, "mariadb-dump")?;
                ui::ok(format!("Exportado a {out}"));
                return Ok(());
            }
            #[cfg(unix)]
            {
                let mut dump = client(ctx, "mariadb-dump")
                    .args(["--single-transaction", "--routines", &name])
                    .stdout(Stdio::piped())
                    .spawn()?;
                let mut gzip =
                    Command::new("gzip").stdin(dump.stdout.take().unwrap()).stdout(File::create(&out)?).spawn()?;
                wait_ok(&mut dump, "mariadb-dump")?;
                wait_ok(&mut gzip, "gzip")?;
                ui::ok(format!("Exportado a {out}"));
            }
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
