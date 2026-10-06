# cheka

Entorno local de desarrollo PHP al estilo de **Laravel Valet**, para Ubuntu.
Cada carpeta dentro de `~/Sites` se publica sola como `http://<carpeta>.test`, con la
versión de PHP que elijas por proyecto, MariaDB y HTTPS local.

- **Un solo archivo bash** (`cheka`, unas 1.300 líneas), sin Docker.
- Reutiliza lo que Ubuntu ya trae: **Apache** (los `.htaccess` funcionan tal cual), **MariaDB** y **systemd**.
- Proyectos soportados: WordPress (single y Multisite, por subdirectorios o subdominios), Laravel, CodeIgniter 3 y 4, Bedrock y PHP sin framework.

> Versión 0.1.0, probada en Ubuntu 26.04 con Apache 2.4.66, PHP 8.5 y MariaDB 11.8.
> Para la arquitectura interna y el plan para portarlo a macOS y Windows, consulta
> [`docs/ARQUITECTURA.md`](docs/ARQUITECTURA.md).

---

## Instalación

```bash
sudo ~/cheka/cheka install
```

La instalación es **idempotente**: puedes repetirla sin problema, y es la forma de aplicar
cambios después de editar el script. Hace lo siguiente:

1. Instala `phpX.Y-fpm`, `dnsmasq-base`, `libnss3-tools` y `mkcert`.
2. Copia el script a `/usr/local/bin/cheka` y crea `~/Sites`.
3. Configura el DNS para que solo `*.test` vaya a un dnsmasq local en `127.0.0.1:5300`.
4. Deja PHP-FPM corriendo con tu usuario.
5. Cambia Apache de `mod_php` + `prefork` a `mpm_event` + `proxy_fcgi`, y lo pone a correr con tu usuario.
6. Crea en MariaDB el usuario `<tu-usuario>`, que entra sin contraseña por socket, y el usuario `cheka`/`secret`, para los proyectos.
7. Instala la CA local de mkcert en el sistema y en los navegadores.
8. Verifica que `*.test` resuelva y que el DNS normal siga funcionando.

Para desinstalar:

```bash
sudo cheka uninstall            # revierte Apache, DNS y servicios; conserva binarios y configuración
sudo cheka uninstall --purge    # también borra /opt/cheka, /etc/cheka y ~/.config/cheka
```

Tus proyectos en `~/Sites` nunca se tocan.

---

## Uso rápido

```bash
# Proyecto nuevo, listo para usar
cheka new wordpress mi-blog
cheka new wordpress red --multisite=subdominios --secure
cheka new laravel api --php=8.2
cheka new codeigniter panel
cheka new php experimento

# Proyecto existente: basta con ponerlo en ~/Sites
cd ~/Sites && git clone git@github.com:yo/tienda.git
#   → en un minuto o menos está en http://tienda.test
cd tienda
cheka db import respaldo.sql.gz
cheka isolate 8.1    # versiones disponibles: 8.0–8.5
cheka secure
cheka open
```

---

## Comandos

### Instalación y servicios

| Comando | Qué hace |
|---|---|
| `install` | Configura todo (pide sudo). Idempotente. |
| `uninstall [--purge]` | Revierte la configuración del sistema. |
| `start` / `stop` / `restart` | Controla Apache, dnsmasq, MariaDB y los PHP-FPM. |
| `status` | Estado de los servicios y prueba de DNS. |

### Proyectos nuevos

| Comando | Qué deja listo |
|---|---|
| `new wordpress <nombre>` | Descarga WordPress con WP-CLI, crea la base de datos y `wp-config.php` (`WP_DEBUG`, `WP_ENVIRONMENT_TYPE=local`), lo instala con **admin / admin**, activa enlaces permanentes `/%postname%/` y crea el `.htaccess`. |
| ↳ `--multisite` / `--multisite=subdominios` | Lo instala como Multisite, por subdirectorios o por subdominios. |
| ↳ `--locale=es_MX` | Idioma de WordPress (por defecto `es_MX`). |
| `new laravel <nombre>` | `composer create-project laravel/laravel`, `.env` apuntando a MariaDB y migraciones ejecutadas. |
| `new codeigniter <nombre>` | `composer create-project codeigniter4/appstarter` y `.env` en modo development, con `baseURL` y la base de datos configurados. |
| `new php <nombre>` | Carpeta con un `index.php` (sin base de datos). |
| `--php=8.2`, `--secure` | Sirven para cualquier tipo: versión de PHP propia y HTTPS desde el inicio. |

