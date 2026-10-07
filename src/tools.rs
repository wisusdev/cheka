//! Catálogo de herramientas (`tools/linux.toml`, `tools/windows.toml`) y su instalación.
//!
//! Cada herramienta tiene una parte de root y otra del usuario. Los pasos de root se
//! ejecutan con una sola elevación (`sudo`/`pkexec cheka tools _root …`; UAC en Windows) y
//! avisan de su resultado con una línea `::cheka-tool ok|fail <id>`; luego la parte del
//! usuario corre como él (nvm, rustup y compañía deben quedar en su home, no en el de root).
//!
//! Los dos catálogos tienen los mismos ids: en Linux los pasos son bash y en Windows
//! PowerShell, con ayudantes equivalentes (`apt_install` ↔ `winget_install`, `add_path`…).

use std::collections::{BTreeMap, BTreeSet};
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
    /// Comando que imprime la versión (si falta: la del primer paquete).
    #[serde(default)]
    pub version: String,
    /// Comando que imprime dónde está instalada.
    #[serde(default)]
    pub path: String,
    /// Paquetes de apt (versión y tamaño sin ejecutar nada).
    #[serde(default)]
    pub packages: Vec<String>,
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
    toml::from_str::<Catalog>(CATALOG).expect("catálogo de herramientas inválido").tool
}

#[cfg(unix)]
const CATALOG: &str = include_str!("../tools/linux.toml");
#[cfg(windows)]
const CATALOG: &str = include_str!("../tools/windows.toml");

/// Windows: funciones disponibles en los pasos, la detección y la información del catálogo
/// (los equivalentes de PREAMBLE e INFO_PREAMBLE de bash).
#[cfg(windows)]
const PS_PREAMBLE: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
# Variables y PATH recién instalados (el proceso de cheka puede tener los de antes).
function refresh_path {
  foreach ($scope in 'Machine', 'User') {
    $vars = [Environment]::GetEnvironmentVariables($scope)
    foreach ($k in $vars.Keys) { if ($k -ne 'Path') { Set-Item -Path "env:$k" -Value $vars[$k] } }
  }
  $m = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $u = [Environment]::GetEnvironmentVariable('Path', 'User')
  $env:Path = [Environment]::ExpandEnvironmentVariables("$m;$u")
}
function must([scriptblock]$cmd) {
  & $cmd
  if ($LASTEXITCODE) { throw "Falló ($LASTEXITCODE): $cmd" }
}
function winget_install {
  foreach ($id in $args) {
    winget install -e --id $id --silent --accept-source-agreements --accept-package-agreements --disable-interactivity
    # 0x8A15002B: ya está instalado y al día
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) { throw "winget install $id falló ($LASTEXITCODE)" }
  }
  refresh_path
}
function add_path([string]$dir) {
  $parts = @(([Environment]::GetEnvironmentVariable('Path', 'User')) -split ';' | Where-Object { $_ })
  if ($parts -notcontains $dir) { [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';'), 'User') }
  refresh_path
}
function add_env([string]$name, [string]$value) {
  [Environment]::SetEnvironmentVariable($name, $value, 'User')
  refresh_path
}
function has([string]$cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }
# Programas instalados (se lee el registro una sola vez por ejecución).
$script:__apps = $null
function app([string]$pattern) {
  if ($null -eq $script:__apps) {
    $keys = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
    $script:__apps = @(Get-ItemProperty $keys -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName })
  }
  $script:__apps | Where-Object { $_.DisplayName -like $pattern } | Select-Object -First 1
}
function app_dir([string]$pattern) {
  $a = app $pattern
  if ($a.InstallLocation) { $a.InstallLocation.TrimEnd('\') }
  elseif ($a.DisplayIcon) { Split-Path (($a.DisplayIcon -replace '"', '') -replace ',-?\d+$', '') }
}
function ver { process { if ("$_" -match '\d+(\.[0-9A-Za-z]+)+') { $Matches[0] } } }
function bindir([string]$cmd) { Split-Path (Get-Command $cmd -ErrorAction Stop).Source }
refresh_path
"#;

/// PowerShell con el preámbulo. El script va codificado (`-EncodedCommand`) para que las
/// comillas de los pasos lleguen intactas.
#[cfg(windows)]
fn powershell(script: &str) -> Command {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(crate::platform::encode_powershell(&format!("{PS_PREAMBLE}\n{script}")));
    cmd
}

/// Funciones disponibles en los pasos del catálogo.
#[cfg(unix)]
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

/// Windows: qué herramientas ya están instaladas (una sola invocación de PowerShell).
#[cfg(windows)]
pub fn status(_ctx: &Ctx) -> Vec<ToolStatus> {
    let tools = catalog();
    let mut script = String::new();
    for t in &tools {
        script.push_str(&format!(
            "try {{ $ErrorActionPreference = 'Continue'; if (& {{ {} }}) {{ '{}:1' }} else {{ '{}:0' }} }} catch {{ '{}:0' }}\n",
            t.detect, t.id, t.id, t.id
        ));
    }
    let out = powershell(&script)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let installed: BTreeSet<&str> = out.lines().filter_map(|l| l.trim().strip_suffix(":1")).collect();
    tools
        .into_iter()
        .map(|t| ToolStatus {
            installed: installed.contains(t.id.as_str()),
            needs_root: !t.root.trim().is_empty(),
            tool: t,
        })
        .collect()
}

/// Qué herramientas ya están instaladas (una sola invocación de bash para todas).
#[cfg(unix)]
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

/// Windows: ejecuta un paso del catálogo con PowerShell.
#[cfg(windows)]
pub fn run_step(ctx: &Ctx, tool: &Tool, snippet: &str) -> Result<bool> {
    let tmp = tempfile::tempdir()?;
    // Con -EncodedCommand, PowerShell 5.1 escribe los errores en CLIXML: se muestran como
    // texto y el paso termina con código 1.
    let script = format!(
        "try {{\n{}\n}} catch {{ [Console]::Error.WriteLine(\"Error: \" + $_.Exception.Message); exit 1 }}",
        render(ctx, snippet)?
    );
    let st = powershell(&script)
        .current_dir(tmp.path())
        .env("CHEKA_USER", &ctx.id.user)
        .env("CHEKA_HOME", &ctx.id.home)
        .env("CHEKA_EXE", std::env::current_exe()?)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("No pude ejecutar los pasos de {}", tool.name))?;
    Ok(st.success())
}

/// Ejecuta un paso del catálogo (la salida va directo a la terminal o a la UI).
#[cfg(unix)]
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

// ------------------------------------------------------------------- detalles ----

#[derive(Debug, Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub id: String,
    pub version: Option<String>,
    pub path: Option<String>,
    pub packages: Vec<PackageInfo>,
    /// Solo con `--size`: suma de los paquetes, o lo que ocupa la carpeta.
    pub size_bytes: Option<u64>,
}

