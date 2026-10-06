//! `cheka tools`: lista e instala herramientas del catálogo (tools/linux.toml).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use nix::unistd::geteuid;

use crate::tools::{self, Tool};
use crate::{Ctx, ui};

fn ids_of(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| !a.starts_with("--")).cloned().collect()
}

/// `tools [--json]`
pub fn list(ctx: &Ctx, args: &[String]) -> Result<()> {
    let all = tools::status(ctx);
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    let c = ui::colors();
    let mut category = "";
    for s in &all {
        if s.tool.category != category {
            category = &s.tool.category;
            println!("\n{}{category}{}", c.bold, c.reset);
        }
        let mark = if s.installed { format!("{}✔{}", c.green, c.reset) } else { " ".into() };
        println!("  {mark} {:<16} {}", s.tool.id, s.tool.name);
    }
    println!("\nInstala con: cheka tools install <id> [<id>…]");
    Ok(())
}

/// `tools _root <ids…>` (root): pasos de root, con una línea marcadora por herramienta.
pub fn run_root(ctx: &Ctx, args: &[String]) -> Result<()> {
    if !ctx.is_root() {
        bail!("Uso interno: lo ejecuta 'cheka tools install' con sudo");
    }
    for t in tools::resolve(&ids_of(args))? {
        let ok = t.root.trim().is_empty() || {
            println!("\n==> {} (sistema)", t.name);
            tools::run_step(ctx, &t, &t.root)?
        };
        println!("{}", tools::marker(ok, &t.id));
    }
    Ok(())
}

/// `tools _user <ids…>`: pasos del usuario, con una línea marcadora por herramienta.
pub fn run_user(ctx: &Ctx, args: &[String]) -> Result<()> {
    if geteuid().is_root() && ctx.id.user != "root" {
        bail!("Los pasos del usuario no deben correr como root");
    }
    for t in tools::resolve(&ids_of(args))? {
        let ok = t.user.trim().is_empty() || {
            println!("\n==> {}", t.name);
            tools::run_step(ctx, &t, &t.user)?
        };
        println!("{}", tools::marker(ok, &t.id));
    }
    Ok(())
}

/// Ejecuta `cheka tools _root|_user` mostrando su salida y devuelve el resultado de cada id.
fn run_phase(program: &str, args: &[String]) -> Result<BTreeMap<String, bool>> {
    let mut child = Command::new(program).args(args).stdout(Stdio::piped()).spawn()?;
    let mut results = BTreeMap::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        match tools::parse_marker(&line) {
            Some((id, ok)) => {
                results.insert(id, ok);
            }
            None => println!("{line}"),
        }
    }
    child.wait()?;
    Ok(results)
}

/// `tools install <ids…>`: pide la contraseña una vez para los pasos de root y luego hace
/// los del usuario como él.
pub fn install(args: &[String]) -> Result<()> {
    if geteuid().is_root() {
        bail!("Ejecuta 'cheka tools install' con tu usuario (sin sudo): pedirá la contraseña solo para los pasos que la necesitan");
    }
    let ids = ids_of(args);
    if ids.is_empty() {
        bail!("Uso: cheka tools install <id> [<id>…]   (lista: cheka tools)");
    }
    let selected: Vec<Tool> = tools::resolve(&ids)?;
    let ids: Vec<String> = selected.iter().map(|t| t.id.clone()).collect();
    let exe = std::env::current_exe()?.display().to_string();

    let mut root_ok: BTreeMap<String, bool> = BTreeMap::new();
    if selected.iter().any(|t| !t.root.trim().is_empty()) {
        let mut a = vec!["--".to_string(), exe.clone(), "tools".into(), "_root".into()];
        a.extend(ids.iter().cloned());
        root_ok = run_phase("sudo", &a)?;
    }
    // La parte del usuario solo para lo que no falló como root.
    let user_ids: Vec<String> = ids.iter().filter(|id| root_ok.get(*id).copied().unwrap_or(true)).cloned().collect();
    let mut user_ok = BTreeMap::new();
    if !user_ids.is_empty() {
        let mut a = vec!["tools".to_string(), "_user".into()];
        a.extend(user_ids);
        user_ok = run_phase(&exe, &a)?;
    }

    println!();
    let mut failed = 0;
    for t in &selected {
        let ok = root_ok.get(&t.id).copied().unwrap_or(true) && user_ok.get(&t.id).copied().unwrap_or(false);
        if ok {
            ui::ok(&t.name);
        } else {
            failed += 1;
            ui::error(format!("{} (revisa la salida de arriba)", t.name));
        }
    }
    if selected.iter().any(|t| t.user.contains("add_path") || t.user.contains("add_env")) {
        ui::info("Abre una terminal nueva (o ejecuta: source ~/.bashrc) para cargar el PATH.");
    }
    if failed > 0 {
        return Err(crate::Reported.into());
    }
    Ok(())
}

/// `tools plan <ids…>`: lo que se instalaría (con requisitos) y si necesita root. Para la UI.
pub fn plan(args: &[String]) -> Result<()> {
    #[derive(serde::Serialize)]
    struct Step {
        id: String,
        name: String,
        needs_root: bool,
    }
    let steps: Vec<Step> = tools::resolve(&ids_of(args))?
        .into_iter()
        .map(|t| Step { needs_root: !t.root.trim().is_empty(), id: t.id, name: t.name })
        .collect();
    println!("{}", serde_json::to_string_pretty(&steps)?);
    Ok(())
}

pub fn tools(ctx: &Ctx, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("plan") => plan(&args[1..]),
        None | Some("list") | Some("--json") => list(ctx, args),
        Some("install") => install(&args[1..]),
        Some("_root") => run_root(ctx, &args[1..]),
        Some("_user") => run_user(ctx, &args[1..]),
        Some(other) => bail!("Subcomando desconocido: {other} (usa: cheka tools [list|install <id>…])"),
    }
}
