# cheka: arquitectura y guía para portarlo

Este documento describe **cómo funciona `cheka` por dentro** (la versión 0.1.0 en bash para
Ubuntu) y propone **cómo reescribirlo en Rust** para que funcione en Linux, macOS y Windows.

La idea es que sirva como especificación: si una reimplementación cumple los
[contratos](#7-contratos-que-una-reimplementación-debe-cumplir) de la sección 7, se comporta
igual que el script actual.

> Las afirmaciones sobre macOS y Windows marcadas con **(verificar)** no se probaron. Lo marcado
> con **(verificado)** se comprobó al escribir este documento (octubre de 2026).
>
> **Estado:** las fases 1 (Linux en Rust) y 3 (Windows, con paridad 1:1) están terminadas; la
> 2 (macOS) sigue pendiente. El avance de cada hito está en §8.6.

---

## 1. Qué es cheka

`cheka` no es un servidor. **Orquesta componentes que ya existen**: lee el estado del usuario
(qué carpetas son sitios, qué versión de PHP usa cada uno, cuáles van con HTTPS), genera la
configuración de cada componente y los mantiene sincronizados.

```
                ┌─────────────── navegador ───────────────┐
                │  http(s)://blog.test / tienda.blog.test  │
                └───────────────┬─────────────────────────┘
       DNS *.test               │ HTTP :80 / :443
  ┌─────────────────┐   ┌───────▼────────────────────────────┐
  │ systemd-resolved│   │ Apache 2.4 (mpm_event, usuario)     │
  │  ~test →        │   │  vhost por sitio (generado)         │
  │ dnsmasq :5300   │   │  .htaccess del proyecto             │
  │  *.test→127.0.0.1   └───────┬────────────────────────────┘
  └─────────────────┘           │ FastCGI (socket Unix por versión)
                        ┌───────▼───────┐ ┌──────────────┐
                        │ PHP-FPM 8.5   │ │ PHP-FPM 8.2  │ …   (cheka-php@<v>)
                        └───────┬───────┘ └──────┬───────┘
                                └──────┬─────────┘
                                ┌──────▼──────┐
                                │  MariaDB    │
                                └─────────────┘

  Control:  CLI cheka ──(archivo de petición)──▶ cheka-watch.path ──▶ cheka refresh (root)
            ~/Sites cambia ──────────────────────▶ cheka-watch.path ──▶ cheka refresh
            cada minuto ─────────────────────────▶ cheka-refresh.timer ─▶ cheka refresh
```

### Decisiones de diseño y su motivo

| Decisión | Motivo |
|---|---|
| **Apache** y no nginx | WordPress, CodeIgniter 3 y muchos proyectos heredados dependen de `.htaccess`. Con Apache funcionan sin traducir reglas, que es justo donde Valet (nginx) suele fallar. |
| **PHP-FPM por versión** + `SetHandler` por vhost | Permite una versión distinta por proyecto sin contenedores. |
| **Apache y PHP corren con el usuario** | Se eliminan los problemas de permisos en `storage/`, `wp-content/uploads`, etc. Es aceptable en una máquina de desarrollo. |
| **dnsmasq solo para `~test`** | Se resuelven comodines (`*.sitio.test` para Multisite) sin tocar el DNS del resto del sistema. |
| **Puerto DNS 5300** | El 53 ya lo usa systemd-resolved y el 5353 es de mDNS (Avahi). |
| **Estado del usuario en archivos sueltos** | Es fácil de leer y escribir desde bash sin dependencias. En Rust conviene un solo TOML (ver §8.4). |
| **"Refresh" declarativo** | Todo se regenera desde el estado; nunca se edita la configuración de forma incremental. Así el resultado es idempotente y fácil de razonar. |
| **Binarios estáticos de PHP** (static-php-cli) | En Ubuntu 26.04 no hay PPA con versiones antiguas. Los binarios estáticos no dependen de la distribución. |

---

## 2. Estado y archivos

### 2.1 Estado del usuario: `~/.config/cheka/` (la fuente de verdad)

> Desde la versión 0.2 (Rust) el estado vive en un solo archivo, `cheka.toml` (formato en
> §8.4); los certificados siguen en `certs/`. La tabla describe el formato de la versión en
> bash, que Rust todavía lee y migra; tras migrar, esos archivos quedan en `legacy/`.

| Ruta | Contenido | Escrito por |
|---|---|---|
| `config` | Líneas `clave=valor`; hoy solo `default_php=8.5` | `use`, `install` |
| `paths` | Una carpeta aparcada por línea | `park`, `forget` |
| `links/<sitio>` | **Symlink** a la carpeta del proyecto | `link`, `unlink` |
| `isolated/<sitio>` | Versión de PHP (`8.2`) | `isolate`, `unisolate`, `new --php` |
| `secured/<sitio>` | Marcador vacío: el sitio usa HTTPS | `secure`, `unsecure` |
| `certs/<sitio>.test.pem`, `…-key.pem` | Certificado de mkcert (`sitio.test` + `*.sitio.test`) | `secure` |
| `docroot/<sitio>` | Subcarpeta pública forzada (`web`, `htdocs`…) | `docroot` |
| `.refresh-request` | Marca de tiempo; escribirlo pide un refresh (ver §3.2) | cualquier comando que modifique el estado |

Otros: `~/.local/share/cheka/wp-cli.phar` y la CA de mkcert en `~/.local/share/mkcert/`.

### 2.2 Estado del sistema (generado, se puede reconstruir)

| Ruta | Contenido |
|---|---|
| `/usr/local/bin/cheka` | Copia instalada del script |
| `/usr/local/bin/php8.X` | Envoltorios de CLI para los PHP estáticos |
| `/etc/cheka/user` | Usuario dueño de los proyectos; lo usan los servicios que corren como root |
| `/etc/cheka/php/<v>/php-fpm.conf`, `php.ini`, `conf.d/` | Configuración de cada PHP. `conf.d/` es del usuario y no se sobrescribe. |
| `/opt/cheka/php/<v>/{php-fpm,php,VERSION}` | Binarios estáticos |
| `/etc/apache2/cheka/sites/<sitio>.conf` | Un vhost por sitio |
| `/etc/apache2/sites-available/cheka.conf` | `IncludeOptional` de los vhosts y el comodín `*.test` |
| `/etc/apache2/conf-available/cheka.conf` | `ServerName localhost`, `ProxyTimeout` |
| `/etc/apache2/envvars` | Bloque `# >>> cheka … # <<< cheka` con `APACHE_RUN_USER/GROUP` |
| `/etc/systemd/system/cheka-*.{service,path,timer}` | Unidades (ver §4) |
| `/etc/systemd/resolved.conf.d/cheka.conf` | `DNS=127.0.0.1:5300`, `Domains=~test` |
| `/run/cheka/php-<v>/fpm.sock` | Socket de cada PHP-FPM |
| `/run/cheka/last-refresh` | Resultado del último refresh (texto, `ERROR: …` si falló) |
| `/var/log/cheka/` | `<sitio>-error.log` (Apache), `php-<v>-errors.log`, `php-<v>-fpm.log` |

---

## 3. Flujos principales

### 3.1 `refresh`: el núcleo

Corre como root y está serializado con `flock` en `/run/cheka/refresh.lock`.

```
1. sitios = carpetas dentro de cada ruta de `paths`  ∪  links/*   (los links tienen prioridad)
   nombre = minúsculas, [^a-z0-9-] → "-", sin guiones en los extremos
2. para cada sitio:
     (tipo, docroot) = detect(ruta)                 # tabla en el README
     php = isolated/<sitio> o default_php
     si php no está instalado → avisar y usar default_php
     escribir vhost en un directorio temporal
3. asegurar que cada cheka-php@<v> en uso esté activo
4. regenerar cheka-watch.path si cambiaron las rutas aparcadas (daemon-reload + restart)
5. si el directorio temporal es idéntico al actual → "Sin cambios", fin
6. respaldar, reemplazar, `apache2ctl -t`
     falla → restaurar el respaldo, escribir "ERROR: …" en last-refresh, salir con error
     pasa  → `systemctl reload apache2`
7. escribir el resultado en /run/cheka/last-refresh
```

Propiedades que hay que conservar: **idempotente**, **atómico desde el punto de vista de
Apache** (valida antes de recargar y revierte si falla) y **barato cuando no hay cambios**,
porque el temporizador lo ejecuta cada minuto.

### 3.2 Comandos sin sudo (IPC mínimo)

Los comandos del usuario (`link`, `secure`, `isolate`…) solo modifican `~/.config/cheka/` y
después llaman a `request_refresh`:

```
si soy root            → refresh directo
si cheka-watch.path está activo:
    escribir date +%s%N en ~/.config/cheka/.refresh-request
    (systemd detecta el cambio con PathChanged → arranca cheka-refresh.service como root)
    esperar hasta 15 s a que /run/cheka/last-refresh sea más nuevo que la petición
    mostrar su contenido (o fallar si empieza con "ERROR")
si no                  → sudo cheka refresh
```

Es un IPC de pobre, pero evita pedir la contraseña en el uso diario. **En Rust debe
reemplazarse por un socket o named pipe hacia un daemon** (§8.2).

Solo piden sudo `install`, `uninstall`, `start/stop/restart` y descargar una versión de PHP
nueva (`php:install`, que se invoca desde `isolate`, `use` o `new --php`).

### 3.3 `install`

Pasos, todos idempotentes: paquetes de apt → archivos y estado inicial → unidades de systemd →
DNS (dnsmasq y la configuración de resolved) → PHP-FPM del sistema (desactiva el servicio
`phpX.Y-fpm` de Debian y su conf de Apache, con `a2disconf phpX.Y-fpm`) → Apache (módulos, `envvars`, conf,
site) → refresh y reinicio de Apache → watcher y temporizador → usuarios de MariaDB → CA de
mkcert (`TRUST_STORES=system` como root y `TRUST_STORES=nss` como usuario) → verificación de
DNS.

### 3.4 `new`

```
validar tipo, nombre, que la carpeta no exista; db = nombre con - → _
asegurar PHP (descarga con sudo si hace falta); isolated/<sitio> si no es la versión por defecto
mkdir; trap EXIT para avisar si falla a medias
--secure → mkcert antes de instalar, para que la URL guardada sea https
new_<tipo>:
  wordpress  : wp core download --locale → db create → wp config create (+WP_DEBUG…)
               → wp core install | multisite-install [--subdomains] (admin/admin)
               → wp rewrite structure /%postname%/ → escribir .htaccess (3 variantes)
  laravel    : composer create-project → db create → .env (DB_*, APP_URL)
               → borrar database.sqlite → artisan migrate
  codeigniter: composer create-project codeigniter4/appstarter → cp env .env
               → CI_ENVIRONMENT, app.baseURL, database.default.*
  php        : index.php
request_refresh → resumen
```

WP-CLI no escribe el `.htaccess` desde la línea de comandos porque no detecta Apache. Por eso
cheka lo genera: hay una variante para un sitio único, otra para Multisite por subdirectorios
y otra para Multisite por subdominios, copiadas de las reglas oficiales de WordPress.

---

## 4. Plantillas generadas (referencia)

### vhost (por sitio)

```apache
<VirtualHost *:80>
    ServerName {sitio}.test
    ServerAlias *.{sitio}.test
    DocumentRoot "{docroot}"
    <Directory "{ruta_proyecto}">
        Options FollowSymLinks
        AllowOverride All
        Require all granted
    </Directory>
    DirectoryIndex index.php index.html index.htm
    <FilesMatch "\.php$">
        <If "-f %{REQUEST_FILENAME}">
            SetHandler "proxy:unix:/run/cheka/php-{v}/fpm.sock|fcgi://cheka-php-{v}"
        </If>
    </FilesMatch>
    ErrorLog /var/log/cheka/{sitio}-error.log
    # si es seguro: RewriteRule ^ https://%{HTTP_HOST}%{REQUEST_URI} [R=302,L]
</VirtualHost>
# si es seguro: el mismo cuerpo en <VirtualHost *:443> + SSLEngine/SSLCertificate*
```

### Comodín (después de todos los sitios)

```apache
<VirtualHost *:80>
    ServerName no-encontrado.test
    ServerAlias *.test
    RewriteEngine On
    RewriteRule ^ - [R=404,L]
    ErrorDocument 404 "cheka: no hay un sitio para este dominio. Revisa: cheka sites"
</VirtualHost>
```

### Pool de PHP-FPM

`pm = ondemand` (no consume RAM mientras no hay peticiones), `pm.max_children = 10`,
`user`/`listen.owner` = el usuario, socket con modo `0660`, `clear_env = no`,
`request_terminate_timeout = 600`, y errores en `/var/log/cheka/php-<v>-errors.log`.

### php.ini de desarrollo

`memory_limit=512M`, `upload_max_filesize=post_max_size=256M`, `max_execution_time=300`,
`max_input_vars=5000`, `display_errors=On`, `error_reporting=E_ALL`, `date.timezone` del
sistema, `curl.cainfo`/`openssl.cafile` (necesarios en los binarios estáticos),
`mysqli.default_socket`/`pdo_mysql.default_socket = /run/mysqld/mysqld.sock`, y opcache con
`revalidate_freq=0`.

### Unidades de systemd

| Unidad | Función |
|---|---|
| `cheka-php@.service` | `cheka _fpm %i` → `exec php-fpm --nodaemonize --fpm-config … -c …`, `RuntimeDirectory=cheka/php-%i` |
| `cheka-dns.service` | `dnsmasq --keep-in-foreground --no-resolv --no-hosts --listen-address=127.0.0.1 --port=5300 --address=/test/127.0.0.1` |
| `cheka-watch.path` | `PathChanged=` de cada carpeta aparcada y de `.refresh-request` |
| `cheka-refresh.service` | oneshot: `sleep 1; cheka refresh --quiet` |
| `cheka-refresh.timer` | Cada minuto. Corrige detecciones hechas a media clonación. |

---

## 5. Lecciones aprendidas (trampas a no repetir)

Cada punto costó al menos un fallo durante el desarrollo.

1. **El orden de los vhosts importa.** En Apache, el primer vhost cargado es el
   predeterminado. Si los sitios se incluyen antes que `000-default`, cualquier dominio
   desconocido abre el primer proyecto. Por eso los sitios van en `sites-enabled/cheka.conf`,
   que se ordena después de `000-default.conf`, y terminan con el comodín `*.test`.
2. **`fcgi://` debe tener un nombre único por versión** (`fcgi://cheka-php-8.2`). Si no, Apache
   puede reutilizar el *worker* de otro socket.
3. **`<If "-f %{REQUEST_FILENAME}">`** evita mandar a FPM archivos `.php` que no existen. Así
   esas peticiones caen en las reglas de reescritura o en un 404 limpio, y no en *"No input
   file specified"*.
4. **Límite de longitud de los sockets Unix:** 108 bytes en Linux y 104 en macOS. Las rutas de
   socket deben ser cortas; con rutas largas, FPM las trunca en silencio.
5. **Apache tiene `PrivateTmp=yes`** en systemd: los proyectos en `/tmp` dan 403.
6. **Los binarios estáticos de PHP no tienen php.ini.** Sin `curl.cainfo` fallan las
   peticiones HTTPS (actualizaciones de WordPress, Composer), y sin `mysqli.default_socket`
   falla `DB_HOST=localhost`.
7. **`PHP_INI_SCAN_DIR=":/ruta"`:** los dos puntos al inicio conservan el directorio de
   escaneo compilado (las extensiones de apt) y agregan el propio.
8. **systemd-resolved con `Domains=~test`** (dominio solo de enrutamiento) evita que el DNS
   global se use como ruta por defecto. Se verificó que `ubuntu.com` sigue resolviendo.
9. **Bash: `set -e` combinado con una función que termina en `a && b`.** Si `a` falla, la
   función devuelve 1 y el script muere sin mensaje. Pasó en la primera instalación
   ("no habilitado" no es un error). En Rust, modela explícitamente qué estados son normales.
10. **No confíes en un HTTP 200 como prueba de que un sitio existe.** El vhost predeterminado
    también responde 200. Las pruebas deben comprobar el contenido o el vhost generado.
11. **"DNS seguro" de Chrome y Brave** con un proveedor personalizado se salta el resolver del
    sistema, así que `.test` no resuelve.
12. **Laravel nuevo usa SQLite por defecto:** hay que cambiar el `.env` y borrar
    `database/database.sqlite` antes de migrar.
13. **Una unidad `.path` de systemd no aguanta ráfagas.** Cada cambio en `~/Sites` arranca
    `cheka-refresh.service`, y systemd limita los arranques por intervalo (`StartLimitBurst`).
    Con `composer create-project` y varias peticiones seguidas de la CLI se supera el límite
    y `cheka-watch.path` queda en `failed (unit-start-limit-hit)`, sin avisar a nadie. El daemon
    en Rust lo evita agrupando eventos (*debounce*) y sin arrancar un servicio por evento.

---

## 6. Matriz de portabilidad

Cada componente de Linux y su equivalente en las otras plataformas.

| Componente | Linux (actual) | macOS | Windows |
|---|---|---|---|
| **Servidor web** | Apache del sistema (apt) | Homebrew `httpd`, corriendo como LaunchDaemon (root) para usar el puerto 80, con `User`/`Group` = el usuario | [Apache Lounge](https://www.apachelounge.com) (zip, VS18 desde 2026; sha256 en el `.zip.txt` publicado), instalado como servicio con `httpd -k install` **(verificado)** |
| **Ejecutar PHP** | PHP-FPM por versión, socket Unix | PHP-FPM por versión, socket Unix (igual que Linux) | **No hay FPM.** `mod_fcgid` (zip aparte en Apache Lounge) con `FcgidWrapper "C:/…/php-cgi.exe" .php` por vhost. Evita `php-cgi -b`, que atiende una sola petición a la vez. |
| **Binarios de PHP** | apt (versión del sistema) + static-php-cli `bulk` linux (8.0–8.5) | static-php-cli `bulk`: fpm + cli 8.0–8.5, x86_64 y aarch64 **(verificado)**. Para 7.4: tap `shivammathur/php` de Homebrew **(verificar)** | Zips NTS oficiales de windows.php.net. Hay un índice `releases.json` con rutas y sha256, de **7.4 a 8.5** **(verificado)**. static-php-cli solo tiene `cli`/`micro` para Windows, sin fpm ni cgi **(verificado)**. |
| **DNS `*.test`** | dnsmasq :5300 + configuración de systemd-resolved (`~test`) | `/etc/resolver/test` con `nameserver 127.0.0.1` y `port 5300` + dnsmasq de Homebrew o un DNS integrado. Es el método de Valet. | No hay resolver por sufijo como en macOS. **Opción A:** cheka administra el archivo `hosts` (sin comodines, así que cada subsitio de Multisite necesita su propia entrada). **Opción B:** regla NRPT (`Add-DnsClientNrptRule -Namespace ".test" -NameServers 127.0.0.1`) + un DNS integrado en el puerto 53 **(verificar en Windows Home)**. |
| **Servicios** | systemd (`.service`, `.path`, `.timer`) | launchd: plists en `/Library/LaunchDaemons`. `WatchPaths` equivale a `.path`; `StartInterval`, a `.timer`. | Servicio de Windows (el daemon de cheka); Apache y MariaDB ya se instalan como servicios. |
| **Vigilar `~/Sites`** | `cheka-watch.path` + temporizador | `WatchPaths` o el daemon con FSEvents | El daemon con `ReadDirectoryChangesW` |
| **Privilegios** | `sudo` | `sudo` | UAC: solo se eleva `install`; el daemon corre como LocalSystem |
| **IPC sin contraseña** | Archivo `.refresh-request` + `PathChanged` | Socket Unix hacia el daemon | Named pipe hacia el daemon |
| **Usuario de Apache y PHP** | El usuario (`envvars`, `user=` del pool) | El usuario (`User`/`Group` en httpd.conf, `user=` del pool) | El servicio corre como LocalSystem y puede leer los proyectos; no hace falta cambiar de usuario |
| **HTTPS** | mkcert (sistema + NSS) | mkcert (Keychain + NSS) | mkcert (almacén de certificados de Windows). Firefox necesita `security.enterprise_roots.enabled` **(verificar)** |
| **MariaDB** | apt; usuario propio con `unix_socket` | Homebrew `mariadb` | MSI o zip oficial. **No existe `unix_socket`**: solo usuario y contraseña |
| **Enlaces (`link`)** | Symlinks en `links/` | Symlinks | Los symlinks requieren modo desarrollador: guardar los enlaces en el archivo de configuración |
| **Rutas de configuración** | `~/.config/cheka` | `~/Library/Application Support/cheka` | `%APPDATA%\cheka`; sistema en `C:\ProgramData\cheka` |
| **Abrir navegador** | `xdg-open` | `open` | `start` / `ShellExecute` |
| **Rutas en la config de Apache** | `/…` | `/…` | Barras normales: `C:/Users/…` |

**Conclusión:** macOS es casi un calco de Linux: cambian el gestor de servicios, la forma de
configurar el DNS y la instalación de Apache. **Windows es el caso distinto**: usa FastCGI con
`mod_fcgid` en vez de FPM, el DNS es más limitado, no tiene `unix_socket` en MariaDB y los
symlinks son problemáticos. Por eso conviene que la abstracción del "backend de PHP" admita
**destinos FastCGI distintos** desde el principio.

---

## 7. Contratos que una reimplementación debe cumplir

Son las pruebas de aceptación. Todas se ejecutaron contra la versión en bash.

**Sitios y detección**
- [ ] Una carpeta nueva en una ruta aparcada queda accesible en `<nombre>.test` en menos de 5 s y sin intervención.
- [ ] Al borrar la carpeta, el sitio desaparece solo.
- [ ] El nombre se normaliza: `My_Site` → `my-site`.
- [ ] La detección sigue la tabla del README, incluido el docroot de Laravel, CodeIgniter 4 y Bedrock.
- [ ] `*.sitio.test` responde con el mismo sitio (Multisite por subdominios).
- [ ] Un `*.test` desconocido devuelve 404 con un mensaje de cheka; `localhost` no cambia.

**PHP**
- [ ] Dos sitios con versiones de PHP distintas responden al mismo tiempo, cada uno con la suya.
- [ ] `cheka php`, `composer` y `wp` usan la versión del sitio del directorio actual.
- [ ] Los archivos estáticos se sirven sin pasar por PHP; un `.php` inexistente da 404; `index.php/ruta` (PATH_INFO) funciona.
- [ ] PHP corre como el usuario: puede escribir en el proyecto sin `chmod`.

**Comandos y estado**
- [ ] Los comandos diarios (`link`, `secure`, `isolate` a una versión ya instalada, `new`) no piden contraseña.
- [ ] Una configuración inválida nunca deja Apache caído: se revierte y se informa el error.
- [ ] `install` es idempotente y `uninstall` deja el sistema como estaba, sin tocar los proyectos.

**HTTPS y proyectos nuevos**
- [ ] `secure`: HTTPS válido sin advertencias para `sitio.test` y `*.sitio.test`; `http` redirige a `https`.
- [ ] `new wordpress --multisite=subdominios --secure`: sitio en el idioma configurado, enlaces permanentes y un subsitio creado con WP-CLI que responde 200.
- [ ] `new laravel`: tablas migradas en MariaDB y página de inicio 200.
- [ ] `new codeigniter`: página de bienvenida 200 y `spark db:table` se conecta.

---

## 8. Propuesta: cheka en Rust

### 8.1 Por qué Rust (y la alternativa)

Rust encaja bien: produce **un solo binario por plataforma, sin runtime**, y tiene buenas
bibliotecas (crates) para cada pieza: CLI, vigilancia de archivos, DNS, servicios de Windows,
HTTP y descompresión. Su tipado obliga a modelar los estados ("no habilitado" frente a
"error", lección 9 de §5).

**Go** sería una alternativa igual de válida y quizá más rápida de escribir (compilación
cruzada trivial y concurrencia sencilla para el daemon). La arquitectura de abajo sirve para
cualquiera de los dos. Si ya te inclinas por Rust, no hay razón técnica para cambiar.

Ojo: Rust **no elimina** las dependencias externas (Apache, PHP, MariaDB, mkcert). cheka
sigue siendo un orquestador; lo que gana es portabilidad, un daemon y un IPC de verdad.

### 8.2 Arquitectura

```
cheka  (un solo binario)
├── cli/        clap: comandos del usuario. Nunca se eleva, salvo install/uninstall/php install.
├── core/       sin dependencias de plataforma:
│   ├── state     carga y guarda el TOML (§8.4) y migra el estado de la versión en bash
│   ├── sites     enumeración, normalización de nombres, resolución por directorio actual
│   ├── detect    reglas de detección (tabla del README) → ProjectKind + docroot
│   ├── render    plantillas (minijinja) de vhost, ini, pool y comodín → archivos
│   ├── refresh   diff → validar → aplicar → revertir (§3.1)
│   ├── scaffold  new wordpress|laravel|codeigniter|php
│   └── db        create/drop/import/export (llama a mariadb/mariadb-dump o usa un driver)
├── daemon/     `cheka daemon`, corre privilegiado como servicio:
│   ├── ipc       socket Unix o named pipe: recibe {Refresh, Status, …} y responde
│   ├── watch     crate notify sobre las rutas aparcadas (con debounce)
│   ├── timer     refresh periódico
│   └── dns       (opcional) servidor DNS integrado con hickory-server: *.test → 127.0.0.1
└── platform/   linux.rs · macos.rs · windows.rs   (implementan los traits de abajo)
```

El daemon reemplaza a `cheka-watch.path`, `cheka-refresh.timer`, `cheka-dns.service` y al
archivo `.refresh-request`. La CLI le envía `Refresh` y recibe el resultado de forma
síncrona, sin esperas activas ni marcas de tiempo.

### 8.3 Traits de plataforma (borrador)

```rust
/// Cómo llega Apache a un PHP concreto.
pub enum FastCgiTarget {
    UnixSocket(PathBuf),                       // Linux, macOS (PHP-FPM)
    Fcgid { php_cgi: PathBuf, phprc: PathBuf }, // Windows (mod_fcgid)
}

pub trait PhpProvider {
    fn available(&self) -> Result<Vec<PhpVersion>>;      // lo que se puede instalar
    fn installed(&self) -> Result<Vec<PhpVersion>>;
    fn install(&self, v: &PhpVersion) -> Result<()>;      // descarga + verificación sha256
    fn cli(&self, v: &PhpVersion) -> PathBuf;
    fn target(&self, v: &PhpVersion) -> FastCgiTarget;
    fn ensure_running(&self, v: &PhpVersion) -> Result<()>; // no-op con fcgid
}

pub trait WebServer {
    fn render_site(&self, site: &Site, php: &FastCgiTarget) -> String;
    fn render_catch_all(&self, tld: &str) -> String;
    fn validate(&self) -> Result<(), String>;             // apache2ctl -t / httpd -t
    fn reload(&self) -> Result<()>;
}

pub trait Resolver {           // configura el sistema para que *.test llegue a 127.0.0.1
    fn install(&self) -> Result<()>;   // resolved / /etc/resolver / NRPT u hosts
    fn uninstall(&self) -> Result<()>;
    fn check(&self, host: &str) -> bool;
}

pub trait ServiceManager {     // systemd / launchd / Windows Services
    fn install_daemon(&self, exe: &Path) -> Result<()>;
    fn start(&self, name: &str) -> Result<()>;
    fn stop(&self, name: &str) -> Result<()>;
    fn status(&self, name: &str) -> ServiceState;  // Active | Inactive | NotInstalled | Failed
}

pub trait Platform {
    fn paths(&self) -> Paths;          // config, datos, logs, runtime (crate directories)
    fn php(&self) -> &dyn PhpProvider;
    fn web(&self) -> &dyn WebServer;
    fn resolver(&self) -> &dyn Resolver;
    fn services(&self) -> &dyn ServiceManager;
    fn trust_ca(&self) -> Result<()>;  // v1: llamar a `mkcert -install`
    fn open_url(&self, url: &str) -> Result<()>;
}
```

Las plantillas de Apache son casi iguales en las tres plataformas. Solo cambia el bloque de
PHP: `SetHandler proxy:unix:…` en Linux y macOS, y `AddHandler fcgid-script .php` +
`FcgidWrapper … .php` + `Options +ExecCGI` en Windows. Conviene una sola plantilla con un
bloque condicional.

### 8.4 Estado en un solo TOML

```toml
# ~/.config/cheka/cheka.toml   (macOS: ~/Library/Application Support/cheka, Windows: %APPDATA%\cheka)
version = 1
tld = "test"
default_php = "8.5"
paths = ["/home/avelar/Sites"]

[links]                     # reemplaza a links/* (sin symlinks: funciona en Windows)
fuera = "/home/avelar/otro-proyecto"

[sites.mi-blog]             # reemplaza a isolated/, secured/ y docroot/
php = "8.2"
secure = true

[sites.legado]
docroot = "htdocs"

[db]
user = "cheka"
password = "secret"
```

**Migración:** si existe `~/.config/cheka/config` (formato de la versión en bash), se leen
`config`, `paths`, `links/*`, `isolated/*`, `secured/*` y `docroot/*`, se escribe
`cheka.toml` y se renombra la carpeta antigua a `legacy/`.

### 8.5 Crates sugeridos

| Necesidad | Crate |
|---|---|
| CLI | `clap` (derive) |
| Estado | `serde`, `toml` |
| Rutas por sistema operativo | `directories` |
| Plantillas | `minijinja` |
| Vigilar carpetas | `notify` + `notify-debouncer-full` |
| IPC | `interprocess` (socket Unix y named pipe con la misma API) |
| DNS integrado (opcional) | `hickory-server` |
| Descargas | `reqwest` (con `rustls`), `sha2` para verificar |
| Archivos comprimidos | `flate2` + `tar` (Linux y macOS), `zip` (Windows) |
| Servicio de Windows | `windows-service` |
| Errores y logs | `anyhow` / `thiserror`, `tracing` |
| Certificados (fase 2) | `rcgen` (en v1, mejor llamar a `mkcert`) |

### 8.6 Plan por fases

1. **Paridad en Linux.** Reimplementar el comportamiento actual en Rust, leyendo el estado de
   la versión en bash y migrándolo. Usar *golden tests*: comparar los vhosts, pools e inis
   generados con los de la versión en bash. Mantener un modo "prefijo" (raíz falsa, sin
   servicios) como `CHEKA_PREFIX`, para probar sin root en CI. Hitos:
   - **1.1 ✅** Núcleo (rutas, estado en el formato de bash, sitios, detección, plantillas
     minijinja, `refresh`) y comandos de lectura (`sites`, `paths`, `versions`, `which-php`,
     `php`, `composer`, `status`, `migrate`), más `php:install` sin descarga. El binario se
     llama `cheka-rs` mientras convive con el script. `tests/parity.rs` compara byte a byte
     contra el script de bash.
   - **1.2 ✅** Comandos que modifican el estado (`park`, `forget`, `link`, `unlink`,
     `isolate`, `unisolate`, `use`, `docroot`, `secure`, `unsecure`, `open`, `log`, `db`,
     `wp`, `new`, `start/stop/restart`) y la descarga de binarios de PHP. La paridad cubre
     una secuencia de 42 pasos (estado y archivos finales idénticos) y `db` contra el
     MariaDB real. `new` (WordPress Multisite + HTTPS, Laravel, CodeIgniter) se probó en el
     sistema real usando el vigilante instalado, sin sudo.
   - **1.3 ✅** `install`/`uninstall` y el daemon `cheka.service` (`cheka daemon`): API por
     socket Unix en `/run/cheka/cheka.sock` (JSON por línea; autoriza con `SO_PEERCRED` solo a
     root y al dueño de los proyectos), vigilancia con `notify` + *debounce* y refresh cada
     minuto. Reemplaza a `cheka-watch.path`, `cheka-refresh.timer` y `.refresh-request`, que
     `install` retira. `install`/`uninstall` funcionan en modo prefijo para probarlos sin root;
     `tests/daemon_install.rs` compara las unidades contra el `write_units` de bash y prueba el
     daemon en vivo (API, carpetas nuevas, rutas aparcadas después de arrancar).
   - **1.4 ✅** El estado vive en `cheka.toml`. El primer guardado (o `install`, o `cheka
     migrate`) migra el formato de bash y aparta los archivos viejos en `legacy/`; `cheka
     migrate --legacy` hace el camino inverso. Las credenciales de la base pasan a `[db]`.
     El binario se llama `cheka`, el script de bash se movió a `legacy/cheka.sh` y Rust ya
     no genera unidades heredadas. La paridad compara salidas y archivos byte a byte y el
     estado por contenido.
2. **macOS.** `platform/macos.rs`: Homebrew `httpd` + launchd + `/etc/resolver/test` +
   static-php-cli para macOS. Reutiliza casi todo de Linux.
3. **Windows.** `platform/windows.rs`: Apache Lounge + `mod_fcgid` + PHP NTS de
   `releases.json`, servicio de Windows, named pipe y DNS (primero el archivo `hosts`, luego
   NRPT + DNS integrado). **Decidido:** DNS con el archivo `hosts`; Apache y PHP los
   descarga cheka a `C:\ProgramData\cheka`, MariaDB con winget. Hitos:
   - **3.1 ✅** Compila en Windows. Lo que depende del sistema operativo vive en
     `src/platform/` (`unix.rs`, `windows.rs`): privilegios (`IsUserAnAdmin`; elevación con el
     `sudo` de Windows 11), `exec`, permisos, dueños, symlinks, usuario y rutas canónicas sin
     `\\?\`. `nix` es dependencia solo de Unix. Rutas: `%APPDATA%\cheka` (usuario) y
     `%ProgramData%\cheka` (sistema); servicios con `sc.exe`/PowerShell. Funcionan los
     comandos de lectura y los que cambian el estado (`park`, `link`, `isolate`…). `install`,
     `php:install` y `daemon` avisan que aún no existen; `tests/parity.rs` y
     `tests/daemon_install.rs` son solo de Unix.
   - **3.2 ✅** `src/windows_setup.rs`: `php:install`/`php:update`/`php:updates` con los zips
     NTS de `releases.json` (sha256 verificado), Apache Lounge + `mod_fcgid` (sha256 del
     `.zip.txt`) y `install`/`uninstall` parciales: runtime de Visual C++ (winget), binario en
     `%ProgramData%\cheka\bin` (agregado al PATH del sistema), PHP por defecto y el servicio
     `cheka-apache`. Plantillas en `templates/windows/` (`FcgidWrapper` por vhost, rutas con `/`,
     vhost de `localhost` primero). **Decisión:** el php.ini va junto a `php.exe`/`php-cgi.exe`
     (PHP en Windows lo busca ahí), con los ajustes de `[php."X.Y"]` al final; así la CLI y
     Apache comparten configuración sin wrappers ni `PHP_INI_SCAN_DIR`. mod_fcgid arranca
     php-cgi con el entorno vacío: `cheka.conf` le pasa `SystemRoot`, `PATH` y `TEMP`. Las
     extensiones comunes se activan solas; `php:ext` aún no aplica en Windows. Probado con
     Apache en primer plano (modo prefijo): dos versiones a la vez, PATH_INFO, estáticos, 404,
     subdominios, docroot de Laravel, comodín, `localhost`, HTTPS saliente y `php:ini`.
   - **3.3 ✅** El daemon es el servicio `cheka` (LocalSystem, crate `windows-service`) con
     el mismo código que en Linux (`daemon.rs`); solo cambia el transporte: named pipe
     `\\.\pipe\cheka` con ACL para SYSTEM, administradores y el SID del dueño (lo guarda
     `install` en `etc\user-sid`; equivale a `SO_PEERCRED`). Su salida va a
     `logs\cheka-daemon.log`. `refresh` mantiene un bloque de cheka en el archivo `hosts`
     (un nombre por sitio más `cheka-check.test` para `status`; sin comodines, así que los
     subsitios de Multisite por subdominio necesitan `cheka link`). mkcert: CA creada con
     `TRUST_STORES=nss` e instalada en el almacén de la **máquina** con `certutil` (sin el
     diálogo del almacén del usuario; Chrome, Edge y Firefox la aceptan); Apache escucha en
     443. MariaDB con winget (servicio `MariaDB`, root sin contraseña y solo local); como no
     hay `unix_socket`, `cheka db` usa el usuario de `[db]` por 127.0.0.1, y exporta sin
     comprimir (Windows no trae gzip). Sin `sudo` en modo "en línea", la elevación usa UAC y
     muestra la salida en la misma terminal. Probado en modo prefijo: API por la pipe, carpeta
     nueva publicada sola, `link` sin permisos, `hosts`, `secure` con HTTPS válido (sitio y
     subdominio) e identidad del usuario vista desde SYSTEM.
   - **3.4 ✅ Paridad con Linux** (lo que en 3.3 quedó distinto):
     - *DNS con comodines:* `dns.rs`, un DNS mínimo en el daemon (127.0.0.1:53) que responde
       `*.test`, más una regla NRPT `.test → 127.0.0.1` (el `~test` de systemd-resolved).
       Funciona en Windows 11 Home **(verificado)**. El archivo `hosts` queda solo como
       respaldo para navegadores con DNS cifrado; `cheka-check.test` ya no va ahí, así que
       `status` comprueba el DNS de verdad.
     - *`php:ext`:* disponibles = `ext\php_*.dll`; activar/desactivar se guarda en
       `cheka.toml` como en Linux; `install` baja la DLL de PECL (`downloads.php.net/~windows/
       pecl`) para la versión de PHP, y sus DLL dependientes van junto a `php.exe`.
     - *`date.timezone`:* la zona de Windows convertida a IANA con el ICU del sistema
       (`ucal_getTimeZoneIDForWindowsID`) y la región del usuario.
     - *MariaDB sin contraseña:* el plugin `named_pipe` autentica con el usuario de Windows
       (el `unix_socket` de Linux); `install` activa la pipe y pone `protocol=PIPE` y el
       usuario en el `[client]` del `my.ini`, así que `mariadb` y `cheka db` entran como tú.
       `.sql.gz` con `flate2`.
     - *`cheka tools`:* `tools/windows.toml` con los mismos ids, nombres, categorías y
       requisitos que `linux.toml`, en PowerShell con winget. Redis es Memurai (compatible).
     - *`services`/`service`:* estado, PID, memoria, versión, puertos y logs con `sc`,
       PowerShell y `netstat`; Windows no tiene journal (los logs son archivos).
     - *UI:* el mismo panel; las acciones de administrador piden UAC (las pide `cheka`) en
       vez de `pkexec`, sin ventanas de consola, con `icon.ico`.
   - **3.5 ✅ Instalador y publicación.**
     - *Instalador:* NSIS generado por Tauri (`ui/tauri.windows.conf.json`): la UI
       (`cheka-ui.exe`, `mainBinaryName` para no chocar con la CLI) y `cheka.exe` en Archivos
       de programa, para todos los usuarios, en español. `ui/windows/hooks.nsh`: al actualizar
       (si existe `etc\user`) corre `cheka install` con la versión nueva; al desinstalar,
       `cheka uninstall` (proyectos, MariaDB y `%ProgramData%\cheka` se conservan).
     - **Decisión:** el instalador **no** configura el sistema. Corre elevado y no sabría quién
       es el dueño de los proyectos; al terminar abre la UI como el usuario (`RunAsUser`), que
       muestra la configuración guiada (`setup_state`/`run_setup`): `cheka install` con UAC
       una sola vez y una lista de pasos que se arma con los títulos `== … ==` de su salida.
       En las actualizaciones el dueño ya está en `etc\user`, así que el hook sí puede.
     - *CI* (`.github/workflows/ci.yml`): clippy y `cargo test` en Ubuntu (incluye la paridad
       con `legacy/cheka.sh`) y Windows en cada PR.
     - *Versiones* (`release.yml`): un tag `vX.Y.Z` publica en GitHub Releases el instalador,
       `cheka.exe` suelto y el binario de Linux (compilado en Ubuntu 22.04 por la glibc), con
       `SHA256SUMS.txt`. La versión sale del tag: el flujo la escribe en `Cargo.toml` y
       `tauri.conf.json` antes de compilar.
4. **UI de bandeja (estilo PHP Monitor)** — Linux y Windows, crate `ui/` (`cheka-ui`, Tauri v2,
   interfaz en HTML/CSS/JS sin frameworks). Ventana con sitios (abrir, HTTPS, versión de PHP,
   carpeta, logs, enlazar carpetas), versiones de PHP, servicios, asistente de `new` con la
   salida en vivo y logs; bandeja con acceso a cada sitio y a reiniciar servicios.
   **Decisión:** la UI corre como el usuario y usa el binario `cheka` instalado como motor
   (`sites/versions/status --json` para leer y los mismos comandos de la CLI para modificar),
   en vez de hablar solo con el daemon como se planeó. Motivo: el daemon corre como root y no
   debe ejecutar acciones del usuario (Composer, WP-CLI); así la UI se comporta exactamente
   igual que la terminal y no duplica lógica. Las acciones de root usan `pkexec`. Los
   comandos permitidos están en listas blancas en `ui/src/main.rs`.
   **Ajustes y extensiones por versión** (`php:info/ini/ext/update`, sección PHP de la UI):
   se guardan en `cheka.toml` y el daemon los aplica (`99-cheka.ini`, y para el PHP de apt
   una carpeta de extensiones propia, `ext.d`, que refleja al sistema más los cambios del
   usuario). **Decidido:** las extensiones solo se gestionan en el PHP de apt; en los
   binarios estáticos se muestran pero no se pueden cambiar. Si hace falta en esas versiones,
   la alternativa sería compilarlas con static-php-cli.
5. **Extras.** DNS integrado en todas las plataformas (deja de depender de dnsmasq), PHP 7.4
   donde exista (Windows y macOS vía Homebrew), `cheka share` (túnel) y quizá un icono en la
   bandeja.

### 8.7 Estrategia de pruebas

- **Unitarias en `core`:** detección (con árboles de carpetas falsos), normalización de nombres,
  render con *golden files* y migración del estado.
- **Integración por plataforma en CI:** GitHub Actions ofrece `ubuntu-latest`, `macos-latest` y
  `windows-latest`. Levantar Apache y PHP en un puerto alto, con usuario normal, y ejecutar los
  contratos de §7 con `curl`. Eso ya se hizo a mano con la versión en bash usando el puerto
  8080.
- **Pruebas manuales:** la instalación real en cada sistema operativo (DNS, CA, servicios).

---

## 9. Pendientes conocidos (de la versión en bash)

- Los subsitios de Multisite creados después del alta se registran con `http://` aunque el
  sitio use HTTPS.
- No hay PHP 7.4 en Linux; se podría resolver con un backend de contenedor solo para esa
  versión.
- Los binarios estáticos no cargan extensiones como Xdebug; solo el PHP de apt lo permite.
- `db drop` pide confirmación interactiva; falta un `--force` para scripts.
- La credencial `cheka`/`secret` está fija en el script; debería ir a la configuración.
