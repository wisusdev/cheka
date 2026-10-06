//! Detección del tipo de proyecto y su carpeta pública. Gana la primera regla que se cumple.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Custom,
    WordpressBedrock,
    Wordpress,
    WpMultisite,
    WpMultisiteSubdomains,
    Laravel,
    Codeigniter4,
    Codeigniter3,
    Php,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Custom => "personalizado",
            Kind::WordpressBedrock => "wordpress-bedrock",
            Kind::Wordpress => "wordpress",
            Kind::WpMultisite => "wp-multisite",
            Kind::WpMultisiteSubdomains => "wp-multisite-subdominios",
            Kind::Laravel => "laravel",
            Kind::Codeigniter4 => "codeigniter4",
            Kind::Codeigniter3 => "codeigniter3",
            Kind::Php => "php",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub kind: Kind,
    pub docroot: PathBuf,
}

/// `dir/sub` como concatenación de texto (bash: "$d/$sub").
fn sub(dir: &Path, rel: &str) -> PathBuf {
    PathBuf::from(format!("{}/{rel}", dir.display()))
}

fn wp_flag(config: &str, name: &str) -> bool {
    static RES: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (multi, sub) = RES.get_or_init(|| {
        (
            Regex::new(r#"MULTISITE['"][[:space:]]*,[[:space:]]*true"#).unwrap(),
            Regex::new(r#"SUBDOMAIN_INSTALL['"][[:space:]]*,[[:space:]]*true"#).unwrap(),
        )
    });
    match name {
        "MULTISITE" => multi.is_match(config),
        _ => sub.is_match(config),
    }
}

/// `docroot_override`: valor de `cheka docroot` para este sitio, si existe.
pub fn detect(dir: &Path, docroot_override: Option<&str>) -> Detection {
    let d = |kind, docroot| Detection { kind, docroot };
    let has = |rel: &str| sub(dir, rel).is_file();
    let has_dir = |rel: &str| sub(dir, rel).is_dir();

    if let Some(rel) = docroot_override {
        return d(Kind::Custom, sub(dir, rel));
    }
    if has("web/wp-config.php") || has_dir("web/wp") {
        return d(Kind::WordpressBedrock, sub(dir, "web"));
    }
    if has("wp-config.php") || has("wp-load.php") {
        let cfg = std::fs::read_to_string(sub(dir, "wp-config.php")).unwrap_or_default();
        let kind = if !wp_flag(&cfg, "MULTISITE") {
            Kind::Wordpress
        } else if wp_flag(&cfg, "SUBDOMAIN_INSTALL") {
            Kind::WpMultisiteSubdomains
        } else {
            Kind::WpMultisite
        };
        return d(kind, dir.to_path_buf());
    }
    if has("artisan") {
        return d(Kind::Laravel, sub(dir, "public"));
    }
    if has("spark") {
        return d(Kind::Codeigniter4, sub(dir, "public"));
    }
    if has_dir("system") && has_dir("application") {
        return d(Kind::Codeigniter3, dir.to_path_buf());
    }
    if has("public/index.php") {
        return d(Kind::Php, sub(dir, "public"));
    }
    d(Kind::Php, dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for f in files {
            let p = t.path().join(f);
            if f.ends_with('/') {
                fs::create_dir_all(&p).unwrap();
            } else {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, "").unwrap();
            }
        }
        t
    }

    fn kind(files: &[&str]) -> (Kind, String) {
        let t = tree(files);
        let det = detect(t.path(), None);
        let rel = det.docroot.strip_prefix(t.path()).unwrap().display().to_string();
        (det.kind, rel)
    }

    #[test]
    fn reglas_en_orden() {
        assert_eq!(kind(&["web/wp-config.php", "artisan"]), (Kind::WordpressBedrock, "web".into()));
        assert_eq!(kind(&["wp-load.php"]), (Kind::Wordpress, "".into()));
        assert_eq!(kind(&["artisan", "public/"]), (Kind::Laravel, "public".into()));
        assert_eq!(kind(&["spark"]), (Kind::Codeigniter4, "public".into()));
        assert_eq!(kind(&["system/", "application/"]), (Kind::Codeigniter3, "".into()));
        assert_eq!(kind(&["public/index.php"]), (Kind::Php, "public".into()));
        assert_eq!(kind(&["index.php"]), (Kind::Php, "".into()));
    }

    #[test]
    fn multisite() {
        let t = tree(&[]);
        fs::write(t.path().join("wp-config.php"), "define( 'MULTISITE', true );").unwrap();
        assert_eq!(detect(t.path(), None).kind, Kind::WpMultisite);
        fs::write(
            t.path().join("wp-config.php"),
            "define('MULTISITE',true);\ndefine( \"SUBDOMAIN_INSTALL\" ,  true );",
        )
        .unwrap();
        assert_eq!(detect(t.path(), None).kind, Kind::WpMultisiteSubdomains);
        fs::write(t.path().join("wp-config.php"), "define('MULTISITE', false);").unwrap();
        assert_eq!(detect(t.path(), None).kind, Kind::Wordpress);
    }

    #[test]
    fn docroot_personalizado_gana() {
        let t = tree(&["artisan", "htdocs/"]);
        let det = detect(t.path(), Some("htdocs"));
        assert_eq!(det.kind, Kind::Custom);
        assert!(det.docroot.ends_with("htdocs"));
    }
}
