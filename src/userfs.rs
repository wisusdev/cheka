//! Escritura en el estado del usuario (`~/.config/cheka`). Si el comando corre como root
//! (p. ej. durante `install`), los archivos quedan a nombre del usuario, como el `as_user`
//! de la versión en bash.

use std::fs;
use std::os::unix::fs::{chown, lchown, symlink};
use std::path::Path;

use anyhow::{Context, Result};
use nix::unistd::geteuid;

use crate::identity::Identity;

fn give(id: &Identity, p: &Path, link: bool) -> Result<()> {
    if geteuid().is_root() && id.user != "root" {
        let (u, g) = (Some(id.uid.as_raw()), Some(id.gid.as_raw()));
        if link { lchown(p, u, g)? } else { chown(p, u, g)? }
    }
    Ok(())
}

/// Deja el archivo a nombre del usuario (solo hace algo si corremos como root).
pub fn own(id: &Identity, path: &Path) -> Result<()> {
    give(id, path, false)
}

pub fn mkdir(id: &Identity, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("No pude crear {}", dir.display()))?;
    give(id, dir, false)
}

pub fn write(id: &Identity, path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        mkdir(id, parent)?;
    }
    fs::write(path, contents).with_context(|| format!("No pude escribir {}", path.display()))?;
    give(id, path, false)
}

/// `touch`: crea el archivo vacío si no existe (no cambia su contenido).
pub fn touch(id: &Identity, path: &Path) -> Result<()> {
    if !path.exists() {
        write(id, path, "")?;
    }
    Ok(())
}

pub fn append_line(id: &Identity, path: &Path, line: &str) -> Result<()> {
    let mut s = fs::read_to_string(path).unwrap_or_default();
    s.push_str(line);
    s.push('\n');
    write(id, path, &s)
}

/// `ln -sfn destino enlace`
pub fn symlink_force(id: &Identity, target: &Path, link: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        mkdir(id, parent)?;
    }
    if fs::symlink_metadata(link).is_ok() {
        fs::remove_file(link)?;
    }
    symlink(target, link).with_context(|| format!("No pude crear el enlace {}", link.display()))?;
    give(id, link, true)
}

/// `rm -f`
pub fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