La base de datos se llama igual que el sitio, con `-` cambiado por `_` (`mi-blog` → `mi_blog`).

### Sitios

| Comando | Qué hace |
|---|---|
| `park [dir]` | Publica cada subcarpeta de `dir` como `<subcarpeta>.test`. `~/Sites` ya viene aparcado. |
| `forget [dir]` | Deja de aparcar `dir`. |
| `paths` | Lista las carpetas aparcadas. |
| `link [nombre]` | Publica el directorio actual como `nombre.test` aunque esté fuera de `~/Sites`. |
| `unlink [nombre]` | Elimina un enlace. |
| `sites` (o `ls`, `links`) | Tabla con cada sitio: tipo detectado, PHP, URL y ruta. |
| `open [sitio]` | Abre el sitio en el navegador. |
| `secure [sitio]` / `unsecure [sitio]` | Activa o quita HTTPS. El certificado cubre `sitio.test` y `*.sitio.test`. |
| `docroot [subcarpeta]` | Fuerza la carpeta pública. Sin argumento, vuelve a la detección automática. |
| `log [sitio]` | Sigue los logs de Apache y PHP del sitio. |
| `refresh` | Regenera la configuración de Apache. Normalmente ocurre solo. |

Los comandos sin `[sitio]` usan el sitio del directorio actual.

### PHP

| Comando | Qué hace |
|---|---|
| `versions` | Versiones soportadas e instaladas (la marcada con `*` es la de por defecto). |
| `use <versión>` | Cambia la versión por defecto. |
| `isolate <versión> [--site=x]` | Asigna una versión al sitio actual. Si no está instalada, la descarga (pide sudo una vez). |
| `unisolate` | El sitio actual vuelve a la versión por defecto. |
| `php …`, `composer …`, `wp …` | Ejecutan PHP, Composer o WP-CLI **con la versión del sitio actual**. |
| `which-php` | Ruta del binario de PHP del sitio actual. |

Las versiones distintas de la del sistema también quedan disponibles como `php8.2`,
`php8.4`, etc. en `/usr/local/bin`.

### Base de datos (MariaDB)

| Comando | Qué hace |
|---|---|
| `db create [base]` | Crea la base (`utf8mb4_unicode_ci`). |
| `db drop [base]` | Borra la base, pidiendo que escribas su nombre para confirmar. |
| `db list` | Lista las bases. |
| `db import <archivo.sql[.gz]> [base]` | Importa un respaldo; crea la base si no existe. |
| `db export [base] [archivo]` | Exporta a `.sql.gz`. |

**Credenciales para tus proyectos:** usuario `cheka`, contraseña `secret`, host `localhost`
o `127.0.0.1`. Desde la terminal entras con `mariadb`, sin contraseña.

---

## Detección de proyectos

Gana la primera regla que se cumple:

| Si encuentra… | Tipo | Carpeta pública |
|---|---|---|
| `cheka docroot` configurado | `personalizado` | la indicada |
| `web/wp-config.php` o `web/wp/` | `wordpress-bedrock` | `web/` |
| `wp-config.php` o `wp-load.php` | `wordpress`, `wp-multisite` o `wp-multisite-subdominios` (según `MULTISITE` y `SUBDOMAIN_INSTALL`) | raíz |
| `artisan` | `laravel` | `public/` |
| `spark` | `codeigniter4` | `public/` |
| `system/` y `application/` | `codeigniter3` | raíz |
| `public/index.php` | `php` | `public/` |
| (ninguna) | `php` | raíz |

Cada sitio responde también en `*.sitio.test`, lo que permite usar Multisite por subdominios.

---

## Versiones de PHP