/// Funciones para los comandos `version` y `path` del catálogo.
#[cfg(unix)]
const INFO_PREAMBLE: &str = r#"
ver() { grep -oE '[0-9]+([.][0-9A-Za-z]+)+' | head -1; }
bindir() { dirname "$(readlink -f "$(command -v "$1")")"; }
export -f ver bindir
export PATH="$HOME/.cargo/bin:/usr/local/go/bin:$HOME/.config/composer/vendor/bin:/opt/flutter/bin:$PATH"
"#;

/// Quita la época ("1:") y la revisión de Debian ("-0ubuntu1") de una versión de apt.
#[cfg(unix)]
fn upstream(v: &str) -> String {
    let v = v.split_once(':').map_or(v, |(_, rest)| rest);
    v.rsplit_once('-').map_or(v, |(up, _)| up).to_string()
}

/// Paquete → (versión, tamaño instalado en bytes).
#[cfg(unix)]
fn dpkg_info(packages: &[String]) -> BTreeMap<String, (String, u64)> {
    if packages.is_empty() {
        return BTreeMap::new();
    }
    let out = Command::new("dpkg-query")
        .args(["-W", "-f=${Package}\t${Version}\t${Installed-Size}\n"])
        .args(packages)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, version, kib) = (f.next()?, f.next()?, f.next().unwrap_or("0"));
            (!version.is_empty()).then(|| (name.to_string(), (version.to_string(), kib.parse::<u64>().unwrap_or(0) * 1024)))
        })
        .collect()
}

#[cfg(unix)]
fn du(path: &str) -> Option<u64> {
    let out = Command::new("timeout").args(["10", "du", "-sb", path]).stderr(Stdio::null()).output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.parse().ok()
}

/// Windows: lo que ocupa una carpeta (como `du -sb`, con el mismo límite de 10 s).
#[cfg(windows)]
fn du(path: &str) -> Option<u64> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut total = 0u64;
    let mut pending = vec![std::path::PathBuf::from(path)];
    while let Some(dir) = pending.pop() {
        if std::time::Instant::now() > deadline {
            return None;
        }
        for e in std::fs::read_dir(&dir).ok()?.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                pending.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    Some(total)
}

