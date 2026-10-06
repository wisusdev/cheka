//! Panel y bandeja de cheka (Tauri). Es otro cliente del núcleo, igual que la CLI: usa el
//! `cheka` instalado como motor (JSON para leer, los mismos comandos para modificar), así
//! que se comporta exactamente igual que la terminal. Las acciones de root pasan por
//! `pkexec` (diálogo gráfico de contraseña).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WindowEvent, Wry};
use tauri_plugin_opener::OpenerExt;

/// Comandos que la UI puede ejecutar como el usuario.
const USER_COMMANDS: &[&str] = &[
    "secure", "unsecure", "isolate", "unisolate", "use", "docroot", "link", "unlink", "refresh", "park", "forget",
    "php:ini", "php:ext",
];
/// Comandos que requieren root (vía pkexec).
const ROOT_COMMANDS: &[&str] = &["start", "stop", "restart", "php:install", "php:update", "php:ext"];
const LOG_DIR: &str = "/var/log/cheka";
const TRAY_ID: &str = "cheka";

/// Nombres de sitio de la bandeja actual, para no reconstruir el menú si no cambió.
#[derive(Default)]
struct TrayState(Mutex<Vec<(String, String)>>);

#[derive(Serialize)]
struct CmdOut {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Binario de cheka: `CHEKA_BIN` (desarrollo) o el instalado.
fn cheka_bin() -> PathBuf {
    if let Some(b) = std::env::var_os("CHEKA_BIN") {
        return PathBuf::from(b);
    }
    PathBuf::from("/usr/local/bin/cheka")
}

fn run_cheka(args: &[String], cwd: Option<&Path>) -> Result<CmdOut, String> {
    let mut cmd = Command::new(cheka_bin());
    cmd.args(args).stdin(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().map_err(|e| format!("No pude ejecutar cheka: {e}"))?;
    Ok(CmdOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn json(args: &[&str]) -> Result<Value, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = run_cheka(&args, None)?;
    if !out.ok {
        return Err(out.stderr.trim().to_string());
    }
    serde_json::from_str(&out.stdout).map_err(|e| format!("Respuesta inesperada de cheka: {e}"))
}

fn allowed(args: &[String], list: &[&str]) -> Result<(), String> {
    match args.first() {
        Some(c) if list.contains(&c.as_str()) => Ok(()),
        _ => Err(format!("Comando no permitido desde la UI: {:?}", args.first())),
    }
}

fn valid_site(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

// ------------------------------------------------------------------ lectura ----

#[tauri::command]
async fn sites(app: AppHandle) -> Result<Value, String> {
    let value = json(&["sites", "--json"])?;
    update_tray(&app, &value);
    Ok(value)
}

#[tauri::command]
async fn versions() -> Result<Value, String> {
    json(&["versions", "--json"])
}

#[tauri::command]
async fn status() -> Result<Value, String> {
    json(&["status", "--json"])
}

fn valid_version(v: &str) -> bool {
    v.len() <= 4 && v.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Ajustes, extensiones y archivos de una versión de PHP.
#[tauri::command]
async fn php_info(version: String) -> Result<Value, String> {
    if !valid_version(&version) {
        return Err("Versión inválida".into());
    }
    json(&["php:info", &version, "--json"])
}

/// Versiones instaladas con actualización disponible (consulta la red).
#[tauri::command]
async fn php_updates() -> Result<Value, String> {
    json(&["php:updates", "--json"])
}

/// Últimas líneas de los logs de Apache y PHP de un sitio.
#[tauri::command]
async fn read_log(site: String, php: String) -> Result<Value, String> {
    if !valid_site(&site) || !php.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err("Sitio o versión inválidos".into());
    }
    let tail = |file: String| -> Value {
        let path = Path::new(LOG_DIR).join(&file);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(200);
                serde_json::json!({ "file": path.display().to_string(), "lines": lines[start..] })
            }
            Err(_) => serde_json::json!({ "file": path.display().to_string(), "lines": [] }),
        }
    };
    Ok(serde_json::json!({
        "apache": tail(format!("{site}-error.log")),
        "php": tail(format!("php-{php}-errors.log")),
    }))
}

// ------------------------------------------------------------------ acciones ----

/// Comando de cheka como el usuario (lista blanca).
#[tauri::command]
async fn run(args: Vec<String>) -> Result<CmdOut, String> {
    allowed(&args, USER_COMMANDS)?;
    // Instalar extensiones necesita root: va por run_root (pkexec).
    if args.first().is_some_and(|c| c == "php:ext") && args.get(2).is_some_and(|a| a == "install") {
        return Err("Instalar extensiones requiere permisos de administrador".into());
    }
    run_cheka(&args, None)
}

/// `cheka link <nombre>` desde la carpeta indicada.
#[tauri::command]
async fn link_folder(path: String, name: String) -> Result<CmdOut, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(format!("No existe la carpeta {path}"));
    }
    let mut args = vec!["link".to_string()];
    if !name.trim().is_empty() {
        args.push(name.trim().to_string());
    }
    run_cheka(&args, Some(&dir))
}

