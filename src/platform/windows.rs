use std::ffi::OsString;
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

/// Zona horaria del sistema con nombre IANA ("America/El_Salvador"), que es lo que espera
/// PHP. Windows usa nombres propios ("Central America Standard Time"); la conversión la hace
/// el ICU que trae Windows 10/11 (la misma tabla de CLDR que usa todo el mundo).
pub fn iana_timezone() -> Option<String> {
    use windows_sys::Win32::Globalization::{U_ZERO_ERROR, ucal_getTimeZoneIDForWindowsID};
    use windows_sys::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID,
    };
    // SAFETY: estructura de datos simples; Windows la llena.
    let mut info: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetDynamicTimeZoneInformation(&mut info) } == TIME_ZONE_ID_INVALID {
        return None;
    }
    let key = &info.TimeZoneKeyName;
    let len = key.iter().position(|&c| c == 0).unwrap_or(key.len());
    if len == 0 {
        return None;
    }
    // Con la región del usuario ("SV") ICU da la zona de ese país (America/El_Salvador) en
    // vez de la genérica de la zona de Windows (America/Guatemala); sin región, la genérica.
    let region = user_region();
    for region in [region.as_deref(), None] {
        let region_c = region.map(|r| format!("{r}\0"));
        let region_ptr = region_c.as_ref().map_or(std::ptr::null(), |r| r.as_ptr());
        let mut buf = [0u16; 128];
        let mut status = U_ZERO_ERROR;
        // SAFETY: `key` tiene `len` caracteres, `buf` la capacidad indicada y la región
        // (si hay) termina en 0.
        let n = unsafe {
            ucal_getTimeZoneIDForWindowsID(
                key.as_ptr(),
                len as i32,
                region_ptr,
                buf.as_mut_ptr(),
                buf.len() as i32,
                &mut status,
            )
        };
        if status <= U_ZERO_ERROR && n > 0 {
            return String::from_utf16(&buf[..n as usize]).ok();
        }
    }
    None
}

/// Región del usuario en Windows (código ISO de dos letras, p. ej. "SV").
fn user_region() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultGeoName;
    let mut buf = [0u16; 16];
    // SAFETY: búfer con la capacidad indicada.
    let n = unsafe { GetUserDefaultGeoName(buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 1 {
        return None;
    }
    let s = String::from_utf16(&buf[..(n - 1) as usize]).ok()?;
    (s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic())).then_some(s)
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

/// Carpeta de perfiles (`C:\Users`). Se lee del registro porque el servicio corre como
/// SYSTEM, cuyo perfil está en `C:\Windows\system32\config\systemprofile`.
fn profiles_dir() -> Option<PathBuf> {
    let from_registry = Command::new("reg.exe")
        .args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList", "/v", "ProfilesDirectory"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout).lines().find(|l| l.contains("ProfilesDirectory")).and_then(|l| {
                // "    ProfilesDirectory    REG_EXPAND_SZ    %SystemDrive%\Users"
                let value = l.split("REG_EXPAND_SZ").nth(1).or_else(|| l.split("REG_SZ").nth(1))?.trim();
                let drive = env_nonempty("SystemDrive").unwrap_or_else(|| "C:".into());
                Some(PathBuf::from(value.replace("%SystemDrive%", &drive)))
            })
        });
    from_registry
        .filter(|p| p.is_dir())
        .or_else(|| env_nonempty("PUBLIC").and_then(|p| PathBuf::from(p).parent().map(Path::to_path_buf)))
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

/// ¿Está el `sudo` de Windows 11 activado en modo "en línea" (usa la misma consola)?
fn sudo_inline() -> bool {
    if crate::util::which("sudo.exe").is_none() {
        return false;
    }
    Command::new("reg.exe")
        .args(["query", r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Sudo", "/v", "Enabled"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains("Enabled"))
                .any(|l| l.split_whitespace().last() == Some("0x3"))
        })
}

/// Variables que el proceso elevado debe ver igual que nosotros: con UAC arranca con un
/// entorno nuevo (y, si otra cuenta da los permisos, con otro USERNAME).
fn elevated_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ["CHEKA_PREFIX", "CHEKA_CONF", "CHEKA_USER", "TZ"]
        .iter()
        .filter_map(|k| env_nonempty(k).map(|v| (k.to_string(), v)))
        .collect();
    if env_nonempty("CHEKA_USER").is_none()
        && let Ok(user) = current_user_name()
    {
        env.push(("CHEKA_USER".into(), user));
    }
    env
}