/// Windows: versión y carpeta de las herramientas indicadas (o de todas las instaladas). No
/// hay paquetes de apt: la versión sale del comando `version` del catálogo.
#[cfg(windows)]
pub fn info(ctx: &Ctx, ids: &[String], with_size: bool) -> Result<Vec<ToolInfo>> {
    let tools: Vec<Tool> = if ids.is_empty() {
        status(ctx).into_iter().filter(|s| s.installed).map(|s| s.tool).collect()
    } else {
        resolve_exact(ids)?
    };
    let mut script = String::new();
    for t in &tools {
        for (kind, cmd) in [("version", &t.version), ("path", &t.path)] {
            if !cmd.trim().is_empty() {
                script.push_str(&format!(
                    "try {{ $ErrorActionPreference = 'Continue'; $v = @(& {{ {cmd} }} 2>$null)[0]; if ($v) {{ \"{}`t{kind}`t$v\" }} }} catch {{ }}\n",
                    t.id
                ));
            }
        }
    }
    let out = powershell(&script)
        .env("CHEKA_HOME", &ctx.id.home)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut values: BTreeMap<(String, String), String> = BTreeMap::new();
    for line in out.lines() {
        let mut f = line.splitn(3, '\t');
        if let (Some(id), Some(kind), Some(v)) = (f.next(), f.next(), f.next())
            && !v.trim().is_empty()
        {
            values.insert((id.to_string(), kind.to_string()), v.trim().to_string());
        }
    }
    Ok(tools
        .into_iter()
        .map(|t| {
            let version = values.remove(&(t.id.clone(), "version".into()));
            let path = values.remove(&(t.id.clone(), "path".into()));
            let size_bytes = if with_size { path.as_deref().and_then(du) } else { None };
            ToolInfo { id: t.id, version, path, packages: Vec::new(), size_bytes }
        })
        .collect())
}

/// Versión, carpeta y paquetes de las herramientas indicadas (o de todas las instaladas).
#[cfg(unix)]
pub fn info(ctx: &Ctx, ids: &[String], with_size: bool) -> Result<Vec<ToolInfo>> {
    let tools: Vec<Tool> = if ids.is_empty() {
        status(ctx).into_iter().filter(|s| s.installed).map(|s| s.tool).collect()
    } else {
        resolve_exact(ids)?
    };
    // Un solo bash para todos los comandos; cada uno con su propio límite de tiempo.
    let mut script = String::from(INFO_PREAMBLE);
    for t in &tools {
        for (kind, cmd) in [("version", &t.version), ("path", &t.path)] {
            if !cmd.trim().is_empty() {
                let quoted = cmd.replace('\'', r"'\''");
                script.push_str(&format!(
                    "printf '%s\\t{kind}\\t%s\\n' '{}' \"$(timeout 5 bash -c '{quoted}' 2>/dev/null | head -1)\"\n",
                    t.id
                ));
            }
        }
    }
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HOME", &ctx.id.home)
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut values: BTreeMap<(String, String), String> = BTreeMap::new();
    for line in out.lines() {
        let mut f = line.splitn(3, '\t');
        if let (Some(id), Some(kind), Some(v)) = (f.next(), f.next(), f.next())
            && !v.trim().is_empty()
        {
            values.insert((id.to_string(), kind.to_string()), v.trim().to_string());
        }
    }
    let all_pkgs: Vec<String> = tools.iter().flat_map(|t| t.packages.clone()).collect();
    let dpkg = dpkg_info(&all_pkgs);

    Ok(tools
        .into_iter()
        .map(|t| {
            let packages: Vec<PackageInfo> = t
                .packages
                .iter()
                .filter_map(|p| dpkg.get(p).map(|(v, _)| PackageInfo { name: p.clone(), version: v.clone() }))
                .collect();
            let version = values
                .remove(&(t.id.clone(), "version".into()))
                .or_else(|| packages.first().map(|p| upstream(&p.version)));
            let path = values.remove(&(t.id.clone(), "path".into()));
            let size_bytes = with_size.then(|| {
                let from_pkgs: u64 = t.packages.iter().filter_map(|p| dpkg.get(p).map(|(_, s)| *s)).sum();
                if from_pkgs > 0 { Some(from_pkgs) } else { path.as_deref().and_then(du) }
            });
            ToolInfo { id: t.id, version, path, packages, size_bytes: size_bytes.flatten() }
        })
        .collect())
}

/// Como `resolve`, pero sin agregar requisitos.
fn resolve_exact(ids: &[String]) -> Result<Vec<Tool>> {
    let all = catalog();
    for id in ids {
        if !all.iter().any(|t| &t.id == id) {
            bail!("Herramienta desconocida: '{id}'");
        }
    }
    Ok(all.into_iter().filter(|t| ids.contains(&t.id)).collect())
}

#[cfg(all(test, unix))]
mod info_tests {
    use super::upstream;

    #[test]
    fn version_de_apt_limpia() {
        assert_eq!(upstream("1:1.2.95.453.g0eeebbed"), "1.2.95.453.g0eeebbed");
        assert_eq!(upstream("154.0.8037.97-1"), "154.0.8037.97");
        assert_eq!(upstream("10.0.112-0ubuntu1~26.04.1"), "10.0.112");
        assert_eq!(upstream("7:8.0.1-3ubuntu2"), "8.0.1");
        assert_eq!(upstream("4215"), "4215");
    }
}