/// Comando de cheka como root, con el diálogo gráfico de pkexec (lista blanca).
#[tauri::command]
async fn run_root(args: Vec<String>) -> Result<CmdOut, String> {
    allowed(&args, ROOT_COMMANDS)?;
    // De php:ext, como root solo se permite instalar.
    if args.first().is_some_and(|c| c == "php:ext") && args.get(2).is_none_or(|a| a != "install") {
        return Err("Solo la instalación de extensiones requiere permisos de administrador".into());
    }
    let out = Command::new("pkexec")
        .arg(cheka_bin())
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("No pude ejecutar pkexec: {e}"))?;
    let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // 126/127: el usuario canceló el diálogo o no se autorizó
    if matches!(out.status.code(), Some(126) | Some(127)) && stderr.trim().is_empty() {
        stderr = "Se canceló la autorización".into();
    }
    Ok(CmdOut { ok: out.status.success(), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr })
}

#[derive(Clone, Serialize)]
struct OutputLine {
    line: String,
    error: bool,
}

/// `cheka new …`: corre en segundo plano y envía la salida línea a línea
/// (evento `new-output`) y el resultado al terminar (evento `new-done`).
#[tauri::command]
async fn new_project(app: AppHandle, args: Vec<String>) -> Result<(), String> {
    let mut child = Command::new(cheka_bin())
        .arg("new")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("No pude ejecutar cheka: {e}"))?;
    let forward = |stream: Box<dyn std::io::Read + Send>, error: bool, app: AppHandle| {
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = app.emit("new-output", OutputLine { line, error });
            }
        })
    };
    let out = forward(Box::new(child.stdout.take().unwrap()), false, app.clone());
    let err = forward(Box::new(child.stderr.take().unwrap()), true, app.clone());
    std::thread::spawn(move || {
        let ok = child.wait().is_ok_and(|s| s.success());
        let _ = out.join();
        let _ = err.join();
        let _ = app.emit("new-done", ok);
    });
    Ok(())
}

#[tauri::command]
async fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("URL inválida".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    if !Path::new(&path).is_absolute() {
        return Err("Ruta inválida".into());
    }
    app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------- bandeja ----

fn tray_menu(app: &AppHandle, sites: &[(String, String)]) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "open", "Abrir panel", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let list = Submenu::new(app, "Sitios", true)?;
    if sites.is_empty() {
        list.append(&MenuItem::new(app, "Sin sitios todavía", false, None::<&str>)?)?;
    }
    for (name, url) in sites {
        list.append(&MenuItem::with_id(app, format!("site:{url}"), name, true, None::<&str>)?)?;
    }
    menu.append(&list)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "restart", "Reiniciar servicios", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?)?;
    Ok(menu)
}

fn update_tray(app: &AppHandle, value: &Value) {
    let sites: Vec<(String, String)> = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s["name"].as_str()?.to_string(), s["url"].as_str()?.to_string())))
        .collect();
    let state = app.state::<TrayState>();
    let mut current = state.0.lock().unwrap();
    if *current == sites {
        return;
    }
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), tray_menu(app, &sites)) {
        let _ = tray.set_menu(Some(menu));
        *current = sites;
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(TrayState::default())
        .invoke_handler(tauri::generate_handler![
            sites,
            versions,
            status,
            php_info,
            php_updates,
            read_log,
            run,
            link_folder,
            run_root,
            new_project,
            open_url,
            open_path
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .tooltip("cheka")
                .menu(&tray_menu(&handle, &[])?)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main(app),
                    "quit" => app.exit(0),
                    "restart" => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let ok = Command::new("pkexec")
                                .arg(cheka_bin())
                                .arg("restart")
                                .status()
                                .is_ok_and(|s| s.success());
                            let _ = app.emit("services-restarted", ok);
                        });
                    }
                    id => {
                        if let Some(url) = id.strip_prefix("site:") {
                            let _ = app.opener().open_url(url, None::<&str>);
                        }
                    }
                })
                .build(app)?;
            // Primer llenado del menú de sitios
            if let Ok(value) = json(&["sites", "--json"]) {
                update_tray(&handle, &value);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Cerrar la ventana la oculta: cheka sigue en la bandeja.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("no pude iniciar el panel de cheka");
}
