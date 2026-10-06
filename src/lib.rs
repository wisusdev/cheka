//! Núcleo de cheka. La CLI (`main.rs`) es una capa delgada sobre esta biblioteca, para
//! que el futuro daemon y la UI de bandeja reutilicen la misma lógica.

pub mod commands;
pub mod detect;
pub mod identity;
pub mod layout;
pub mod php;
pub mod refresh;
pub mod render;
pub mod sites;
pub mod state;
pub mod system;
pub mod ui;
pub mod util;

use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::{Result, anyhow};
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
