//! Servicios del sistema. `Systemd` es el real en Linux y `WindowsServices` en Windows;
//! `Inert` se usa en modo prueba (CHEKA_PREFIX) y responde "todo bien" como el `sc()` de bash.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};

pub trait System {
    fn is_active(&self, unit: &str) -> bool;
    fn is_enabled(&self, unit: &str) -> bool;
    fn enable(&self, unit: &str, now: bool) -> Result<()>;
    fn disable(&self, unit: &str, now: bool) -> Result<()>;
    fn restart(&self, unit: &str) -> Result<()>;
    fn reload(&self, unit: &str) -> Result<()>;
    fn daemon_reload(&self) -> Result<()>;
    /// Valida la configuración de Apache; el error trae la salida de `apache2ctl -t`.
    fn apache_test(&self) -> std::result::Result<(), String>;
}

pub struct Systemd;

fn systemctl(args: &[&str], quiet: bool) -> Result<()> {
    let mut cmd = Command::new("systemctl");
    cmd.args(args);
    if quiet {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let st = cmd.status()?;
    if !st.success() {
        bail!("systemctl {} falló", args.join(" "));
    }
    Ok(())
}

impl System for Systemd {
    fn is_active(&self, unit: &str) -> bool {
        systemctl(&["is-active", "-q", unit], true).is_ok()
    }
    fn is_enabled(&self, unit: &str) -> bool {
        systemctl(&["is-enabled", "-q", unit], true).is_ok()
    }
    fn enable(&self, unit: &str, now: bool) -> Result<()> {
        if now { systemctl(&["enable", "--now", unit], true) } else { systemctl(&["enable", unit], true) }
    }
    fn disable(&self, unit: &str, now: bool) -> Result<()> {
        if now { systemctl(&["disable", "--now", unit], true) } else { systemctl(&["disable", unit], true) }
    }
    fn restart(&self, unit: &str) -> Result<()> {
        systemctl(&["restart", unit], false)
    }
    fn reload(&self, unit: &str) -> Result<()> {
        systemctl(&["reload", unit], false)
    }
    fn daemon_reload(&self) -> Result<()> {
        systemctl(&["daemon-reload"], false)
    }
    fn apache_test(&self) -> std::result::Result<(), String> {
        match Command::new("apache2ctl").arg("-t").output() {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                Err(s.trim_end().to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Servicios de Windows (`sc.exe` para consultar, PowerShell para arrancar y detener,
/// porque espera a que el servicio termine de cambiar de estado).
#[cfg(windows)]
pub struct WindowsServices {
    pub httpd: std::path::PathBuf,
}

#[cfg(windows)]
impl WindowsServices {
    pub fn new(layout: &crate::layout::Layout) -> Self {
        Self { httpd: layout.opt.join(r"apache\bin\httpd.exe") }
    }
}

#[cfg(windows)]
fn sc(args: &[&str]) -> Option<String> {
    let o = Command::new("sc.exe").args(args).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<()> {
    let st = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdout(Stdio::null())
        .status()?;
    if !st.success() {
        bail!("PowerShell falló: {script}");
    }
    Ok(())
}

/// Estado de un servicio de Windows con los nombres de systemd (active, inactive…),
/// o `None` si no existe.
#[cfg(windows)]
pub fn windows_service_state(name: &str) -> Option<&'static str> {
    let out = sc(&["query", name])?;
    Some(if out.contains("RUNNING") {
        "active"
    } else if out.contains("START_PENDING") {
        "activating"
    } else if out.contains("STOP_PENDING") {
        "deactivating"
    } else {
        "inactive"
    })
}

#[cfg(windows)]
impl System for WindowsServices {
    fn is_active(&self, unit: &str) -> bool {
        windows_service_state(unit) == Some("active")
    }
    fn is_enabled(&self, unit: &str) -> bool {
        sc(&["qc", unit]).is_some_and(|o| o.contains("AUTO_START"))
    }
    fn enable(&self, unit: &str, now: bool) -> Result<()> {
        if sc(&["config", unit, "start=", "auto"]).is_none() {
            bail!("No pude activar el servicio {unit}");
        }
        if now { powershell(&format!("Start-Service -Name '{unit}'")) } else { Ok(()) }
    }
    fn disable(&self, unit: &str, now: bool) -> Result<()> {
        if now {
            powershell(&format!("Stop-Service -Name '{unit}'"))?;
        }
        if sc(&["config", unit, "start=", "demand"]).is_none() {
            bail!("No pude desactivar el servicio {unit}");
        }
        Ok(())
    }
    fn restart(&self, unit: &str) -> Result<()> {
        powershell(&format!("Restart-Service -Name '{unit}'"))
    }
    fn reload(&self, unit: &str) -> Result<()> {
        self.restart(unit)
    }
    fn daemon_reload(&self) -> Result<()> {
        Ok(())
    }
    fn apache_test(&self) -> std::result::Result<(), String> {
        match Command::new(&self.httpd).arg("-t").output() {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                Err(s.trim_end().to_string())
            }
            Err(e) => Err(format!("{}: {e}", self.httpd.display())),
        }
    }
}

pub struct Inert;

impl System for Inert {
    fn is_active(&self, _: &str) -> bool {
        true
    }
    fn is_enabled(&self, _: &str) -> bool {
        true
    }
    fn enable(&self, _: &str, _: bool) -> Result<()> {
        Ok(())
    }
    fn disable(&self, _: &str, _: bool) -> Result<()> {
        Ok(())
    }
    fn restart(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn reload(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn daemon_reload(&self) -> Result<()> {
        Ok(())
    }
    fn apache_test(&self) -> std::result::Result<(), String> {
        Ok(())
    }
}
