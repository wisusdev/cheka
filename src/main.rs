use std::ffi::OsString;
use std::process::ExitCode;

use clap::{ArgAction, Parser, Subcommand};

use cheka::{Ctx, commands, ui};

#[derive(Parser)]
#[command(
    name = "cheka",
    version,
    about = "Entorno local PHP al estilo de Laravel Valet",
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
#[allow(clippy::enum_variant_names)]
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
    /// Pasa el estado a cheka.toml (--dry-run: solo mostrar; --legacy: volver al formato de bash)
    Migrate {
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Cada carpeta dentro de dir → <carpeta>.test
    Park { args: Vec<String> },
    /// Deja de aparcar dir
    Forget { args: Vec<String> },
    /// Publica el directorio actual como nombre.test
    Link { args: Vec<String> },
    /// Elimina un enlace
    Unlink { args: Vec<String> },
    /// Versión de PHP para el sitio actual
    #[command(disable_help_flag = true)]
    Isolate {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// El sitio actual vuelve a la versión por defecto
    #[command(disable_help_flag = true)]
    Unisolate {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Versión de PHP por defecto
    Use { args: Vec<String> },
    /// Fuerza la carpeta pública del sitio actual
    Docroot { args: Vec<String> },
    /// HTTPS con certificado local
    Secure { args: Vec<String> },
    /// Vuelve a HTTP
    Unsecure { args: Vec<String> },
    /// Abre el sitio en el navegador
    Open { args: Vec<String> },
    /// Sigue los logs de Apache y PHP del sitio
    Log { args: Vec<String> },
    /// Base de datos: create|drop|list|import|export
    #[command(disable_help_flag = true)]
    Db {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Crea un proyecto: wordpress|laravel|codeigniter|php <nombre>
    #[command(disable_help_flag = true)]
    New {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// WP-CLI con el PHP del sitio actual
    #[command(disable_help_flag = true)]
    Wp {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Configura todo el sistema (pide sudo). Idempotente
    Install,
    /// Revierte la configuración del sistema
    Uninstall {
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Daemon: vigila los sitios y atiende a la CLI (lo arranca systemd)
    Daemon,
    /// Inicia los servicios
    Start,
    /// Detiene los servicios
    Stop,
    /// Reinicia los servicios
    Restart,
    #[command(name = "_fpm", hide = true)]
    Fpm { version: String },
    #[command(external_subcommand)]
    Other(Vec<OsString>),
}

fn run(cmd: Cmd) -> anyhow::Result<()> {
    use commands::{db, new, site};
    let mut ctx = Ctx::load()?;
    let ctx = &mut ctx;
    match cmd {
        Cmd::Refresh { quiet } => commands::refresh(ctx, quiet),
        Cmd::Sites => commands::sites(ctx),
        Cmd::Paths => commands::paths(ctx),
        Cmd::Versions => commands::versions(ctx),
        Cmd::WhichPhp => commands::which_php(ctx),
        Cmd::Php { args } => commands::php(ctx, args),
        Cmd::Composer { args } => commands::composer(ctx, args),
        Cmd::Status => commands::status(ctx),
        Cmd::PhpInstall { version } => commands::php_install(ctx, &version),
        Cmd::Migrate { args } => commands::migrate(ctx, &args),
        Cmd::Fpm { version } => commands::fpm(ctx, &version),
        Cmd::Park { args } => site::park(ctx, &args),
        Cmd::Forget { args } => site::forget(ctx, &args),
        Cmd::Link { args } => site::link(ctx, &args),
        Cmd::Unlink { args } => site::unlink(ctx, &args),
        Cmd::Isolate { args } => site::isolate(ctx, &args),
        Cmd::Unisolate { args } => site::unisolate(ctx, &args),
        Cmd::Use { args } => site::use_php(ctx, &args),
        Cmd::Docroot { args } => site::docroot(ctx, &args),
        Cmd::Secure { args } => site::secure(ctx, &args),
        Cmd::Unsecure { args } => site::unsecure(ctx, &args),
        Cmd::Open { args } => site::open(ctx, &args),
        Cmd::Log { args } => site::log(ctx, &args),
        Cmd::Db { args } => db::db(ctx, &args),
        Cmd::New { args } => new::new(ctx, &args),
        Cmd::Wp { args } => commands::wp(ctx, args),
        Cmd::Install => cheka::install::install(ctx),
        Cmd::Uninstall { args } => cheka::install::uninstall(ctx, &args),
        Cmd::Daemon => cheka::daemon::run(ctx),
        Cmd::Start => commands::services(ctx, "start"),
        Cmd::Stop => commands::services(ctx, "stop"),
        Cmd::Restart => commands::services(ctx, "restart"),
        Cmd::Other(args) => {
            let name = args.first().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
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
            if e.downcast_ref::<cheka::Reported>().is_none() {
                ui::error(format!("{e:#}"));
            }
            ExitCode::FAILURE
        }
    }
}
