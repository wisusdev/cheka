//! Estado del usuario (docs/ARQUITECTURA.md §8.4).
//!
//! La fuente de verdad es `~/.config/cheka/cheka.toml`. Si todavía no existe, se lee el
//! formato de la versión en bash (archivos sueltos: `config`, `paths`, `links/`…). La
//! primera vez que se guarda, se escribe `cheka.toml` y los archivos viejos se mueven a
//! `legacy/` (nunca se borran). `save_legacy` hace el camino inverso, para volver a bash.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::layout::{DB_PASS, DB_USER, TLD};
use crate::userfs;

pub const TOML_FILE: &str = "cheka.toml";
const TOML_VERSION: u32 = 1;
/// Archivos y carpetas del formato de bash (los certificados no son estado: se quedan).
const LEGACY_ITEMS: [&str; 7] = ["config", "paths", "links", "isolated", "secured", "docroot", ".refresh-request"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Archivos sueltos de la versión en bash.
    #[default]
    Legacy,
    Toml,
}

#[derive(Debug, Clone)]
pub struct State {
    pub conf: PathBuf,
    pub format: Format,
    pub default_php: Option<String>,
    pub paths: Vec<String>,
    /// nombre → carpeta del proyecto
    pub links: BTreeMap<String, PathBuf>,
    pub isolated: BTreeMap<String, String>,
    pub secured: BTreeSet<String>,
    pub docroot: BTreeMap<String, String>,
    pub db_user: String,
    pub db_password: String,
    /// Ajustes por versión de PHP (`[php."8.5"]`).
    pub php: BTreeMap<String, PhpSettings>,
}

/// Ajustes de una versión de PHP que cheka aplica sobre su php.ini.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhpSettings {
    /// Directivas de php.ini (p. ej. upload_max_filesize = "512M").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ini: BTreeMap<String, String>,
    /// Extensiones activadas (true) o desactivadas (false) respecto a lo que trae el sistema.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, bool>,
}

impl PhpSettings {
    pub fn is_empty(&self) -> bool {
        self.ini.is_empty() && self.extensions.is_empty()
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            conf: PathBuf::new(),
            format: Format::Legacy,
            default_php: None,
            paths: Vec::new(),
            links: BTreeMap::new(),
            isolated: BTreeMap::new(),
            secured: BTreeSet::new(),
            docroot: BTreeMap::new(),
            db_user: DB_USER.to_string(),
            db_password: DB_PASS.to_string(),
            php: BTreeMap::new(),
        }
    }
}

// ------------------------------------------------------------------- TOML ----

#[derive(Serialize, Deserialize)]
struct Doc {
    version: u32,
    #[serde(default = "default_tld")]
    tld: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_php: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    links: BTreeMap<String, String>,
    #[serde(default)]
    sites: BTreeMap<String, SiteDoc>,
    #[serde(default)]
    db: DbDoc,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    php: BTreeMap<String, PhpSettings>,
}

#[derive(Serialize, Deserialize, Default)]
struct SiteDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    php: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    secure: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    docroot: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct DbDoc {
    user: String,
    password: String,
}

impl Default for DbDoc {
    fn default() -> Self {
        Self { user: DB_USER.into(), password: DB_PASS.into() }
    }
}

fn default_tld() -> String {
    TLD.into()
}

// ----------------------------------------------------------------- legacy ----

/// `$(<archivo)` en bash: contenido sin los saltos de línea finales.
fn read_value(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim_end_matches('\n').to_string())
}

/// Archivos visibles de un directorio (como el glob `dir/*`).
fn visible_entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (!name.starts_with('.')).then(|| (name, e.path()))
        })
        .collect();
    out.sort();
    out
}

impl State {
    pub fn load(conf: &Path) -> Result<Self> {
        let toml_path = conf.join(TOML_FILE);
        if toml_path.exists() {
            Self::load_toml(conf, &toml_path)
        } else {
            Ok(Self::load_legacy(conf))
        }
    }

