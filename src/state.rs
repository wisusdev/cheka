//! Estado del usuario. En la fase 1 se lee del mismo formato que usa la versión en bash
//! (archivos sueltos en `~/.config/cheka/`), para que ambas sean intercambiables.
//! `to_toml` produce el formato futuro (`cheka.toml`, ver docs/ARQUITECTURA.md §8.4).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::layout::{DB_PASS, DB_USER, TLD};

#[derive(Debug, Default, Clone)]
pub struct State {
    pub conf: PathBuf,
    /// Valor de `default_php=` en `config`, si existe y no está vacío.
    pub default_php: Option<String>,
    /// Líneas de `paths`, tal cual.
    pub paths: Vec<String>,
    /// `links/<nombre>` → destino del symlink (sin resolver).
    pub links: BTreeMap<String, PathBuf>,
    pub isolated: BTreeMap<String, String>,
    pub secured: BTreeSet<String>,
    pub docroot: BTreeMap<String, String>,
}

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
        Ok(st)
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

    pub fn refresh_request(&self) -> PathBuf {
        self.conf.join(".refresh-request")
    }

    pub fn to_toml(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Doc<'a> {
            version: u32,
            tld: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            default_php: Option<&'a str>,
            paths: Vec<&'a str>,
            links: BTreeMap<&'a str, String>,
            sites: BTreeMap<&'a str, SiteDoc<'a>>,
            db: Db<'a>,
        }
        #[derive(Serialize, Default)]
        struct SiteDoc<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            php: Option<&'a str>,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            secure: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            docroot: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Db<'a> {
            user: &'a str,
            password: &'a str,
        }

        let mut sites: BTreeMap<&str, SiteDoc> = BTreeMap::new();
        for (n, v) in &self.isolated {
            sites.entry(n).or_default().php = Some(v);
        }
        for n in &self.secured {
            sites.entry(n).or_default().secure = true;
        }
        for (n, d) in &self.docroot {
            sites.entry(n).or_default().docroot = Some(d);
        }
        let doc = Doc {
            version: 1,
            tld: TLD,
            default_php: self.default_php.as_deref(),
            paths: self.paths.iter().map(String::as_str).filter(|p| !p.is_empty()).collect(),
            links: self.links.iter().map(|(n, t)| (n.as_str(), t.display().to_string())).collect(),
            sites,
            db: Db { user: DB_USER, password: DB_PASS },
        };
        Ok(toml::to_string_pretty(&doc)?)
    }
}
