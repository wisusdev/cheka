//! `refresh`: regenera los vhosts desde el estado. Idempotente, valida antes de recargar
//! y revierte si Apache rechaza la configuración (docs/ARQUITECTURA.md §3.1).

use std::collections::BTreeSet;
use std::fs::{self, File};

use anyhow::{Context, Result, bail};
use nix::fcntl::{Flock, FlockArg};

use crate::Ctx;
use crate::detect::detect;
use crate::render::{self, Vhost};
use crate::util::{conf_files, copy_files, dirs_equal, mkdir, write, write_mode};
use crate::{php, sites, ui};

/// Resultado visible para quien pidió el refresh sin sudo (lo lee `request_refresh`).
fn record(ctx: &Ctx, msg: &str) -> Result<()> {
    write_mode(&ctx.layout.run_dir.join("last-refresh"), &format!("{msg}\n"), 0o644)
}

pub fn run(ctx: &Ctx) -> Result<()> {
    let l = &ctx.layout;
    mkdir(&l.apache_sites)?;
    mkdir(&l.run_dir)?;
    let lock = File::create(l.run_dir.join("refresh.lock"))?;
    let _lock = Flock::lock(lock, FlockArg::LockExclusive).map_err(|(_, e)| e).context("flock")?;

    let st = &ctx.state;
    let def = st.default_php();
    let staging = tempfile::tempdir()?;
    let mut versions = BTreeSet::new();
    let mut count = 0;

    for site in sites::list(st) {
        let path_str = site.path.display().to_string();
        if path_str.contains('"') {
            ui::warn(format!("Omitiendo '{path_str}': la ruta contiene comillas"));
            continue;
        }
        let det = detect(&site.path, st.docroot.get(&site.name).map(String::as_str));
        let mut v = st.site_php(&site.name);
        if !php::installed(l, &v) {
            ui::warn(format!(
                "{}: PHP {v} no está instalado, uso PHP {def} (instálalo con: cheka isolate {v})",
                site.name
            ));
            v = def.clone();
        }
        let (cert, key) = (st.cert(&site.name), st.cert_key(&site.name));
        let conf = render::vhost(
            l,
            &Vhost {
                name: &site.name,
                kind: det.kind.as_str(),
                path: &site.path,
                docroot: &det.docroot,
                php: &v,
                tls: st.is_secure(&site.name).then_some((cert.as_path(), key.as_path())),
            },
        );
        write(&staging.path().join(format!("{}.conf", site.name)), &conf)?;
        versions.insert(v);
        count += 1;
    }

    for v in &versions {
        let unit = php::unit(v);
        if !ctx.sys.is_active(&unit) && ctx.sys.enable(&unit, true).is_err() {
            ui::warn(format!("No pude iniciar PHP {v}"));
        }
    }
    write_watch_unit(ctx)?;

    if dirs_equal(staging.path(), &l.apache_sites) {
        let msg = format!("Sin cambios ({count} sitios)");
        record(ctx, &msg)?;
        ui::ok(msg);
        return Ok(());
    }

    let backup = tempfile::tempdir()?;
    copy_files(&l.apache_sites, backup.path())?;
    replace_confs(&l.apache_sites, staging.path())?;
    if !l.is_test() {
        if let Err(err) = ctx.sys.apache_test() {
            replace_confs(&l.apache_sites, backup.path())?;
            record(ctx, "ERROR: La configuración generada no es válida (revisa: sudo apache2ctl -t)")?;
            bail!("La configuración generada no es válida; restauré la anterior:\n{err}");
        }
        if ctx.sys.is_active("apache2") {
            ctx.sys.reload("apache2")?;
        }
    }
    let msg = format!("Apache actualizado ({count} sitios)");
    record(ctx, &msg)?;
    ui::ok(msg);
    Ok(())
}

/// Pide un refresh después de modificar el estado. Como root (o en modo prueba) lo hace
/// directamente; si no, se lo pide al servicio `cheka-watch` sin sudo y espera su
/// resultado; si el servicio no responde, recurre a sudo (docs/ARQUITECTURA.md §3.2).
pub fn request(ctx: &mut Ctx) -> Result<()> {
    ctx.reload_state()?;
    if ctx.is_root() {
        return run(ctx);
    }
    if ctx.sys.is_active("cheka-watch.path") {
        let req = ctx.state.refresh_request();
        let last = ctx.layout.run_dir.join("last-refresh");
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        fs::write(&req, format!("{stamp}\n")).with_context(|| format!("No pude escribir {}", req.display()))?;
        let mtime = |p: &std::path::Path| fs::metadata(p).and_then(|m| m.modified()).ok();
        for _ in 0..60 {
            if let (Some(l), Some(r)) = (mtime(&last), mtime(&req))
                && l > r
            {
                let msg = fs::read_to_string(&last)?.trim_end_matches('\n').to_string();
                if let Some(err) = msg.strip_prefix("ERROR") {
                    bail!("{}", err.strip_prefix(": ").unwrap_or(err));
                }
                ui::ok(msg);
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        ui::warn("El servicio cheka-watch no respondió; lo hago con sudo");
    }
    ctx.run_as_root(&["refresh"])
}

fn replace_confs(dir: &std::path::Path, from: &std::path::Path) -> Result<()> {
    for f in conf_files(dir)? {
        fs::remove_file(f)?;
    }
    for f in conf_files(from)? {
        fs::copy(&f, dir.join(f.file_name().unwrap()))?;
    }
    Ok(())
}

/// Regenera `cheka-watch.path` si cambiaron las carpetas aparcadas.
fn write_watch_unit(ctx: &Ctx) -> Result<()> {
    let f = ctx.layout.units.join("cheka-watch.path");
    let new = render::watch_unit(&ctx.state.paths, &ctx.state.refresh_request());
    if fs::read_to_string(&f).is_ok_and(|old| old == new) {
        return Ok(());
    }
    mkdir(&ctx.layout.units)?;
    write_mode(&f, &new, 0o644)?;
    ctx.sys.daemon_reload()?;
    if ctx.sys.is_enabled("cheka-watch.path") {
        ctx.sys.restart("cheka-watch.path")?;
    }
    Ok(())
}