| Versión | Origen |
|---|---|
| La del sistema (8.5 en Ubuntu 26.04) | Paquete `php8.5-fpm` de apt. Admite extensiones de apt, incluida Xdebug. |
| 8.0 – 8.4 | Binarios estáticos de [static-php-cli](https://static-php.dev) (`bulk`), en `/opt/cheka/php/<v>/`. |

Los binarios estáticos traen: apcu, bcmath, bz2, curl, dom, exif, gd, gmp, iconv, imagick,
imap, intl, mbstring, mysqli, opcache, pdo_mysql, pdo_pgsql, pgsql, redis, soap, sodium,
sqlite3, xsl, zip, entre otras. **No pueden cargar extensiones adicionales** (`.so`), así que
no admiten Xdebug.

Para ajustes propios de `php.ini` usa `/etc/cheka/php/<v>/conf.d/*.ini`, que cheka no
sobrescribe.

---

## Limitaciones conocidas

- **PHP 7.4 no está disponible.** El PPA de ondrej no tiene paquetes para Ubuntu 26.04 y
  static-php-cli empieza en 8.0.
- **"DNS seguro" del navegador:** con un proveedor personalizado en Chrome o Brave, `.test`
  no resuelve. Déjalo en "automático" o desactívalo.
- **`/tmp`:** Apache usa un `/tmp` privado (`PrivateTmp`), así que los sitios en `/tmp` dan
  error 403. Pon tus proyectos en tu home.
- **Subsitios de Multisite:** los creados después con `cheka wp site create` se registran con
  `http://` aunque el sitio principal use HTTPS. Responden igual por HTTPS.
- **Detección al clonar:** si clonas un repositorio grande, durante unos segundos puede
  detectarse como `php`. El temporizador lo corrige en un minuto o menos; si no quieres
  esperar, ejecuta `cheka refresh`.

---

## Solución de problemas

| Síntoma | Revisa |
|---|---|
| `x.test` no abre | `cheka status`, que incluye la prueba de DNS, y `resolvectl query x.test` |
| "cheka: no hay un sitio para este dominio" | El nombre no coincide con ninguna carpeta: `cheka sites` |
| Error 502 o 503 | El PHP-FPM de esa versión: `systemctl status cheka-php@8.2` |
| Cambios que no aparecen | `cheka refresh` y `journalctl -u cheka-refresh -n 20` |
| Errores de PHP | `cheka log` |
| Apache no recarga | `sudo apache2ctl -t`. cheka valida antes de recargar y, si algo falla, restaura la configuración anterior. |

---

## Port a Rust (en curso)

En la rama `rust-fase-1` está la reimplementación en Rust, que convive con el script y se
compila como `cheka-rs`. El plan completo está en
[`docs/ARQUITECTURA.md` §8](docs/ARQUITECTURA.md#8-propuesta-cheka-en-rust).

```bash
cargo build                 # target/debug/cheka-rs
cargo test                  # unitarias + paridad byte a byte contra ./cheka (bash)
cargo clippy --all-targets
```

Ya están portados todos los comandos, más `daemon` (el servicio que reemplaza al vigilante
de systemd) y `migrate`, que muestra una vista previa del estado en TOML. Con `PARITY_SHOW=1 cargo test
--test parity -- --nocapture` se ve la salida de cada paso de las pruebas de paridad.

## Pruebas

Esto se ejecutó durante el desarrollo:

- **Modo prefijo, sin root.** Con `CHEKA_PREFIX=/ruta CHEKA_CONF=/ruta cheka …` todo se escribe
  bajo ese prefijo y se omiten `systemctl` y `apache2ctl`. Con esa configuración se levantaron un
  Apache y varios PHP-FPM con el usuario normal en el puerto 8080. Los 6 tipos de proyecto
  respondieron con su carpeta pública y su versión de PHP, y también se probaron los
  subdominios, los archivos estáticos, el 404 y PATH_INFO.
- **En el sistema real:**
  - Publicación automática de una carpeta nueva en unos 3 segundos.
  - PHP corriendo con el usuario normal y conexión a MariaDB.
  - `secure`, `link` y `unlink` sin sudo; `localhost` intacto y el vhost comodín para dominios desconocidos.
  - `new` con los cuatro tipos: WordPress Multisite por subdominios con HTTPS y un subsitio, Laravel con sus migraciones en MariaDB, CodeIgniter 4 conectado a la base, y PHP plano.
