//! Núcleo de cheka. La CLI (`main.rs`) es una capa delgada sobre esta biblioteca, para
//! que el futuro daemon y la UI de bandeja reutilicen la misma lógica.

pub mod commands;
pub mod daemon;
pub mod detect;
pub mod identity;
pub mod install;
pub mod ipc;
pub mod layout;
pub mod php;
pub mod phpconf;
pub mod refresh;
pub mod render;
pub mod report;
pub mod sites;
pub mod state;
pub mod system;
pub mod tools;
pub mod ui;
pub mod userfs;
pub mod util;

use std::fmt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, anyhow};

/// Error que ya se mostró al usuario (main solo debe salir con código 1).
#[derive(Debug)]
pub struct Reported;

impl fmt::Display for Reported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(ya informado)")
    }
}

impl std::error::Error for Reported {}
use nix::unistd::geteuid;

use identity::Identity;
use layout::Layout;
use state::State;
use system::System;

/// Todo lo que un comando necesita saber del entorno.
pub struct Ctx {
    pub layout: Layout,
    pub id: Identity,
    pub state: State,
    pub sys: Box<dyn System>,
}

impl Ctx {
    pub fn load() -> Result<Self> {
        let layout = Layout::from_env();
        let id = Identity::resolve(&layout)?;
        let state = State::load(&id.conf)?;
        let sys: Box<dyn System> = if layout.is_test() { Box::new(system::Inert) } else { Box::new(system::Systemd) };
        Ok(Self { layout, id, state, sys })
    }

    /// Root de verdad, o modo prueba (que nunca necesita privilegios).
    pub fn is_root(&self) -> bool {
        geteuid().is_root() || self.layout.is_test()
    }

    /// Relee el estado del usuario después de modificarlo.
    pub fn reload_state(&mut self) -> Result<()> {
        self.state = State::load(&self.id.conf)?;
        Ok(())
    }

    /// Directorio actual "lógico" (`$PWD` de bash: conserva los symlinks del camino).
    pub fn cwd(&self) -> Result<PathBuf> {
        let real = std::env::current_dir()?;
        Ok(std::env::var_os("PWD")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && std::fs::canonicalize(p).ok() == std::fs::canonicalize(&real).ok())
            .unwrap_or(real))
    }

    /// Ejecuta este mismo binario como root (`sudo cheka …`), o directamente si ya lo somos.
    pub fn run_as_root(&self, args: &[&str]) -> Result<()> {
        let exe = std::env::current_exe()?;
        let status = if self.is_root() {
            Command::new(exe).args(args).status()?
        } else {
            Command::new("sudo").arg("--").arg(exe).args(args).status()?
        };
        if !status.success() {
            return Err(Reported.into());
        }
        Ok(())
    }

    /// Si no somos root, vuelve a ejecutar el mismo comando con sudo (no regresa).
    pub fn ensure_root(&self) -> Result<()> {
        if self.is_root() {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        let err = Command::new("sudo").arg("--").arg(exe).args(std::env::args_os().skip(1)).exec();
        Err(anyhow!("No pude ejecutar sudo: {err}"))
    }
}
