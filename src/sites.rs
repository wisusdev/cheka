//! Enumeración de sitios: carpetas dentro de las rutas aparcadas + enlaces.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::state::State;
use crate::util::canonicalize_lenient;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub name: String,
    /// Ruta real (resuelta) del proyecto.
    pub path: PathBuf,
}

/// Nombre de sitio a partir de un nombre de carpeta: minúsculas, todo lo que no sea
/// `[a-z0-9-]` se vuelve `-`, y se quita **un** guion al inicio y al final
/// (igual que `${n##-}` / `${n%%-}` en bash).
pub fn normalize(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let n: String = lower
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' { c } else { '-' })
        .collect();
    let n = n.strip_prefix('-').unwrap_or(&n);
    n.strip_suffix('-').unwrap_or(n).to_string()
}

/// Subcarpetas visibles de `dir` (siguiendo symlinks), como el glob `dir/*/`.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// Todos los sitios, ordenados por nombre. Los enlaces tienen prioridad sobre las
/// carpetas aparcadas con el mismo nombre.
pub fn list(state: &State) -> Vec<Site> {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for p in state.paths.iter().filter(|p| !p.is_empty()) {
        let p = Path::new(p);
        if !p.is_dir() {
            continue;
        }
        for d in subdirs(p) {
            let Some(real) = canonicalize_lenient(&d) else { continue };
            // En bash el nombre sale de la ruta ya resuelta (basename de readlink -f).
            let name = normalize(&real.file_name().unwrap_or_default().to_string_lossy());
            if !name.is_empty() {
                seen.insert(name, real);
            }
        }
    }
    for (name, target) in &state.links {
        let link = state.conf.join("links").join(name);
        let target = if target.is_absolute() { target.clone() } else { link.parent().unwrap().join(target) };
        if let Some(real) = canonicalize_lenient(&target) {
            seen.insert(name.clone(), real);
        }
    }
    seen.into_iter().map(|(name, path)| Site { name, path }).collect()
}

/// El sitio pedido por nombre o, sin nombre, el que contiene a `cwd` (el más específico).
pub fn resolve(sites: &[Site], want: Option<&str>, cwd: &Path) -> Result<Site> {
    if let Some(want) = want.filter(|w| !w.is_empty()) {
        return match sites.iter().find(|s| s.name == want) {
            Some(s) => Ok(s.clone()),
            None => bail!("No existe el sitio '{want}'. Revisa: cheka sites"),
        };
    }
    let cwd = canonicalize_lenient(cwd).unwrap_or_else(|| cwd.to_path_buf());
    sites
        .iter()
        .filter(|s| cwd.starts_with(&s.path))
        .max_by_key(|s| s.path.as_os_str().len())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Este directorio no es un sitio de cheka. Usa 'cheka link' o 'cheka park'."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_como_bash() {
        assert_eq!(normalize("My_Site"), "my-site");
        assert_eq!(normalize("Proyecto X"), "proyecto-x");
        assert_eq!(normalize("_raro_"), "raro");
        assert_eq!(normalize("__doble"), "-doble"); // bash solo quita un guion
        assert_eq!(normalize("Año"), "a-o");
        assert_eq!(normalize("api.v2"), "api-v2");
    }

    #[test]
    fn resuelve_el_mas_especifico() {
        let s = vec![
            Site { name: "a".into(), path: "/x/a".into() },
            Site { name: "b".into(), path: "/x/a/b".into() },
        ];
        assert_eq!(resolve(&s, None, Path::new("/x/a/b/c")).unwrap().name, "b");
        assert_eq!(resolve(&s, None, Path::new("/x/a/z")).unwrap().name, "a");
        assert!(resolve(&s, None, Path::new("/x/ab")).is_err());
        assert_eq!(resolve(&s, Some("a"), Path::new("/")).unwrap().name, "a");
    }
}
