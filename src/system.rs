//! Servicios del sistema. `Systemd` es el real; `Inert` se usa en modo prueba
//! (CHEKA_PREFIX) y responde "todo bien" como el `sc()` de bash.

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