/// Línea para `cmd /s /c`: entra a la carpeta actual, define el entorno y ejecuta `exe`
/// mandando toda la salida a `log`.
fn elevated_cmdline(exe: &Path, args: &[OsString], log: &Path) -> Result<String> {
    let quote = |s: &str| -> Result<String> {
        if s.contains('"') {
            bail!("No puedo pasar comillas dobles a un comando elevado: {s}");
        }
        Ok(format!("\"{s}\""))
    };
    let mut line = String::new();
    if let Ok(cwd) = std::env::current_dir() {
        line.push_str(&format!("cd /d {} && ", quote(&cwd.display().to_string())?));
    }
    for (k, v) in elevated_env() {
        line.push_str(&format!("set {} && ", quote(&format!("{k}={v}"))?));
    }
    line.push_str(&quote(&exe.display().to_string())?);
    for a in args {
        line.push(' ');
        line.push_str(&quote(&a.to_string_lossy())?);
    }
    line.push_str(&format!(" > {} 2>&1", quote(&log.display().to_string())?));
    Ok(line)
}

/// `-EncodedCommand` de PowerShell: base64 del script en UTF-16LE. Evita que las comillas
/// del script se pierdan al pasarlo como argumento.
pub fn encode_powershell(script: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Eleva con UAC: `cmd.exe` como administrador (sin ventana) escribe la salida en un archivo
/// temporal, que se muestra aquí mientras corre. Devuelve el código de salida.
fn run_with_uac(exe: &Path, args: &[OsString]) -> Result<i32> {
    run_via_cmd(exe, args, true)
}

/// `elevate = false` solo lo usan las pruebas (mismo camino, sin la ventana de UAC).
fn run_via_cmd(exe: &Path, args: &[OsString], elevate: bool) -> Result<i32> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let log = std::env::temp_dir().join(format!("cheka-elevado-{}.log", std::process::id()));
    std::fs::write(&log, "")?;
    let cmdline = elevated_cmdline(exe, args, &log)?;
    let script = format!(
        "$p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/d /s /c \"{}\"'{} -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        cmdline.replace('\'', "''"),
        if elevate { " -Verb RunAs" } else { "" }
    );
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_powershell(&script)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut file = std::fs::File::open(&log)?;
    let mut out = io::stdout();
    let mut pos = 0u64;
    let mut pump = |file: &mut std::fs::File| {
        let mut chunk = Vec::new();
        if file.seek(SeekFrom::Start(pos)).and_then(|_| file.read_to_end(&mut chunk)).is_ok() && !chunk.is_empty() {
            pos += chunk.len() as u64;
            let _ = out.write_all(&chunk);
            let _ = out.flush();
        }
    };
    let status = loop {
        pump(&mut file);
        if let Some(st) = child.try_wait()? {
            pump(&mut file);
            break st;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    drop(file);
    let _ = std::fs::remove_file(&log);
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    if !status.success() && (err.contains("canceled") || err.contains("cancelad")) {
        bail!("Se canceló la solicitud de permisos de administrador");
    }
    if !status.success() && err.contains("Start-Process") {
        bail!("No pude pedir permisos de administrador: {}", err.trim());
    }
    Ok(status.code().unwrap_or(1))
}

/// Ejecuta `exe args…` como administrador y devuelve su código de salida. Usa el `sudo` de
/// Windows 11 si está en modo "en línea"; si no, la ventana de UAC.
pub fn run_elevated(exe: &Path, args: &[OsString]) -> Result<i32> {
    if sudo_inline() {
        return Ok(Command::new("sudo.exe").arg(exe).args(args).status()?.code().unwrap_or(1));
    }
    run_with_uac(exe, args)
}

/// Como `run_elevated`, pero termina este proceso con el código del elevado.
pub fn exec_elevated(exe: &Path, args: &[OsString]) -> io::Error {
    match run_elevated(exe, args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // ya está explicado en el mensaje; main solo debe salir con 1
            crate::ui::error(format!("{e:#}"));
            std::process::exit(1)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zona_horaria_iana() {
        // Siempre hay una zona configurada; su nombre IANA lleva "/" (o es "UTC"/"Etc/…").
        let tz = iana_timezone().expect("sin zona horaria");
        assert!(tz.contains('/') || tz == "UTC", "{tz}");
    }

    #[test]
    fn base64_de_powershell() {
        // "ab" en UTF-16LE = 61 00 62 00
        assert_eq!(encode_powershell("ab"), "YQBiAA==");
        assert_eq!(encode_powershell("a"), "YQA=");
        assert_eq!(encode_powershell("abc"), "YQBiAGMA");
    }

    #[test]
    fn comando_por_cmd_con_comillas_y_espacios() {
        // Mismo camino que la elevación con UAC, sin pedir permisos.
        let exe = Path::new(r"C:\Windows\System32\where.exe");
        let code = run_via_cmd(exe, &[OsString::from("cmd.exe")], false).unwrap();
        assert_eq!(code, 0);
        let code = run_via_cmd(exe, &[OsString::from("no-existe-'raro' con espacios.exe")], false).unwrap();
        assert_ne!(code, 0);
    }
}
