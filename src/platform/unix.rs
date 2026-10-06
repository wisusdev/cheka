use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use nix::unistd::{AccessFlags, Group, User, access, geteuid};

use super::UserInfo;

/// Root de verdad (sin contar el modo prueba).
pub fn is_root() -> bool {
    geteuid().is_root()
}

/// Reemplaza el proceso actual por `cmd`. Solo regresa si no se pudo ejecutar.
pub fn exec(cmd: &mut Command) -> io::Error {
    cmd.exec()
}

/// Equivale a `[[ -x ruta ]]`.
pub fn is_executable(p: &Path) -> bool {
    access(p, AccessFlags::X_OK).is_ok()
}

pub fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

pub fn canonicalize(p: &Path) -> io::Result<std::path::PathBuf> {
    std::fs::canonicalize(p)
}

/// Cambia el dueño (`link`: del enlace, no de su destino).
pub fn chown(p: &Path, uid: u32, gid: u32, link: bool) -> io::Result<()> {
    let (u, g) = (Some(uid), Some(gid));
    if link { std::os::unix::fs::lchown(p, u, g) } else { std::os::unix::fs::chown(p, u, g) }
}

pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

pub fn current_user_name() -> Result<String> {
    Ok(User::from_uid(geteuid())?.ok_or_else(|| anyhow!("No encuentro el usuario actual"))?.name)
}

pub fn user_info(name: &str) -> Result<UserInfo> {
    let user = User::from_name(name)?.ok_or_else(|| anyhow!("No existe el usuario '{name}'"))?;
    let group = Group::from_gid(user.gid)?
        .map(|g| g.name)
        .with_context(|| format!("No encuentro el grupo de '{name}'"))?;
    Ok(UserInfo { name: name.to_string(), uid: user.uid.as_raw(), gid: user.gid.as_raw(), group, home: user.dir })
}

/// Configuración del usuario: `~/.config/cheka`.
pub fn user_conf_dir(home: &Path) -> std::path::PathBuf {
    home.join(".config/cheka")
}

/// Comando que vuelve a ejecutar `exe args…` con privilegios (`sudo -- exe args…`).
pub fn elevate_command(exe: &Path) -> Result<Command> {
    let mut cmd = Command::new("sudo");
    cmd.arg("--").arg(exe);
    Ok(cmd)
}

/// Sigue los logs como `tail -n 50 -F` (no regresa salvo error).
pub fn follow_logs(files: &[std::path::PathBuf]) -> io::Error {
    exec(Command::new("tail").args(["-n", "50", "-F"]).args(files))
}

/// Abre una URL con el navegador del usuario.
pub fn open_url_command(url: &str) -> Command {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url);
    cmd
}
