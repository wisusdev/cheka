//! Primitivas que dependen del sistema operativo. El resto del núcleo solo usa estas
//! funciones, así que portar cheka a otra plataforma empieza aquí (docs/ARQUITECTURA.md §8.3).

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

/// Datos de una cuenta del sistema. En Windows `uid`/`gid` valen 0 y `group` va vacío:
/// los archivos no cambian de dueño.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub group: String,
    pub home: std::path::PathBuf,
}