    fn load_toml(conf: &Path, path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("No pude leer {}", path.display()))?;
        let doc: Doc = toml::from_str(&text).with_context(|| format!("{} no es válido", path.display()))?;
        if doc.version != TOML_VERSION {
            bail!("{}: versión {} no soportada (esta versión de cheka entiende la {TOML_VERSION})", path.display(), doc.version);
        }
        if doc.tld != TLD {
            bail!("{}: tld = \"{}\" no está soportado todavía (solo \"{TLD}\")", path.display(), doc.tld);
        }
        let mut st = State {
            conf: conf.to_path_buf(),
            format: Format::Toml,
            default_php: doc.default_php.filter(|v| !v.is_empty()),
            paths: doc.paths,
            links: doc.links.into_iter().map(|(n, p)| (n, PathBuf::from(p))).collect(),
            db_user: doc.db.user,
            db_password: doc.db.password,
            php: doc.php.into_iter().filter(|(_, s)| !s.is_empty()).collect(),
            ..Default::default()
        };
        for (name, site) in doc.sites {
            if let Some(v) = site.php {
                st.isolated.insert(name.clone(), v);
            }
            if site.secure {
                st.secured.insert(name.clone());
            }
            if let Some(d) = site.docroot {
                st.docroot.insert(name, d);
            }
        }
        Ok(st)
    }

    fn load_legacy(conf: &Path) -> Self {
        let mut st = State { conf: conf.to_path_buf(), ..Default::default() };
        if let Ok(cfg) = fs::read_to_string(conf.join("config")) {
            st.default_php = cfg
                .lines()
                .rev()
                .find_map(|l| l.strip_prefix("default_php="))
                .filter(|v| !v.is_empty())
                .map(str::to_string);
        }
        if let Ok(paths) = fs::read_to_string(conf.join("paths")) {
            st.paths = paths.lines().map(str::to_string).collect();
        }
        for (name, path) in visible_entries(&conf.join("links")) {
            if let Ok(target) = fs::read_link(&path) {
                let target = if target.is_absolute() { target } else { conf.join("links").join(target) };
                st.links.insert(name, target);
            }
        }
        for (name, path) in visible_entries(&conf.join("isolated")) {
            if let Some(v) = read_value(&path).filter(|_| path.is_file()) {
                st.isolated.insert(name, v);
            }
        }
        for (name, path) in visible_entries(&conf.join("secured")) {
            if path.is_file() {
                st.secured.insert(name);
            }
        }
        for (name, path) in visible_entries(&conf.join("docroot")) {
            if let Some(v) = read_value(&path).filter(|_| path.is_file()) {
                st.docroot.insert(name, v);
            }
        }
        st
    }

    /// Mismo contenido, sin importar dónde ni en qué formato está guardado.
    pub fn same_content(&self, other: &State) -> bool {
        let key = |s: &State| {
            (
                s.default_php.clone(),
                s.paths.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>(),
                s.links.clone(),
                s.isolated.clone(),
                s.secured.clone(),
                s.docroot.clone(),
                (s.db_user.clone(), s.db_password.clone()),
                s.php.clone(),
            )
        };
        key(self) == key(other)
    }

    pub fn has_legacy_files(&self) -> bool {
        LEGACY_ITEMS.iter().any(|i| fs::symlink_metadata(self.conf.join(i)).is_ok())
    }

    pub fn to_toml(&self) -> Result<String> {
        let mut sites: BTreeMap<String, SiteDoc> = BTreeMap::new();
        for (n, v) in &self.isolated {
            sites.entry(n.clone()).or_default().php = Some(v.clone());
        }
        for n in &self.secured {
            sites.entry(n.clone()).or_default().secure = true;
        }
        for (n, d) in &self.docroot {
            sites.entry(n.clone()).or_default().docroot = Some(d.clone());
        }
        let doc = Doc {
            version: TOML_VERSION,
            tld: TLD.into(),
            default_php: self.default_php.clone(),
            paths: self.paths.iter().filter(|p| !p.is_empty()).cloned().collect(),
            links: self.links.iter().map(|(n, t)| (n.clone(), t.display().to_string())).collect(),
            sites,
            db: DbDoc { user: self.db_user.clone(), password: self.db_password.clone() },
            php: self.php.iter().filter(|(_, s)| !s.is_empty()).map(|(v, s)| (v.clone(), s.clone())).collect(),
        };
        let body = toml::to_string_pretty(&doc)?;
        Ok(format!("# Estado de cheka. Lo escribe cheka; puedes editarlo a mano con cuidado.\n{body}"))
    }

    /// Guarda en `cheka.toml` (escritura atómica) y, si quedaban archivos del formato
    /// de bash, los mueve a `legacy/`.
    pub fn save(&mut self, id: &Identity) -> Result<()> {
        userfs::mkdir(id, &self.conf)?;
        let path = self.conf.join(TOML_FILE);
        let tmp = self.conf.join(format!(".{TOML_FILE}.tmp"));
        userfs::write(id, &tmp, &self.to_toml()?)?;
        fs::rename(&tmp, &path).with_context(|| format!("No pude escribir {}", path.display()))?;
        self.format = Format::Toml;
        if self.has_legacy_files() {
            let legacy = self.conf.join("legacy");
            userfs::mkdir(id, &legacy)?;
            for item in LEGACY_ITEMS {
                let from = self.conf.join(item);
                if fs::symlink_metadata(&from).is_err() {
                    continue;
                }
                let to = legacy.join(item);
                if to.is_dir() && !to.is_symlink() {
                    fs::remove_dir_all(&to)?;
                } else if fs::symlink_metadata(&to).is_ok() {
                    fs::remove_file(&to)?;
                }
                fs::rename(&from, &to).with_context(|| format!("No pude mover {} a legacy/", from.display()))?;
            }
        }
        Ok(())
    }

    /// Escribe el estado en el formato de bash (para volver a esa versión) y aparta
    /// `cheka.toml` como `cheka.toml.bak`. Los ajustes de PHP (`[php]`) no existen en la
    /// versión en bash: se pierden en ese formato, pero siguen en el `.bak`.
    pub fn save_legacy(&mut self, id: &Identity) -> Result<()> {
        let c = self.conf.clone();
        for dir in ["links", "isolated", "secured", "docroot"] {
            let d = c.join(dir);
            if d.is_dir() {
                fs::remove_dir_all(&d)?;
            }
            userfs::mkdir(id, &d)?;
        }
        let default = self.default_php.as_ref().map(|v| format!("default_php={v}\n")).unwrap_or_default();
        userfs::write(id, &c.join("config"), &default)?;
        let paths: String = self.paths.iter().filter(|p| !p.is_empty()).map(|p| format!("{p}\n")).collect();
        userfs::write(id, &c.join("paths"), &paths)?;
        for (n, target) in &self.links {
            userfs::symlink_force(id, target, &c.join("links").join(n))?;
        }
        for (n, v) in &self.isolated {
            userfs::write(id, &c.join("isolated").join(n), &format!("{v}\n"))?;
        }
        for n in &self.secured {
            userfs::write(id, &c.join("secured").join(n), "")?;
        }
        for (n, d) in &self.docroot {
            userfs::write(id, &c.join("docroot").join(n), &format!("{d}\n"))?;
        }
        let toml = c.join(TOML_FILE);
        if toml.exists() {
            fs::rename(&toml, c.join(format!("{TOML_FILE}.bak")))?;
        }
        self.format = Format::Legacy;
        Ok(())
    }

    pub fn default_php(&self) -> String {
        self.default_php.clone().unwrap_or_else(crate::php::system_php)
    }

    pub fn site_php(&self, site: &str) -> String {
        self.isolated.get(site).cloned().unwrap_or_else(|| self.default_php())
    }

    pub fn cert(&self, site: &str) -> PathBuf {
        self.conf.join(format!("certs/{site}.{TLD}.pem"))
    }

    pub fn cert_key(&self, site: &str) -> PathBuf {
        self.conf.join(format!("certs/{site}.{TLD}-key.pem"))
    }

    /// Seguro = marcado con `secure` y con el certificado presente.
    pub fn is_secure(&self, site: &str) -> bool {
        self.secured.contains(site) && self.cert(site).is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(conf: &Path) -> State {
        let mut st = State { conf: conf.to_path_buf(), default_php: Some("8.3".into()), ..Default::default() };
        st.paths = vec!["/home/u/Sites".into(), "/srv/otros".into()];
        st.links.insert("api".into(), "/home/u/proyectos/api".into());
        st.isolated.insert("blog".into(), "8.1".into());
        st.secured.insert("blog".into());
        st.secured.insert("tienda".into());
        st.docroot.insert("legado".into(), "htdocs".into());
        st
    }

    #[test]
    fn toml_ida_y_vuelta() {
        let t = tempfile::tempdir().unwrap();
        let st = sample(t.path());
        fs::write(t.path().join(TOML_FILE), st.to_toml().unwrap()).unwrap();
        let back = State::load(t.path()).unwrap();
        assert_eq!(back.format, Format::Toml);
        assert!(st.same_content(&back));
    }

    #[test]
    fn toml_legible() {
        let t = tempfile::tempdir().unwrap();
        let text = sample(t.path()).to_toml().unwrap();
        assert!(text.contains("default_php = \"8.3\""), "{text}");
        assert!(text.contains("[sites.blog]\nphp = \"8.1\"\nsecure = true"), "{text}");
        assert!(text.contains("[sites.tienda]\nsecure = true"), "{text}");
        assert!(text.contains("[db]\nuser = \"cheka\"\npassword = \"secret\""), "{text}");
    }

    #[test]
    fn rechaza_version_desconocida() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join(TOML_FILE), "version = 99\n").unwrap();
        let err = State::load(t.path()).unwrap_err().to_string();
        assert!(err.contains("versión 99 no soportada"), "{err}");
    }

    #[test]
    fn toml_minimo_usa_valores_por_defecto() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join(TOML_FILE), "version = 1\n").unwrap();
        let st = State::load(t.path()).unwrap();
        assert_eq!((st.db_user.as_str(), st.db_password.as_str()), ("cheka", "secret"));
        assert!(st.paths.is_empty() && st.default_php.is_none());
    }
}
