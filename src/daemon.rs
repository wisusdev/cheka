//! `cheka daemon`: corre como servicio (root; LocalSystem en Windows). Reemplaza a
//! `cheka-watch.path`, `cheka-refresh.timer` y al archivo `.refresh-request` de la versión
//! en bash:
//!
//! - atiende a la CLI por un socket Unix o una named pipe (solo root/administradores y el
//!   dueño de los proyectos),
//! - vigila las carpetas aparcadas y el estado del usuario,
//! - hace un refresh periódico (corrige detecciones hechas a media clonación).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Result, bail};
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

/// Atiende una petición ya autorizada.
fn answer(conn: impl std::io::Read, lock: &Mutex<()>) -> Response {
    match ipc::read_request(conn) {
        Ok(Request::Ping) => Response::ok("pong"),
        Ok(Request::Refresh) => refresh_now(lock, Why::Cli),
        Err(e) => Response::error(format!("Petición inválida: {e}")),
    }
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

// ------------------------------------------------------------- socket Unix ----

#[cfg(unix)]
mod transport {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use anyhow::{Context, Result};
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

    use crate::Ctx;
    use crate::ipc::{self, Response};

    fn handle(stream: UnixStream, allowed_uid: u32, lock: &Mutex<()>) {
        let uid = getsockopt(&stream, PeerCredentials).map(|c| c.uid()).unwrap_or(u32::MAX);
        let resp = if uid != 0 && uid != allowed_uid {
            Response::error("No autorizado")
        } else {
            super::answer(&stream, lock)
        };
        let _ = ipc::write_response(&stream, &resp);
    }

    /// Abre el socket y atiende a la CLI en otro hilo.
    pub fn serve(ctx: &Ctx, lock: &Arc<Mutex<()>>) -> Result<String> {
        let socket = ctx.layout.socket();
        fs::create_dir_all(&ctx.layout.run_dir)?;
        let _ = fs::remove_file(&socket);
        let listener =
            UnixListener::bind(&socket).with_context(|| format!("No pude abrir {}", socket.display()))?;
        // Cualquiera puede conectar; la autorización se hace con SO_PEERCRED en `handle`.
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
        let allowed_uid = ctx.id.uid;
        let lock = Arc::clone(lock);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let lock = Arc::clone(&lock);
                thread::spawn(move || handle(stream, allowed_uid, &lock));
            }
        });
        Ok(socket.display().to_string())
    }
}

// --------------------------------------------------------- named pipe (Windows) ----

#[cfg(windows)]
mod transport {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use anyhow::{Context, Result};
    use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    use crate::Ctx;
    use crate::ipc;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Descriptor de seguridad a partir de SDDL (se conserva mientras viva el daemon).
    fn security_descriptor(sddl: &str) -> io::Result<usize> {
        let mut psd: *mut c_void = std::ptr::null_mut();
        // SAFETY: cadena terminada en 0; `psd` recibe memoria que Windows reserva.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide(sddl).as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(psd as usize)
    }

    /// Crea una instancia de la pipe (aún sin cliente).
    fn create(name: &[u16], psd: usize, first: bool) -> io::Result<File> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd as *mut c_void,
            bInheritHandle: 0,
        };
        // La primera instancia falla si otro proceso ya tiene la pipe (otro daemon, o
        // alguien intentando suplantarlo).
        let open_mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        // SAFETY: `name` termina en 0 y `sa` vive durante la llamada.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                &sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `h` es un handle válido y propio; `File` lo cierra al soltarse.
        Ok(unsafe { File::from_raw_handle(h as RawHandle) })
    }

    /// Espera a que un cliente se conecte a la instancia.
    fn connect(pipe: &File) -> io::Result<()> {
        // SAFETY: handle válido, sin E/S superpuesta.
        if unsafe { ConnectNamedPipe(pipe.as_raw_handle() as _, std::ptr::null_mut()) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
                return Err(e);
            }
        }
        Ok(())
    }

    /// SID del dueño de los proyectos (lo guarda `install`); sin él, los usuarios
    /// interactivos de la máquina.
    fn owner_sid(ctx: &Ctx) -> String {
        std::fs::read_to_string(ctx.layout.etc.join("user-sid"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("S-1-"))
            .unwrap_or_else(|| "IU".to_string())
    }

    /// Abre la pipe y atiende a la CLI en otro hilo. Pueden conectarse SYSTEM, los
    /// administradores y el dueño de los proyectos (equivale al SO_PEERCRED de Linux).
    pub fn serve(ctx: &Ctx, lock: &Arc<Mutex<()>>) -> Result<String> {
        let path = ctx.layout.socket().display().to_string();
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{})", owner_sid(ctx));
        let psd = security_descriptor(&sddl).context("Descriptor de seguridad de la pipe")?;
        let name = wide(&path);
        // La primera instancia se crea aquí: si otro proceso ya tiene la pipe, falla ya.
        let mut next = create(&name, psd, true)
            .with_context(|| format!("No pude abrir {path} (¿ya corre otro daemon de cheka?)"))?;
        let lock = Arc::clone(lock);
        thread::spawn(move || {
            loop {
                if connect(&next).is_ok() {
                    let conn = next;
                    let lock = Arc::clone(&lock);
                    thread::spawn(move || {
                        let resp = super::answer(&conn, &lock);
                        let _ = ipc::write_response(&conn, &resp);
                        let _ = conn.sync_all(); // FlushFileBuffers: que el cliente lea todo
                    });
                }
                next = loop {
                    match create(&name, psd, false) {
                        Ok(f) => break f,
                        Err(e) => {
                            eprintln!("No pude crear otra instancia de la pipe: {e}");
                            thread::sleep(std::time::Duration::from_secs(1));
                        }
                    }
                };
            }
        });
        Ok(path)
    }
}

