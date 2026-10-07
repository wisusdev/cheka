//! `services` y `service <nombre> …`: estado detallado y gestión individual de cada servicio.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::report::{self, service_ids};
use crate::{Ctx, ui};

const ACTIONS: [&str; 5] = ["start", "stop", "restart", "enable", "disable"];

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn human_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", b as f64 / (1u64 << 20) as f64)
    }
}

fn human_duration(s: u64) -> String {
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        _ => format!("{} d {} h", s / 86400, s % 86400 / 3600),
    }
}

/// `services [--json]`
pub fn list(ctx: &Ctx, args: &[String]) -> Result<()> {
    let all = report::services(ctx);
    if args.iter().any(|a| a == "--json") {
        return print_json(&all);
    }
    let c = ui::colors();
    for s in &all {
        let dot = if s.state == "active" { c.green } else { c.red };
        println!("{dot}●{} {}{}{}  ({})", c.reset, c.bold, s.label, c.reset, s.id);
        let mut facts = vec![s.state.clone()];
        if let Some(v) = &s.version {
            facts.push(format!("versión {v}"));
        }
        if let Some(u) = s.uptime_secs {
            facts.push(format!("activo hace {}", human_duration(u)));
        }
        if let Some(m) = s.memory_bytes {
            facts.push(human_bytes(m));
        }
        if let Some(p) = s.pid {
            facts.push(format!("PID {p}"));
        }
        facts.push(if s.enabled { "inicia con el sistema".into() } else { "no inicia con el sistema".into() });
        println!("    {}", facts.join(" · "));
        if !s.listen.is_empty() {
            println!("    escucha: {}", s.listen.join(", "));
        }
    }
    Ok(())
}

fn known(id: &str) -> Result<()> {
    if service_ids().iter().any(|(s, _)| s == id) {
        Ok(())
    } else {
        let ids: Vec<String> = service_ids().into_iter().map(|(s, _)| s).collect();
        bail!("Servicio desconocido: '{id}'. Disponibles: {}", ids.join(", "))
    }
}

#[derive(Serialize)]
struct Logs {
    journal: Vec<String>,
    files: Vec<LogFile>,
}

#[derive(Serialize)]
struct LogFile {
    file: String,
    lines: Vec<String>,
}

fn tail(path: &str, n: usize) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    Some(lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect())
}

/// `service <nombre> start|stop|restart|enable|disable|logs [--json]`
pub fn service(ctx: &Ctx, args: &[String]) -> Result<()> {
    let usage = "Uso: cheka service <nombre> start|stop|restart|enable|disable|logs [--json]";
    let (Some(id), Some(action)) = (args.first(), args.get(1)) else { bail!("{usage}") };
    let id = id.trim_end_matches(".service");
    known(id)?;
    if action == "logs" {
        let detail = report::services(ctx).into_iter().find(|s| s.id == id).unwrap();
        // Windows no tiene journal: todo está en los archivos de log.
        let journal: Vec<String> = if cfg!(windows) {
            Vec::new()
        } else {
            let out = Command::new("journalctl")
                .args(["-u", id, "-n", "150", "--no-pager", "-o", "short-iso"])
                .env("LC_ALL", "C")
                .stderr(Stdio::piped())
                .output()?;
            String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
        };
        let files = detail
            .log_files
            .iter()
            .filter(|f| std::path::Path::new(f).is_file())
            .map(|f| LogFile { file: f.clone(), lines: tail(f, 150).unwrap_or_else(|| vec!["(sin permiso para leerlo)".into()]) })
            .collect();
        let logs = Logs { journal, files };
        if args.iter().any(|a| a == "--json") {
            return print_json(&logs);
        }
        for l in &logs.journal {
            println!("{l}");
        }
        for f in &logs.files {
            println!("\n== {} ==", f.file);
            for l in &f.lines {
                println!("{l}");
            }
        }
        return Ok(());
    }
    if !ACTIONS.contains(&action.as_str()) {
        bail!("{usage}");
    }
    ctx.ensure_root()?;
    let ok = run_action(ctx, action, id);
    let what = match action.as_str() {
        "start" => "iniciado",
        "stop" => "detenido",
        "restart" => "reiniciado",
        "enable" => "iniciará con el sistema",
        _ => "no iniciará con el sistema",
    };
    if !ok {
        bail!("No pude hacer '{action}' en {id} (revisa: cheka service {id} logs)");
    }
    ui::ok(format!("{id}: {what}"));
    if id == "cheka" && action == "stop" {
        let how = if cfg!(windows) { "permisos de administrador" } else { "sudo" };
        ui::warn(format!("Con el daemon detenido, las carpetas nuevas no se publican solas y la CLI pedirá {how}."));
    }
    Ok(())
}

#[cfg(unix)]
fn run_action(_ctx: &Ctx, action: &str, id: &str) -> bool {
    Command::new("systemctl").args([action, id]).status().is_ok_and(|s| s.success())
}

/// Windows: iniciar/detener/reiniciar con PowerShell; activar/desactivar cambia el tipo de
/// inicio (automático o manual) sin tocar si está corriendo.
#[cfg(windows)]
fn run_action(ctx: &Ctx, action: &str, id: &str) -> bool {
    match action {
        "enable" => ctx.sys.enable(id, false).is_ok(),
        "disable" => ctx.sys.disable(id, false).is_ok(),
        other => crate::commands::service_action(other, id),
    }
}
