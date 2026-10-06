use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, anyhow, bail};

use super::UserInfo;

#[link(name = "shell32")]
unsafe extern "system" {
    fn IsUserAnAdmin() -> i32;
}

/// El proceso corre elevado (consola de administrador, `sudo` de Windows o un servicio).
pub fn is_root() -> bool {
    // SAFETY: función de Win32 sin argumentos ni estado compartido.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Windows no puede reemplazar el proceso: ejecuta `cmd` con la misma consola, espera y
/// termina con su código de salida. Solo regresa si no se pudo ejecutar.
pub fn exec(cmd: &mut Command) -> io::Error {
    match cmd.status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(e) => e,
    }
}

const EXECUTABLE_EXTS: [&str; 4] = ["exe", "bat", "cmd", "com"];

/// Un archivo con extensión de ejecutable.
pub fn is_executable(p: &Path) -> bool {
    p.is_file()
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXECUTABLE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// `fs::canonicalize` sin el prefijo `\\?\` que agrega Windows: esas rutas no las entienden
/// Apache, PHP ni el usuario que lee `cheka.toml`. Las rutas de red quedan como `\\servidor\…`.
pub fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    let s = c.to_string_lossy();
    Ok(if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(local) = s.strip_prefix(r"\\?\") {
        PathBuf::from(local)
    } else {
        c
    })
}

/// Los permisos Unix no existen en Windows.
pub fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

/// Los archivos ya quedan a nombre de quien los crea.
pub fn chown(_: &Path, _: u32, _: u32, _: bool) -> io::Result<()> {
    Ok(())
}

/// Requiere modo desarrollador o privilegios de administrador.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

pub fn current_user_name() -> Result<String> {
    env_nonempty("USERNAME").ok_or_else(|| anyhow!("No encuentro el usuario actual (USERNAME)"))
}

/// Carpeta de perfiles (`C:\Users`), a partir del perfil actual o de `PUBLIC`.
fn profiles_dir() -> Option<PathBuf> {
    env_nonempty("USERPROFILE")
        .or_else(|| env_nonempty("PUBLIC"))
        .and_then(|p| PathBuf::from(p).parent().map(Path::to_path_buf))
}

pub fn user_info(name: &str) -> Result<UserInfo> {
    let current = current_user_name().ok();
    let home = match env_nonempty("USERPROFILE") {
        Some(h) if current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)) => PathBuf::from(h),
        _ => profiles_dir().map(|d| d.join(name)).ok_or_else(|| anyhow!("No encuentro el perfil de '{name}'"))?,
    };
    if !home.is_dir() {
        bail!("No existe el usuario '{name}' (no encuentro {})", home.display());
    }
    Ok(UserInfo { name: name.to_string(), uid: 0, gid: 0, group: String::new(), home })
}

/// Configuración del usuario: `%APPDATA%\cheka` (`<perfil>\AppData\Roaming\cheka`).
pub fn user_conf_dir(home: &Path) -> PathBuf {
    home.join("AppData").join("Roaming").join("cheka")
}

/// Comando que vuelve a ejecutar `exe args…` elevado. Usa el `sudo` de Windows 11 (modo
/// "en línea", activable en Configuración → Sistema → Para programadores), que conserva la
/// consola; sin él, hay que abrir una terminal como administrador.
pub fn elevate_command(exe: &Path) -> Result<Command> {
    if crate::util::which("sudo.exe").is_none() {
        bail!(
            "Este comando necesita permisos de administrador. Ábrelo en una terminal como \
             administrador, o activa 'sudo' en Configuración → Sistema → Para programadores"
        );
    }
    let mut cmd = Command::new("sudo.exe");
    cmd.arg(exe);
    Ok(cmd)
}

/// Sigue los logs como `tail -n 50 -F`: muestra las últimas 50 líneas de cada uno y
/// luego lo que se agregue, aunque el archivo aún no exista o se trunque. No regresa.
pub fn follow_logs(files: &[PathBuf]) -> io::Error {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut offsets: Vec<Option<u64>> = vec![None; files.len()];
    let mut last_shown: Option<usize> = None;
    let mut out = io::stdout();
    loop {
        for (i, f) in files.iter().enumerate() {
            let Ok(mut file) = std::fs::File::open(f) else { continue };
            let Ok(len) = file.metadata().map(|m| m.len()) else { continue };
            let start = match offsets[i] {
                Some(off) if off <= len => off,
                Some(_) => 0, // truncado
                None => {
                    // primera vez: las últimas 50 líneas
                    let mut text = String::new();
                    let _ = file.read_to_string(&mut text);
                    let lines: Vec<&str> = text.lines().collect();
                    let tail = lines[lines.len().saturating_sub(50)..].join("\n");
                    let _ = writeln!(out, "==> {} <==", f.display());
                    if !tail.is_empty() {
                        let _ = writeln!(out, "{tail}");
                    }
                    last_shown = Some(i);
                    offsets[i] = Some(len);
                    continue;
                }
            };
            if start == len {
                continue;
            }
            let mut chunk = Vec::new();
            if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_to_end(&mut chunk)).is_err() {
                continue;
            }
            if last_shown != Some(i) {
                let _ = writeln!(out, "\n==> {} <==", f.display());
                last_shown = Some(i);
            }
            let _ = out.write_all(&chunk);
            let _ = out.flush();
            offsets[i] = Some(start + chunk.len() as u64);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Abre una URL con el navegador predeterminado.
pub fn open_url_command(url: &str) -> Command {
    let mut cmd = Command::new("rundll32.exe");
    cmd.args(["url.dll,FileProtocolHandler", url]);
    cmd
}
