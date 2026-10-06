use std::ffi::OsString;
use std::process::ExitCode;

use clap::{ArgAction, Parser, Subcommand};

use cheka::{Ctx, commands, ui};

/// Comandos de la versión en bash que todavía no se portaron.
const PENDING: &[&str] = &[
    "install", "uninstall", "park", "forget", "link", "unlink", "isolate", "unisolate", "use",
    "docroot", "wp", "new", "secure", "unsecure", "open", "log", "db", "start", "stop", "restart",
];

#[derive(Parser)]
#[command(
    name = "cheka",
    version,
    about = "Entorno local PHP al estilo de Laravel Valet (port en Rust, fase 1)",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    version: Option<bool>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenera la configuración de Apache (se hace solo)
    Refresh {
        #[arg(long)]
        quiet: bool,
    },
    /// Lista sitios, tipo detectado, PHP y URL
    #[command(visible_aliases = ["links", "ls"])]
    Sites,
    /// Carpetas aparcadas
    Paths,
    /// Versiones de PHP disponibles e instaladas
    Versions,
    /// Ruta del PHP del sitio actual
    #[command(name = "which-php")]
    WhichPhp,
    /// Ejecuta el PHP del sitio actual
    #[command(disable_help_flag = true)]
    Php {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Composer con el PHP del sitio actual
    #[command(disable_help_flag = true)]
    Composer {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Estado de los servicios
    Status,
    /// Instala (o reconfigura) una versión de PHP
    #[command(name = "php:install")]
    PhpInstall { version: String },
    /// Muestra el estado en el formato TOML futuro
    Migrate,
    #[command(name = "_fpm", hide = true)]
    Fpm { version: String },
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

fn run(cmd: Cmd) -> anyhow::Result<()> {
    let ctx = Ctx::load()?;
    match cmd {
        Cmd::Refresh { quiet } => commands::refresh(&ctx, quiet),
        Cmd::Sites => commands::sites(&ctx),
        Cmd::Paths => commands::paths(&ctx),
        Cmd::Versions => commands::versions(&ctx),
        Cmd::WhichPhp => commands::which_php(&ctx),
        Cmd::Php { args } => commands::php(&ctx, args),
        Cmd::Composer { args } => commands::composer(&ctx, args),
        Cmd::Status => commands::status(&ctx),
        Cmd::PhpInstall { version } => commands::php_install(&ctx, &version),
        Cmd::Migrate => commands::migrate(&ctx),
        Cmd::Fpm { version } => commands::fpm(&ctx, &version),
        Cmd::Other(args) => {
            let name = args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
            if PENDING.contains(&name.as_str()) {
                anyhow::bail!("'{name}' todavía no está portado a Rust; usa la versión en bash: ~/cheka/cheka {name}")
            }
            anyhow::bail!("Comando desconocido: {name} (usa: cheka --help)")
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Some(cmd) = cli.cmd else {
        use clap::CommandFactory;
        let _ = Cli::command().print_help();
        return ExitCode::SUCCESS;
    };
    match run(cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ui::error(format!("{e:#}"));
            ExitCode::FAILURE
        }
    }
}
