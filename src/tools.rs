//! Catálogo de herramientas (`tools/linux.toml`) y su instalación.
//!
//! Cada herramienta tiene una parte de root y otra del usuario. Los pasos de root se
//! ejecutan con una sola elevación (`sudo`/`pkexec cheka tools _root …`) y avisan de su
//! resultado con una línea `::cheka-tool ok|fail <id>`; luego la parte del usuario corre
//! como él (nvm, rustup y compañía deben quedar en su home, no en el de root).

use std::collections::BTreeSet;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use minijinja::{Environment, context};
use serde::{Deserialize, Serialize};

use crate::Ctx;

pub const MARKER: &str = "::cheka-tool";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    pub detect: String,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Deserialize)]
struct Catalog {
    tool: Vec<Tool>,
}

/// El catálogo incluido en el binario. `CHEKA_TOOLS=/ruta.toml` usa otro (para pruebas: sudo
/// no conserva la variable, así que los pasos de root siempre usan el incluido).
pub fn catalog() -> Vec<Tool> {
    if let Some(path) = std::env::var_os("CHEKA_TOOLS") {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        return toml::from_str::<Catalog>(&text).map(|c| c.tool).unwrap_or_default();
    }
    toml::from_str::<Catalog>(include_str!("../tools/linux.toml")).expect("tools/linux.toml inválido").tool
}

/// Funciones disponibles en los pasos del catálogo.
const PREAMBLE: &str = r#"
apt_install() { DEBIAN_FRONTEND=noninteractive apt-get install -y "$@"; }
_cheka_line() {
  mkdir -p "$(dirname "$1")"; touch "$1"
  grep -qxF -- "$2" "$1" || echo "$2" >> "$1"
  if [ "$(id -u)" = 0 ]; then chown "$CHEKA_USER:" "$1"; fi
}
add_path() {
  _cheka_line "$CHEKA_HOME/.bashrc" "export PATH=\"$1:\$PATH\""
  if command -v fish >/dev/null; then _cheka_line "$CHEKA_HOME/.config/fish/conf.d/cheka-tools.fish" "fish_add_path -g $1"; fi
}
add_env() {
  _cheka_line "$CHEKA_HOME/.bashrc" "export $1=\"$2\""
  if command -v fish >/dev/null; then _cheka_line "$CHEKA_HOME/.config/fish/conf.d/cheka-tools.fish" "set -gx $1 $2"; fi
}
"#;

#[derive(Debug, Serialize)]
pub struct ToolStatus {
    #[serde(flatten)]
    pub tool: Tool,
    pub installed: bool,
    pub needs_root: bool,
}

/// Qué herramientas ya están instaladas (una sola invocación de bash para todas).
pub fn status(ctx: &Ctx) -> Vec<ToolStatus> {
    let tools = catalog();
    let mut script = String::from(
        "export PATH=\"$HOME/.cargo/bin:/usr/local/go/bin:$HOME/.config/composer/vendor/bin:/opt/flutter/bin:$PATH\"\n",
    );
    for t in &tools {
        script.push_str(&format!("if ( {} ) >/dev/null 2>&1; then echo '{}:1'; else echo '{}:0'; fi\n", t.detect, t.id, t.id));
    }
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HOME", &ctx.id.home)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let installed: BTreeSet<&str> = out.lines().filter_map(|l| l.strip_suffix(":1")).collect();
    tools
        .into_iter()
        .map(|t| ToolStatus {
            installed: installed.contains(t.id.as_str()),
            needs_root: !t.root.trim().is_empty(),
            tool: t,
        })
        .collect()
}

/// Herramientas pedidas más sus requisitos, en el orden del catálogo.
pub fn resolve(ids: &[String]) -> Result<Vec<Tool>> {
    let all = catalog();
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<String> = ids.to_vec();
    while let Some(id) = pending.pop() {
        let Some(t) = all.iter().find(|t| t.id == id) else {
            let known: Vec<&str> = all.iter().map(|t| t.id.as_str()).collect();
            bail!("Herramienta desconocida: '{id}'. Disponibles: {}", known.join(", "));
        };
        if wanted.insert(id.clone()) {
            pending.extend(t.requires.iter().cloned());
        }
    }
    Ok(all.into_iter().filter(|t| wanted.contains(&t.id)).collect())
}

fn render(ctx: &Ctx, snippet: &str) -> Result<String> {
    // Las credenciales van dentro de comillas SQL: no pueden tener comillas.
    for v in [&ctx.state.db_user, &ctx.state.db_password] {
        if v.contains(['\'', '"', '\\', '$']) {
            bail!("[db] en cheka.toml: el usuario y la contraseña no pueden tener ' \" \\ ni $");
        }
    }
    let env = Environment::new();
    Ok(env.render_str(snippet, context! { db_user => &ctx.state.db_user, db_password => &ctx.state.db_password })?)
}

/// Ejecuta un paso del catálogo (la salida va directo a la terminal o a la UI).
pub fn run_step(ctx: &Ctx, tool: &Tool, snippet: &str) -> Result<bool> {
    let tmp = tempfile::tempdir()?;
    let script = format!("{PREAMBLE}\n{}", render(ctx, snippet)?);
    let st = Command::new("bash")
        .args(["-euo", "pipefail", "-c", &script])
        .current_dir(tmp.path())
        .env("CHEKA_USER", &ctx.id.user)
        .env("CHEKA_HOME", &ctx.id.home)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("No pude ejecutar los pasos de {}", tool.name))?;
    Ok(st.success())
}

pub fn marker(ok: bool, id: &str) -> String {
    format!("{MARKER} {} {id}", if ok { "ok" } else { "fail" })
}

/// Lee una línea marcadora: Some((id, ok)).
pub fn parse_marker(line: &str) -> Option<(String, bool)> {
    let rest = line.strip_prefix(MARKER)?.trim();
    let (state, id) = rest.split_once(' ')?;
    Some((id.trim().to_string(), state == "ok"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogo_valido() {
        let all = catalog();
        assert!(all.len() >= 20);
        let ids: BTreeSet<&str> = all.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids.len(), all.len(), "ids repetidos");
        for t in &all {
            assert!(!t.root.trim().is_empty() || !t.user.trim().is_empty(), "{} no tiene pasos", t.id);
            for r in &t.requires {
                assert!(ids.contains(r.as_str()), "{} requiere '{r}', que no existe", t.id);
            }
        }
        assert!(ids.contains("rust"));
        assert!(!ids.contains("php") && !ids.contains("apache"), "PHP y Apache los gestiona cheka");
    }

    #[test]
    fn requisitos_y_orden() {
        let tools = resolve(&["laravel".into()]).unwrap();
        let ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["composer", "laravel"]);
        assert!(resolve(&["no-existe".into()]).is_err());
    }

    #[test]
    fn marcadores() {
        assert_eq!(parse_marker(&marker(true, "go")), Some(("go".into(), true)));
        assert_eq!(parse_marker(&marker(false, "go")), Some(("go".into(), false)));
        assert_eq!(parse_marker("otra línea"), None);
    }
}
