//! `cheka daemon` en Windows: será un servicio de Windows con un named pipe (fase C del
//! port, docs/ARQUITECTURA.md §8.6). Mientras tanto, los cambios se aplican con
//! `cheka refresh` desde una terminal de administrador.

use anyhow::{Result, bail};

use crate::Ctx;

pub fn run(_ctx: &Ctx) -> Result<()> {
    bail!("El daemon de cheka todavía no está disponible en Windows; usa 'cheka refresh' como administrador")
}
