use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Equivale a `[[ -x ruta ]]`.
pub fn is_executable(p: &Path) -> bool {
    crate::platform::is_executable(p)
}

/// Equivale a `readlink -f`: resuelve todo, y admite que el último componente no exista.
pub fn canonicalize_lenient(p: &Path) -> Option<PathBuf> {
    if let Ok(c) = crate::platform::canonicalize(p) {
        return Some(c);
    }
    let parent = crate::platform::canonicalize(p.parent()?).ok()?;
    Some(parent.join(p.file_name()?))
}

/// Busca un ejecutable en el PATH (`command -v`).
pub fn which(name: impl AsRef<OsStr>) -> Option<PathBuf> {
    let name = name.as_ref();
    // Windows: `mkcert` también encuentra `mkcert.exe` (como hace la terminal con PATHEXT).
    let names: Vec<std::ffi::OsString> = if cfg!(windows) && Path::new(name).extension().is_none() {
        ["exe", "cmd", "bat"].iter().map(|e| Path::new(name).with_extension(e).into_os_string()).collect()
    } else {
        vec![name.to_os_string()]
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|c| c.is_file() && is_executable(c))
}

/// Busca un archivo (no necesariamente ejecutable) en las carpetas del PATH.
pub fn find_in_path(name: impl AsRef<OsStr>) -> Option<PathBuf> {
    let name = name.as_ref();
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

pub fn write_mode(path: &Path, contents: &str, mode: u32) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("No pude escribir {}", path.display()))?;
    crate::platform::set_mode(path, mode)?;
    Ok(())
}

pub fn write(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("No pude escribir {}", path.display()))
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("No pude crear {}", path.display()))
}

/// Archivos `*.conf` de primer nivel de un directorio.
pub fn conf_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "conf") && p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// `diff -rq a b` sobre directorios planos: mismos nombres y mismo contenido.
pub fn dirs_equal(a: &Path, b: &Path) -> bool {
    fn snapshot(d: &Path) -> Option<Vec<(std::ffi::OsString, Vec<u8>)>> {
        let mut v = Vec::new();
        for e in fs::read_dir(d).ok()?.flatten() {
            let ft = e.file_type().ok()?;
            if ft.is_dir() {
                return None; // no se esperan subdirectorios; tratarlos como diferencia
            }
            v.push((e.file_name(), fs::read(e.path()).ok()?));
        }
        v.sort();
        Some(v)
    }
    matches!((snapshot(a), snapshot(b)), (Some(x), Some(y)) if x == y)
}

/// Copia los archivos de primer nivel de `from` a `to` (como `cp -a from/. to/`, sin subdirectorios).
pub fn copy_files(from: &Path, to: &Path) -> Result<()> {
    for e in fs::read_dir(from)?.flatten() {
        if e.file_type()?.is_file() {
            fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}
