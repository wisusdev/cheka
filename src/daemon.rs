//! `cheka daemon`: corre como servicio (root). Reemplaza a `cheka-watch.path`,
//! `cheka-refresh.timer` y al archivo `.refresh-request` de la versión en bash:
//!
//! - atiende a la CLI por un socket Unix (solo root y el dueño de los proyectos),
//! - vigila las carpetas aparcadas y el estado del usuario,
//! - hace un refresh periódico (corrige detecciones hechas a media clonación).

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use notify_debouncer_mini::{DebounceEventResult, new_debouncer, notify::RecursiveMode};

use crate::ipc::{self, Request, Response};
use crate::{Ctx, refresh, ui};

const PERIOD: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    Cli,
    Files,
    Periodic,
}

/// Un refresh a la vez, siempre con el estado recién leído.
fn refresh_now(lock: &Mutex<()>, why: Why) -> Response {
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    // El refresh periódico es silencioso para no llenar el journal cada minuto.
    ui::set_quiet(true);
    ui::set_silent_warnings(why == Why::Periodic);
    let result = Ctx::load().and_then(|ctx| refresh::run(&ctx));
    ui::set_silent_warnings(false);
    match result {
        Ok(msg) => {
            if why != Why::Periodic || !msg.starts_with("Sin cambios") {
                println!("refresh ({}): {msg}", match why {
                    Why::Cli => "cli",
                    Why::Files => "carpetas",
                    Why::Periodic => "periódico",
                });
            }
            Response::ok(msg)
        }
        Err(e) => {
            eprintln!("refresh falló: {e:#}");
            Response::error(format!("{e:#}"))
        }
    }
}

fn handle(stream: UnixStream, allowed_uid: u32, lock: &Mutex<()>) {
    let uid = getsockopt(&stream, PeerCredentials).map(|c| c.uid()).unwrap_or(u32::MAX);
    let resp = if uid != 0 && uid != allowed_uid {
        Response::error("No autorizado")
    } else {
        match ipc::read_request(&stream) {
            Ok(Request::Ping) => Response::ok("pong"),
            Ok(Request::Refresh) => refresh_now(lock, Why::Cli),
            Err(e) => Response::error(format!("Petición inválida: {e}")),
        }
    };
    let _ = ipc::write_response(&stream, &resp);
}

/// Carpetas a vigilar: las aparcadas que existen y el estado del usuario.
fn watch_set() -> BTreeSet<PathBuf> {
    let Ok(ctx) = Ctx::load() else { return BTreeSet::new() };
    let mut set: BTreeSet<PathBuf> =
        ctx.state.paths.iter().filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).collect();
    if ctx.state.conf.is_dir() {
        set.insert(ctx.state.conf.clone());
    }
    set
}

pub fn run(ctx: &Ctx) -> Result<()> {
    if !ctx.is_root() {
        bail!("El daemon corre como root; lo arranca systemd (cheka.service)");
    }
    let socket = ctx.layout.socket();
    fs::create_dir_all(&ctx.layout.run_dir)?;
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("No pude abrir {}", socket.display()))?;
    // Cualquiera puede conectar; la autorización se hace con SO_PEERCRED en `handle`.
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
    let allowed_uid = ctx.id.uid;
    let lock = Arc::new(Mutex::new(()));
    println!("cheka daemon escuchando en {} (usuario: {})", socket.display(), ctx.id.user);

    // Estado inicial al arrancar.
    refresh_now(&lock, Why::Files);

    // API para la CLI
    {
        let lock = Arc::clone(&lock);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let lock = Arc::clone(&lock);
                thread::spawn(move || handle(stream, allowed_uid, &lock));
            }
        });
    }

    // Vigilancia de carpetas
    let (tx, rx) = mpsc::channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(DEBOUNCE, tx)?;
    let mut watching: BTreeSet<PathBuf> = BTreeSet::new();
    let mut sync_watches = |watching: &mut BTreeSet<PathBuf>| {
        let wanted = watch_set();
        for p in watching.difference(&wanted) {
            let _ = debouncer.watcher().unwatch(p);
        }
        for p in wanted.difference(watching) {
            if let Err(e) = debouncer.watcher().watch(p, RecursiveMode::NonRecursive) {
                eprintln!("No pude vigilar {}: {e}", p.display());
            }
        }
        *watching = wanted;
    };
    sync_watches(&mut watching);

    loop {
        match rx.recv_timeout(PERIOD) {
            Ok(_) => {
                refresh_now(&lock, Why::Files);
                sync_watches(&mut watching); // por si cambiaron las carpetas aparcadas
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                refresh_now(&lock, Why::Periodic);
                sync_watches(&mut watching);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("El vigilante de archivos se detuvo"),
        }
    }
}
