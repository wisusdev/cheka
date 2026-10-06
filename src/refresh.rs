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