// -------------------------------------------------------- servicio de Windows ----

#[cfg(windows)]
mod service {
    use std::ffi::OsString;
    use std::os::windows::io::IntoRawHandle;
    use std::sync::OnceLock;
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
    use windows_service::{define_windows_service, service_dispatcher};
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    use crate::Ctx;
    use crate::layout::Layout;

    pub const NAME: &str = "cheka";
    static HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    fn status(state: ServiceState, code: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }
    }

    /// Un servicio no tiene consola: la salida va a `logs\cheka-daemon.log` (se reinicia
    /// al pasar de 5 MB).
    fn redirect_output(layout: &Layout) {
        let log = layout.log_dir.join("cheka-daemon.log");
        let _ = std::fs::create_dir_all(&layout.log_dir);
        if std::fs::metadata(&log).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::remove_file(&log);
        }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let h = file.into_raw_handle();
            // SAFETY: handle válido que no se vuelve a cerrar (vive lo que el proceso).
            unsafe {
                SetStdHandle(STD_OUTPUT_HANDLE, h as _);
                SetStdHandle(STD_ERROR_HANDLE, h as _);
            }
        }
    }

    fn service_main(_args: Vec<OsString>) {
        let handler = |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(h) = HANDLE.get() {
                    let _ = h.set_service_status(status(ServiceState::Stopped, 0));
                }
                std::process::exit(0);
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(h) = service_control_handler::register(NAME, handler) else { return };
        let _ = HANDLE.set(h);
        let _ = h.set_service_status(status(ServiceState::Running, 0));
        let result = Ctx::load().and_then(|ctx| {
            redirect_output(&ctx.layout);
            super::run_daemon(&ctx)
        });
        if let Err(e) = &result {
            eprintln!("cheka daemon: {e:#}");
        }
        let _ = h.set_service_status(status(ServiceState::Stopped, u32::from(result.is_err())));
    }

    /// Entra al despachador de servicios. Devuelve `Ok(false)` si no lo lanzó el
    /// administrador de servicios (p. ej. `cheka daemon` desde una terminal).
    pub fn dispatch() -> windows_service::Result<bool> {
        const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: i32 = 1063;
        match service_dispatcher::start(NAME, ffi_service_main) {
            Ok(()) => Ok(true),
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(windows)]
pub use service::NAME as SERVICE_NAME;

/// `cheka daemon`. En Windows, si lo arranca el administrador de servicios, corre como
/// servicio; desde una terminal corre en primer plano (útil para depurar).
pub fn run(ctx: &Ctx) -> Result<()> {
    #[cfg(windows)]
    if service::dispatch()? {
        return Ok(());
    }
    run_daemon(ctx)
}

fn run_daemon(ctx: &Ctx) -> Result<()> {
    if !ctx.is_root() {
        #[cfg(unix)]
        bail!("El daemon corre como root; lo arranca systemd (cheka.service)");
        #[cfg(windows)]
        bail!("El daemon corre como administrador; lo arranca el servicio 'cheka'");
    }
    let lock = Arc::new(Mutex::new(()));
    let endpoint = transport::serve(ctx, &lock)?;
    println!("cheka daemon escuchando en {endpoint} (usuario: {})", ctx.id.user);

    // Estado inicial al arrancar.
    refresh_now(&lock, Why::Files);

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
