//! Quién es el dueño de los proyectos y dónde vive su configuración.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use nix::unistd::{Group, Uid, User, geteuid};

use crate::layout::Layout;

#[derive(Debug, Clone)]
pub struct Identity {
    pub user: String,
    pub uid: Uid,
    pub group: String,
    pub home: PathBuf,
    /// `~/.config/cheka` (o `CHEKA_CONF`).
    pub conf: PathBuf,
    pub wpcli: PathBuf,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

impl Identity {
    /// Misma prioridad que bash: CHEKA_USER → SUDO_USER (si somos root) →
    /// /etc/cheka/user (si somos root, p. ej. desde un servicio) → usuario actual.
    pub fn resolve(layout: &Layout) -> Result<Self> {
        let root = geteuid().is_root();
        let user_file = layout.etc.join("user");
        let name = if let Some(u) = env_nonempty("CHEKA_USER") {
            u
        } else if let Some(u) = env_nonempty("SUDO_USER").filter(|u| root && u != "root") {
            u
        } else if root && user_file.is_file() {
            std::fs::read_to_string(&user_file)?.trim_end_matches('\n').to_string()
        } else {
            User::from_uid(geteuid())?
                .ok_or_else(|| anyhow!("No encuentro el usuario actual"))?
                .name
        };
        let user = User::from_name(&name)?.ok_or_else(|| anyhow!("No existe el usuario '{name}'"))?;
        let group = Group::from_gid(user.gid)?
            .map(|g| g.name)
            .with_context(|| format!("No encuentro el grupo de '{name}'"))?;
        let home = user.dir;
        let conf = env_nonempty("CHEKA_CONF")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config/cheka"));
        Ok(Self {
            wpcli: home.join(".local/share/cheka/wp-cli.phar"),
            user: name,
            uid: user.uid,
            group,
            home,
            conf,
        })
    }
}
