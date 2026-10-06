//! `install`/`uninstall` en Windows: descargará Apache Lounge y PHP NTS a
//! `C:\ProgramData\cheka`, configurará `mod_fcgid`, el archivo `hosts`, mkcert y MariaDB
//! (fases B y C del port, docs/ARQUITECTURA.md §8.6).

use anyhow::{Result, bail};

use crate::Ctx;

pub fn install(_ctx: &mut Ctx) -> Result<()> {
    bail!("'cheka install' todavía no está disponible en Windows")
}

pub fn uninstall(_ctx: &mut Ctx, _args: &[String]) -> Result<()> {
    bail!("'cheka uninstall' todavía no está disponible en Windows")
}
