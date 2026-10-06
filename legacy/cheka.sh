#!/usr/bin/env bash
# cheka — entorno local PHP para Ubuntu, al estilo de Laravel Valet.
#
#   Apache (el del sistema) + PHP-FPM multi-versión + MariaDB + dominios *.test
#
# Estado del usuario:  ~/.config/cheka/
# Estado del sistema:  /etc/cheka, /etc/apache2/cheka, /opt/cheka, /var/log/cheka
set -euo pipefail

CHEKA_VERSION="0.1.0"
SELF="$(readlink -f "${BASH_SOURCE[0]}")"

# CHEKA_PREFIX permite probar la herramienta sin root (todo se escribe bajo ese prefijo
# y se omiten systemctl/apache2ctl).
PREFIX="${CHEKA_PREFIX:-}"
BIN="$PREFIX/usr/local/bin"
ETC="$PREFIX/etc/cheka"
OPT="$PREFIX/opt/cheka"
APACHE_SITES="$PREFIX/etc/apache2/cheka/sites"
LOG_DIR="$PREFIX/var/log/cheka"
RUN_DIR="$PREFIX/run/cheka"
UNITS="$PREFIX/etc/systemd/system"
RESOLVED_DROPIN="$PREFIX/etc/systemd/resolved.conf.d/cheka.conf"
APACHE_CONF="$PREFIX/etc/apache2/conf-available/cheka.conf"
APACHE_SITE_CONF="$PREFIX/etc/apache2/sites-available/cheka.conf"
APACHE_ENVVARS="$PREFIX/etc/apache2/envvars"

TLD="test"
DNS_PORT=5300
STATIC_URL="https://dl.static-php.dev/static-php-cli/bulk"
SUPPORTED_PHP="8.0 8.1 8.2 8.3 8.4 8.5"
DB_USER="cheka"
DB_PASS="secret"
WPCLI_URL="https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar"

# ---------------------------------------------------------------- usuario ----

if [[ -n ${CHEKA_USER:-} ]]; then
    :
elif [[ $EUID -eq 0 && -n ${SUDO_USER:-} && $SUDO_USER != root ]]; then
    CHEKA_USER=$SUDO_USER
elif [[ $EUID -eq 0 && -f $ETC/user ]]; then
    CHEKA_USER=$(<"$ETC/user")
else
    CHEKA_USER=$(id -un)
fi
USER_HOME=$(getent passwd "$CHEKA_USER" | cut -d: -f6)
CONF="${CHEKA_CONF:-$USER_HOME/.config/cheka}"
WPCLI="$USER_HOME/.local/share/cheka/wp-cli.phar"

# ---------------------------------------------------------------- helpers ----

if [[ -t 1 ]]; then
    C_B=$'\e[1m' C_G=$'\e[32m' C_Y=$'\e[33m' C_R=$'\e[31m' C_C=$'\e[36m' C_0=$'\e[0m'
else
    C_B="" C_G="" C_Y="" C_R="" C_C="" C_0=""
fi
QUIET=0
info() { [[ $QUIET -eq 1 ]] || echo "${C_C}›${C_0} $*"; }
ok()   { [[ $QUIET -eq 1 ]] || echo "${C_G}✔${C_0} $*"; }
warn() { echo "${C_Y}!${C_0} $*" >&2; }
die()  { echo "${C_R}✘${C_0} $*" >&2; exit 1; }
step() { echo; echo "${C_B}== $* ==${C_0}"; }

is_root() { [[ $EUID -eq 0 || -n $PREFIX ]]; }

# Vuelve a ejecutar el comando completo con sudo si hace falta.
need_root() { is_root || exec sudo -- "$SELF" "$@"; }

# Ejecuta un comando como root (pidiendo sudo si hace falta).
as_root() { if is_root; then "$@"; else sudo -- "$@"; fi; }

# Ejecuta un comando como el usuario dueño de los proyectos.
as_user() {
    if [[ $EUID -eq 0 && $CHEKA_USER != root ]]; then
        sudo -u "$CHEKA_USER" -H -- "$@"
    else
        "$@"
    fi
}

sc() { [[ -n $PREFIX ]] && return 0; systemctl "$@"; }

system_php() { php -r 'echo PHP_MAJOR_VERSION.".".PHP_MINOR_VERSION;' 2>/dev/null || echo "8.5"; }

valid_version() {
    [[ " $SUPPORTED_PHP " == *" $1 "* ]] || die "Versión de PHP no soportada: '$1'. Disponibles: $SUPPORTED_PHP"
}

# Acepta "8.2", "php@8.2" o "php8.2".
norm_version() { local v=${1#php@}; v=${v#php}; echo "$v"; }

site_name() {
    local n=${1,,}
    n=${n//[^a-z0-9-]/-}
    n=${n##-}; n=${n%%-}
    echo "$n"
}

config_get() {
    local key=$1 def=${2:-}
    [[ -f $CONF/config ]] && grep -E "^$key=" "$CONF/config" | tail -1 | cut -d= -f2- | grep . && return 0
    echo "$def"
}

config_set() {
    local key=$1 val=$2
    as_user mkdir -p "$CONF"
    as_user touch "$CONF/config"
    as_user sed -i "/^$key=/d" "$CONF/config"
    echo "$key=$val" | as_user tee -a "$CONF/config" >/dev/null
}

default_php() { config_get default_php "$(system_php)"; }

# ------------------------------------------------------------------- PHP ----

php_fpm_bin() {
    if [[ -x /usr/sbin/php-fpm$1 ]]; then echo "/usr/sbin/php-fpm$1"; else echo "$OPT/php/$1/php-fpm"; fi
}

php_cli_bin() {
    if [[ -x /usr/bin/php$1 ]]; then echo "/usr/bin/php$1"; else echo "$BIN/php$1"; fi
}

php_installed() { [[ -x $(php_fpm_bin "$1") ]]; }

php_socket() { echo "$RUN_DIR/php-$1/fpm.sock"; }

write_php_config() {
    local v=$1 dir="$ETC/php/$1" tz
    tz=$(timedatectl show -p Timezone --value 2>/dev/null || true)
    [[ -n $tz ]] || tz=UTC
    mkdir -p "$dir"
    cat >"$dir/php-fpm.conf" <<EOF
; Generado por cheka — no editar (se sobrescribe).
[global]
pid = $RUN_DIR/php-$v/fpm.pid
error_log = $LOG_DIR/php-$v-fpm.log
daemonize = no

[cheka]
user = $CHEKA_USER
group = $(id -gn "$CHEKA_USER")
listen = $(php_socket "$v")
listen.owner = $CHEKA_USER
listen.group = $(id -gn "$CHEKA_USER")
listen.mode = 0660
pm = ondemand
pm.max_children = 10
pm.process_idle_timeout = 60s
request_terminate_timeout = 600
catch_workers_output = yes
decorate_workers_output = no
clear_env = no
php_admin_value[error_log] = $LOG_DIR/php-$v-errors.log
EOF
    # php.ini propio: valores cómodos para desarrollo. Para cambios personales usa
    # $dir/conf.d/*.ini (no se sobrescribe).
    mkdir -p "$dir/conf.d"
    cat >"$dir/php.ini" <<EOF
; Generado por cheka — no editar (se sobrescribe). Ajustes propios en conf.d/*.ini
memory_limit = 512M
upload_max_filesize = 256M
post_max_size = 256M
max_execution_time = 300
max_input_time = 300
max_input_vars = 5000
display_errors = On
display_startup_errors = On
error_reporting = E_ALL
log_errors = On
date.timezone = $tz
curl.cainfo = /etc/ssl/certs/ca-certificates.crt
openssl.cafile = /etc/ssl/certs/ca-certificates.crt
mysqli.default_socket = /run/mysqld/mysqld.sock
pdo_mysql.default_socket = /run/mysqld/mysqld.sock
opcache.enable = 1
opcache.validate_timestamps = 1
opcache.revalidate_freq = 0
EOF
}

# Envoltorio php8.X para los binarios estáticos (los de apt ya traen el suyo).
write_cli_wrapper() {
    local v=$1
    [[ -x /usr/bin/php$v ]] && return 0
    mkdir -p "$BIN"
    cat >"$BIN/php$v" <<EOF
#!/bin/sh
# Generado por cheka
PHPRC="$ETC/php/$v" PHP_INI_SCAN_DIR=":$ETC/php/$v/conf.d" exec "$OPT/php/$v/php" "\$@"
EOF
    chmod 755 "$BIN/php$v"
}

download_static_php() {
    local v=$1 arch full tmp
    arch=$(uname -m)
    info "Buscando la última versión de PHP $v…"
    full=$(curl -fsSL "$STATIC_URL/" | grep -oE "php-${v//./\\.}\.[0-9]+-fpm-linux-$arch\.tar\.gz" | sort -uV | tail -1 || true)
    [[ -n $full ]] || die "No encontré binarios de PHP $v para $arch en $STATIC_URL"
    full=${full#php-}; full=${full%%-fpm-*}
    tmp=$(mktemp -d)
    info "Descargando PHP $full (fpm + cli)…"
    curl -fL --progress-bar -o "$tmp/fpm.tgz" "$STATIC_URL/php-$full-fpm-linux-$arch.tar.gz"
    curl -fL --progress-bar -o "$tmp/cli.tgz" "$STATIC_URL/php-$full-cli-linux-$arch.tar.gz"
    mkdir -p "$OPT/php/$v"
    tar xzf "$tmp/fpm.tgz" -C "$OPT/php/$v"
    tar xzf "$tmp/cli.tgz" -C "$OPT/php/$v"
    chown -R root:root "$OPT/php/$v" 2>/dev/null || true
    echo "$full" >"$OPT/php/$v/VERSION"
    rm -rf "$tmp"
}

# php:install <versión>   (root)
php_install() {
    local v=$1
    valid_version "$v"
    if ! php_installed "$v"; then
        if [[ -z $PREFIX && -x /usr/bin/php$v ]] && apt-cache show "php$v-fpm" >/dev/null 2>&1; then
            info "Instalando php$v-fpm desde apt…"
            apt-get install -y -qq "php$v-fpm" >/dev/null
            sc disable --now "php$v-fpm" >/dev/null 2>&1 || true
        else
            download_static_php "$v"
        fi
    fi
    mkdir -p "$LOG_DIR" "$RUN_DIR/php-$v"
    write_php_config "$v"
    write_cli_wrapper "$v"
    sc enable "cheka-php@$v" >/dev/null 2>&1
    sc restart "cheka-php@$v"
    ok "PHP $v listo ($(php_fpm_bin "$v"))"
}

ensure_php() {
    local v=$1
    valid_version "$v"
    php_installed "$v" || as_root "$SELF" php:install "$v"
}

# ----------------------------------------------------------------- sitios ----

# Imprime "nombre<TAB>ruta" por cada sitio (carpetas aparcadas + enlaces).
list_sites() {
    declare -A seen=()
    local p d n l
    if [[ -f $CONF/paths ]]; then
        while IFS= read -r p; do
            [[ -n $p && -d $p ]] || continue
            for d in "$p"/*/; do
                [[ -d $d ]] || continue
                d=$(readlink -f "$d")
                n=$(site_name "$(basename "$d")")
                [[ -n $n ]] && seen[$n]=$d
            done
        done <"$CONF/paths"
    fi
    for l in "$CONF"/links/*; do
        [[ -L $l ]] || continue
        seen[$(basename "$l")]=$(readlink -f "$l")
    done
    for n in "${!seen[@]}"; do printf '%s\t%s\n' "$n" "${seen[$n]}"; done | sort
}

# Detecta el tipo de proyecto. Imprime "tipo|docroot".
detect() {
    local d=$1 t
    if [[ -f $CONF/docroot/$2 ]]; then
        echo "personalizado|$d/$(<"$CONF/docroot/$2")"
    elif [[ -f $d/web/wp-config.php || -d $d/web/wp ]]; then
        echo "wordpress-bedrock|$d/web"
    elif [[ -f $d/wp-config.php || -f $d/wp-load.php ]]; then
        t=wordpress
        if grep -Eqs "MULTISITE['\"][[:space:]]*,[[:space:]]*true" "$d/wp-config.php"; then
            if grep -Eqs "SUBDOMAIN_INSTALL['\"][[:space:]]*,[[:space:]]*true" "$d/wp-config.php"; then
                t=wp-multisite-subdominios
            else
                t=wp-multisite
            fi
        fi
        echo "$t|$d"
    elif [[ -f $d/artisan ]]; then
        echo "laravel|$d/public"
    elif [[ -f $d/spark ]]; then
        echo "codeigniter4|$d/public"
    elif [[ -d $d/system && -d $d/application ]]; then
        echo "codeigniter3|$d"
    elif [[ -f $d/public/index.php ]]; then
        echo "php|$d/public"
    else
        echo "php|$d"
    fi
}

site_php() {
    if [[ -f $CONF/isolated/$1 ]]; then cat "$CONF/isolated/$1"; else default_php; fi
}

site_secured() { [[ -f $CONF/secured/$1 && -f $CONF/certs/$1.$TLD.pem ]]; }

# Resuelve el sitio indicado o el del directorio actual → SITE_NAME / SITE_PATH.
resolve_site() {
    local want=${1:-} cwd n p best="" bestp=""
    cwd=$(readlink -f "$PWD")
    while IFS=$'\t' read -r n p; do
        if [[ -n $want ]]; then
            [[ $n == "$want" ]] && { best=$n; bestp=$p; break; }
        elif [[ $cwd == "$p" || $cwd == "$p"/* ]] && ((${#p} > ${#bestp})); then
            best=$n; bestp=$p
        fi
    done < <(list_sites)
    if [[ -z $best ]]; then
        [[ -n $want ]] && die "No existe el sitio '$want'. Revisa: cheka sites"
        die "Este directorio no es un sitio de cheka. Usa 'cheka link' o 'cheka park'."
    fi
    SITE_NAME=$best
    SITE_PATH=$bestp
}

vhost_body() {
    local name=$1 path=$2 docroot=$3 v=$4
    cat <<EOF
    ServerName $name.$TLD
    ServerAlias *.$name.$TLD
    DocumentRoot "$docroot"
    <Directory "$path">
        Options FollowSymLinks
        AllowOverride All
        Require all granted
    </Directory>
    DirectoryIndex index.php index.html index.htm
    <FilesMatch "\.php$">
        <If "-f %{REQUEST_FILENAME}">
            SetHandler "proxy:unix:$(php_socket "$v")|fcgi://cheka-php-$v"
        </If>
    </FilesMatch>
    ErrorLog $LOG_DIR/$name-error.log
EOF
}

write_vhost() {
    local out=$1 name=$2 path=$3 type=$4 docroot=$5 v=$6
    {
        echo "# Generado por cheka — no editar. Sitio: $name ($type, PHP $v)"
        echo "<VirtualHost *:80>"
        vhost_body "$name" "$path" "$docroot" "$v"
        if site_secured "$name"; then
            echo "    RewriteEngine On"
            echo "    RewriteRule ^ https://%{HTTP_HOST}%{REQUEST_URI} [R=302,L]"
        fi
        echo "</VirtualHost>"
        if site_secured "$name"; then
            echo "<VirtualHost *:443>"
            vhost_body "$name" "$path" "$docroot" "$v"
            echo "    SSLEngine on"
            echo "    SSLCertificateFile \"$CONF/certs/$name.$TLD.pem\""
            echo "    SSLCertificateKeyFile \"$CONF/certs/$name.$TLD-key.pem\""
            echo "</VirtualHost>"
        fi
    } >"$out"
}

write_watch_unit() {
    local f="$UNITS/cheka-watch.path" tmp p
    tmp=$(mktemp)
    {
        echo "# Generado por cheka"
        echo "[Unit]"
        echo "Description=cheka: vigila las carpetas aparcadas"
        echo "[Path]"
        if [[ -f $CONF/paths ]]; then
            while IFS= read -r p; do [[ -n $p ]] && echo "PathChanged=$p"; done <"$CONF/paths"
        fi
        echo "PathChanged=$CONF/.refresh-request"
        echo "Unit=cheka-refresh.service"
        echo "[Install]"
        echo "WantedBy=multi-user.target"
    } >"$tmp"
    if ! cmp -s "$tmp" "$f"; then
        mkdir -p "$UNITS"
        mv "$tmp" "$f"
        chmod 644 "$f"
        sc daemon-reload
        if sc is-enabled -q cheka-watch.path 2>/dev/null; then sc restart cheka-watch.path; fi
    else
        rm -f "$tmp"
    fi
}

# Pide un refresh. Sin root, se lo pide al servicio cheka-watch (no requiere sudo) y
# espera su resultado; si el servicio no responde, recurre a sudo.
request_refresh() {
    if is_root; then
        "$SELF" refresh
        return
    fi
    if systemctl is-active -q cheka-watch.path 2>/dev/null; then
        local req="$CONF/.refresh-request" i
        date +%s%N >"$req"
        for ((i = 0; i < 60; i++)); do
            if [[ $RUN_DIR/last-refresh -nt $req ]]; then
                if grep -q '^ERROR' "$RUN_DIR/last-refresh"; then
                    die "$(sed 's/^ERROR: //' "$RUN_DIR/last-refresh")"
                fi
                ok "$(<"$RUN_DIR/last-refresh")"
                return
            fi
            sleep 0.25
        done
        warn "El servicio cheka-watch no respondió; lo hago con sudo"
    fi
    as_root "$SELF" refresh
}

refresh_result() {
    echo "$1" >"$RUN_DIR/last-refresh"
    chmod 644 "$RUN_DIR/last-refresh"
}

# refresh: regenera los vhosts de Apache a partir de los sitios (root).
cmd_refresh() {
    [[ ${1:-} == --quiet ]] && QUIET=1
    need_root refresh "$@"
    mkdir -p "$APACHE_SITES" "$RUN_DIR"
    exec 9>"$RUN_DIR/refresh.lock"
    flock 9

    local new n p type docroot v def count=0
    declare -A versions=()
    def=$(default_php)
    new=$(mktemp -d)
    while IFS=$'\t' read -r n p; do
        if [[ $p == *'"'* ]]; then warn "Omitiendo '$p': la ruta contiene comillas"; continue; fi
        IFS='|' read -r type docroot < <(detect "$p" "$n")
        v=$(site_php "$n")
        if ! php_installed "$v"; then
            warn "$n: PHP $v no está instalado, uso PHP $def (instálalo con: cheka isolate $v)"
            v=$def
        fi
        versions[$v]=1
        write_vhost "$new/$n.conf" "$n" "$p" "$type" "$docroot" "$v"
        count=$((count + 1))
    done < <(list_sites)

    for v in "${!versions[@]}"; do
        sc is-active -q "cheka-php@$v" || sc enable --now "cheka-php@$v" >/dev/null 2>&1 || warn "No pude iniciar PHP $v"
    done
    write_watch_unit

    if diff -rq "$new" "$APACHE_SITES" >/dev/null 2>&1; then
        rm -rf "$new"
        refresh_result "Sin cambios ($count sitios)"
        ok "Sin cambios ($count sitios)"
        return 0
    fi
    local backup
    backup=$(mktemp -d)
    cp -a "$APACHE_SITES/." "$backup/"
    find "$APACHE_SITES" -maxdepth 1 -name '*.conf' -delete
    cp "$new"/*.conf "$APACHE_SITES/" 2>/dev/null || true
    rm -rf "$new"
    if [[ -z $PREFIX ]]; then
        local err
        if ! err=$(apache2ctl -t 2>&1); then
            find "$APACHE_SITES" -maxdepth 1 -name '*.conf' -delete
            cp -a "$backup/." "$APACHE_SITES/"
            rm -rf "$backup"
            refresh_result "ERROR: La configuración generada no es válida (revisa: sudo apache2ctl -t)"
            die "La configuración generada no es válida; restauré la anterior:"$'\n'"$err"
        fi
        if sc is-active -q apache2; then sc reload apache2; fi
    fi
    rm -rf "$backup"
    refresh_result "Apache actualizado ($count sitios)"
    ok "Apache actualizado ($count sitios)"
}

# ------------------------------------------------------------ comandos ----

cmd_park() {
    local dir
    dir=$(readlink -f "${1:-$PWD}")
    [[ -d $dir ]] || die "No existe el directorio: $dir"
    as_user mkdir -p "$CONF"
    as_user touch "$CONF/paths"
    if grep -qxF "$dir" "$CONF/paths"; then
        info "$dir ya estaba aparcado"
    else
        echo "$dir" | as_user tee -a "$CONF/paths" >/dev/null
        ok "Aparcado: cada carpeta dentro de $dir será <carpeta>.$TLD"
    fi
    request_refresh
}

cmd_forget() {
    local dir
    dir=$(readlink -f "${1:-$PWD}")
    grep -qxF "$dir" "$CONF/paths" 2>/dev/null || die "$dir no está aparcado"
    as_user sh -c 'grep -vxF "$1" "$2" >"$2.tmp"; mv "$2.tmp" "$2"' _ "$dir" "$CONF/paths"
    ok "Olvidado: $dir"
    request_refresh
}

cmd_paths() { [[ -s $CONF/paths ]] && cat "$CONF/paths" || info "No hay carpetas aparcadas (usa: cheka park)"; }

cmd_link() {
    local name
    name=$(site_name "${1:-$(basename "$PWD")}")
    [[ -n $name ]] || die "Nombre de sitio inválido"
    as_user mkdir -p "$CONF/links"
    as_user ln -sfn "$(readlink -f "$PWD")" "$CONF/links/$name"
    ok "Enlazado: http://$name.$TLD → $PWD"
    request_refresh
}

cmd_unlink() {
    local name
    name=$(site_name "${1:-$(basename "$PWD")}")
    [[ -L $CONF/links/$name ]] || die "No hay un enlace llamado '$name'"
    as_user rm -f "$CONF/links/$name"
    ok "Enlace eliminado: $name"
    request_refresh
}

cmd_sites() {
    local n p type docroot v url any=0
    printf "${C_B}%-24s %-26s %-5s %-34s %s${C_0}\n" SITIO TIPO PHP URL RUTA
    while IFS=$'\t' read -r n p; do
        any=1
        IFS='|' read -r type docroot < <(detect "$p" "$n")
        v=$(site_php "$n")
        if site_secured "$n"; then url="https://$n.$TLD"; else url="http://$n.$TLD"; fi
        php_installed "$v" || v="$v!"
        printf "%-24s %-26s %-5s %-34s %s\n" "$n" "$type" "$v" "$url" "$p"
    done < <(list_sites)
    [[ $any -eq 1 ]] || info "No hay sitios todavía. Crea una carpeta en ~/Sites o usa 'cheka link'."
}

cmd_isolate() {
    local v site=""
    [[ $# -ge 1 ]] || die "Uso: cheka isolate <versión> [--site=nombre]"
    v=$(norm_version "$1"); shift
    [[ ${1:-} == --site=* ]] && site=${1#--site=}
    resolve_site "$site"
    ensure_php "$v"
    as_user mkdir -p "$CONF/isolated"
    echo "$v" | as_user tee "$CONF/isolated/$SITE_NAME" >/dev/null
    ok "$SITE_NAME usará PHP $v"
    request_refresh
}

cmd_unisolate() {
    local site=""
    [[ ${1:-} == --site=* ]] && site=${1#--site=}
    resolve_site "$site"
    as_user rm -f "$CONF/isolated/$SITE_NAME"
    ok "$SITE_NAME vuelve a la versión por defecto (PHP $(default_php))"
    request_refresh
}

cmd_use() {
    [[ $# -ge 1 ]] || { echo "PHP por defecto: $(default_php)"; return; }
    local v
    v=$(norm_version "$1")
    ensure_php "$v"
    config_set default_php "$v"
    ok "PHP por defecto: $v"
    request_refresh
}

cmd_docroot() {
    resolve_site ""
    if [[ $# -eq 0 ]]; then
        as_user rm -f "$CONF/docroot/$SITE_NAME"
        ok "$SITE_NAME vuelve a la detección automática"
    else
        [[ -d $SITE_PATH/$1 ]] || die "No existe $SITE_PATH/$1"
        as_user mkdir -p "$CONF/docroot"
        echo "${1%/}" | as_user tee "$CONF/docroot/$SITE_NAME" >/dev/null
        ok "$SITE_NAME servirá desde $SITE_PATH/${1%/}"
    fi
    request_refresh
}

# Versión de PHP del directorio actual (la del sitio, o la por defecto).
current_php() {
    if (resolve_site "" >/dev/null 2>&1); then
        resolve_site ""
        site_php "$SITE_NAME"
    else
        default_php
    fi
}

cmd_php() {
    local bin
    bin=$(php_cli_bin "$(current_php)")
    [[ -x $bin ]] || die "No encuentro $bin"
    exec "$bin" "$@"
}

cmd_composer() {
    local bin composer
    bin=$(php_cli_bin "$(current_php)")
    composer=$(command -v composer) || die "Composer no está instalado"
    exec "$bin" "$composer" "$@"
}

cmd_which_php() { php_cli_bin "$(current_php)"; }

ensure_wpcli() {
    [[ -f $WPCLI ]] && return 0
    info "Descargando WP-CLI…"
    as_user mkdir -p "$(dirname "$WPCLI")"
    as_user curl -fsSL -o "$WPCLI" "$WPCLI_URL" || die "No pude descargar WP-CLI"
}

cmd_wp() {
    ensure_wpcli
    exec "$(php_cli_bin "$(current_php)")" "$WPCLI" "$@"
}

# Cambia (o agrega) KEY=valor en un .env de Laravel, aunque esté comentado.
set_env() {
    local file=$1 key=$2 val=$3
    if grep -qE "^#? *$key=" "$file"; then
        sed -i -E "s|^#? *$key=.*|$key=$val|" "$file"
    else
        echo "$key=$val" >>"$file"
    fi
}

# Igual para el .env de CodeIgniter 4 (formato "clave = valor").
set_ci_env() {
    local file=$1 key=$2 val=$3 re=${2//./\\.}
    if grep -qE "^#? *$re *=" "$file"; then
        sed -i -E "s|^#? *$re *=.*|$key = $val|" "$file"
    else
        echo "$key = $val" >>"$file"
    fi
}

write_wp_htaccess() {
    local dir=$1 mode=$2
    {
        echo "# BEGIN WordPress"
        echo "<IfModule mod_rewrite.c>"
        echo "RewriteEngine On"
        echo "RewriteRule .* - [E=HTTP_AUTHORIZATION:%{HTTP:Authorization}]"
        echo "RewriteBase /"
        echo "RewriteRule ^index\.php$ - [L]"
        case $mode in
            subdirectorios)
                echo "RewriteRule ^([_0-9a-zA-Z-]+/)?wp-admin$ \$1wp-admin/ [R=301,L]"
                echo "RewriteCond %{REQUEST_FILENAME} -f [OR]"
                echo "RewriteCond %{REQUEST_FILENAME} -d"
                echo "RewriteRule ^ - [L]"
                echo "RewriteRule ^([_0-9a-zA-Z-]+/)?(wp-(content|admin|includes).*) \$2 [L]"
                echo "RewriteRule ^([_0-9a-zA-Z-]+/)?(.*\.php)$ \$2 [L]"
                echo "RewriteRule . index.php [L]"
                ;;
            subdominios)
                echo "RewriteRule ^wp-admin$ wp-admin/ [R=301,L]"
                echo "RewriteCond %{REQUEST_FILENAME} -f [OR]"
                echo "RewriteCond %{REQUEST_FILENAME} -d"
                echo "RewriteRule ^ - [L]"
                echo "RewriteRule ^(wp-(content|admin|includes).*) \$1 [L]"
                echo "RewriteRule ^(.*\.php)$ \$1 [L]"
                echo "RewriteRule . index.php [L]"
                ;;
            *)
                echo "RewriteCond %{REQUEST_FILENAME} !-f"
                echo "RewriteCond %{REQUEST_FILENAME} !-d"
                echo "RewriteRule . /index.php [L]"
                ;;
        esac
        echo "</IfModule>"
        echo "# END WordPress"
    } >"$dir/.htaccess"
}

new_wordpress() {
    local dir=$1 name=$2 db=$3 url=$4 phpbin=$5 multisite=$6 locale=$7
    ensure_wpcli
    local wp=("$phpbin" "$WPCLI" --path="$dir" --quiet)
    info "Descargando WordPress ($locale)…"
    "${wp[@]}" core download --locale="$locale"
    db_create "$db"
    printf "define( 'WP_DEBUG', true );\ndefine( 'WP_DEBUG_LOG', true );\ndefine( 'WP_ENVIRONMENT_TYPE', 'local' );\n" |
        "${wp[@]}" config create --dbname="$db" --dbuser="$DB_USER" --dbpass="$DB_PASS" --dbhost=localhost \
            --locale="$locale" --extra-php
    local install=(core install)
    if [[ -n $multisite ]]; then
        install=(core multisite-install)
        [[ $multisite == subdominios ]] && install+=(--subdomains)
    fi
    info "Instalando WordPress…"
    "${wp[@]}" "${install[@]}" --url="$url" --title="$name" --admin_user=admin --admin_password=admin \
        --admin_email="admin@$name.$TLD" --skip-email
    "${wp[@]}" rewrite structure '/%postname%/'
    write_wp_htaccess "$dir" "$multisite"
    NEW_NOTES="Admin: $url/wp-admin  (usuario: admin, contraseña: admin)"
}

new_laravel() {
    local dir=$1 name=$2 db=$3 url=$4 phpbin=$5 composer
    composer=$(command -v composer) || die "Composer no está instalado"
    info "Creando proyecto Laravel con Composer…"
    "$phpbin" "$composer" create-project --no-interaction --prefer-dist laravel/laravel "$dir"
    db_create "$db"
    set_env "$dir/.env" APP_URL "$url"
    set_env "$dir/.env" DB_CONNECTION mysql
    set_env "$dir/.env" DB_HOST 127.0.0.1
    set_env "$dir/.env" DB_PORT 3306
    set_env "$dir/.env" DB_DATABASE "$db"
    set_env "$dir/.env" DB_USERNAME "$DB_USER"
    set_env "$dir/.env" DB_PASSWORD "$DB_PASS"
    rm -f "$dir/database/database.sqlite"
    info "Ejecutando migraciones en MariaDB…"
    (cd "$dir" && "$phpbin" artisan migrate --force --no-interaction)
    NEW_NOTES="Proyecto en $dir (.env apuntando a MariaDB '$db')"
}

new_codeigniter() {
    local dir=$1 name=$2 db=$3 url=$4 phpbin=$5 composer
    composer=$(command -v composer) || die "Composer no está instalado"
    info "Creando proyecto CodeIgniter 4 con Composer…"
    "$phpbin" "$composer" create-project --no-interaction --prefer-dist codeigniter4/appstarter "$dir"
    db_create "$db"
    cp "$dir/env" "$dir/.env"
    set_ci_env "$dir/.env" CI_ENVIRONMENT development
    set_ci_env "$dir/.env" app.baseURL "'$url/'"
    set_ci_env "$dir/.env" database.default.hostname localhost
    set_ci_env "$dir/.env" database.default.database "$db"
    set_ci_env "$dir/.env" database.default.username "$DB_USER"
    set_ci_env "$dir/.env" database.default.password "$DB_PASS"
    set_ci_env "$dir/.env" database.default.DBDriver MySQLi
    set_ci_env "$dir/.env" database.default.port 3306
    NEW_NOTES="Proyecto en $dir (.env en modo development, base '$db')"
}

new_php() {
    local dir=$1 name=$2
    cat >"$dir/index.php" <<'PHP'
<?php
echo '<h1>' . htmlspecialchars($_SERVER['HTTP_HOST']) . '</h1>';
echo '<p>PHP ' . PHP_VERSION . ' — edita ' . __FILE__ . '</p>';
PHP
    NEW_NOTES="Proyecto en $dir (sin base de datos; créala con: cheka db create)"
    NEW_DB=0
}

cmd_new() {
    local usage="Uso: cheka new <wordpress|laravel|codeigniter|php> <nombre> [--php=8.2] [--secure] [--multisite[=subdominios]] [--locale=es_MX]"
    [[ $EUID -ne 0 ]] || die "Ejecuta 'cheka new' con tu usuario, sin sudo"
    [[ $# -ge 2 ]] || die "$usage"
    local type=$1 raw=$2 opt php="" secure=0 multisite="" locale=es_MX
    shift 2
    for opt in "$@"; do
        case $opt in
            --php=*) php=$(norm_version "${opt#--php=}") ;;
            --secure) secure=1 ;;
            --multisite | --multisite=subdirectorios | --multisite=subdirectories) multisite=subdirectorios ;;
            --multisite=subdominios | --multisite=subdomains) multisite=subdominios ;;
            --locale=*) locale=${opt#--locale=} ;;
            *) die "Opción desconocida: $opt"$'\n'"$usage" ;;
        esac
    done
    case $type in
        wp | wordpress) type=wordpress ;;
        laravel) ;;
        ci | ci4 | codeigniter) type=codeigniter ;;
        php) ;;
        *) die "Tipo desconocido: $type"$'\n'"$usage" ;;
    esac
    [[ -z $multisite || $type == wordpress ]] || die "--multisite solo aplica a WordPress"

    local name base dir db url phpbin
    name=$(site_name "$raw")
    [[ -n $name ]] || die "Nombre inválido: $raw"
    base=$(head -n 1 "$CONF/paths" 2>/dev/null || true)
    [[ -n $base ]] || base="$USER_HOME/Sites"
    dir="$base/$name"
    [[ ! -e $dir ]] || die "Ya existe $dir"
    if list_sites | cut -f1 | grep -qxF "$name"; then die "Ya existe un sitio llamado '$name'"; fi
    db=${name//-/_}

    if [[ -n $php ]]; then
        ensure_php "$php"
    else
        php=$(default_php)
    fi
    phpbin=$(php_cli_bin "$php")
    [[ -x $phpbin ]] || die "No encuentro $phpbin"

    mkdir -p "$dir"
    trap 'warn "La creación falló a medias. Revisa $dir (bórralo para reintentar) y la base \`$db\`."' EXIT
    if [[ $php != "$(default_php)" ]]; then
        mkdir -p "$CONF/isolated"
        echo "$php" >"$CONF/isolated/$name"
    fi
    url="http://$name.$TLD"
    if [[ $secure -eq 1 ]]; then
        make_cert "$name"
        url="https://$name.$TLD"
    fi

    NEW_NOTES="" NEW_DB=1
    "new_$type" "$dir" "$name" "$db" "$url" "$phpbin" "$multisite" "$locale"
    trap - EXIT
    request_refresh

    echo
    echo "${C_G}${C_B}$name listo:${C_0} $url"
    [[ -n $multisite ]] && echo "  Multisite por $multisite"
    if [[ $NEW_DB -eq 1 ]]; then
        echo "  PHP $php · Base de datos '$db' (usuario $DB_USER / $DB_PASS)"
    else
        echo "  PHP $php"
    fi
    [[ -n $NEW_NOTES ]] && echo "  $NEW_NOTES"
    return 0
}

cmd_versions() {
    local v def mark
    def=$(default_php)
    for v in $SUPPORTED_PHP; do
        mark=" "
        [[ $v == "$def" ]] && mark="*"
        if php_installed "$v"; then
            printf "%s %s  instalada  %s\n" "$mark" "$v" "$(php_fpm_bin "$v")"
        else
            printf "%s %s  -\n" "$mark" "$v"
        fi
    done
    echo "(* = por defecto)"
}

make_cert() {
    local name=$1
    command -v mkcert >/dev/null || die "mkcert no está instalado (ejecuta: sudo cheka install)"
    as_user mkdir -p "$CONF/certs" "$CONF/secured"
    as_user mkcert -cert-file "$CONF/certs/$name.$TLD.pem" -key-file "$CONF/certs/$name.$TLD-key.pem" \
        "$name.$TLD" "*.$name.$TLD" >/dev/null 2>&1 || die "mkcert falló"
    as_user touch "$CONF/secured/$name"
    ok "https://$name.$TLD listo"
}

cmd_secure() {
    resolve_site "${1:-}"
    make_cert "$SITE_NAME"
    request_refresh
}

cmd_unsecure() {
    resolve_site "${1:-}"
    as_user rm -f "$CONF/secured/$SITE_NAME" "$CONF/certs/$SITE_NAME.$TLD.pem" "$CONF/certs/$SITE_NAME.$TLD-key.pem"
    ok "$SITE_NAME vuelve a http"
    request_refresh
}

cmd_open() {
    resolve_site "${1:-}"
    local url="http://$SITE_NAME.$TLD"
    site_secured "$SITE_NAME" && url="https://$SITE_NAME.$TLD"
    xdg-open "$url" >/dev/null 2>&1 &
    echo "$url"
}

cmd_log() {
    resolve_site "${1:-}"
    local v
    v=$(site_php "$SITE_NAME")
    tail -n 50 -F "$LOG_DIR/$SITE_NAME-error.log" "$LOG_DIR/php-$v-errors.log"
}

db_create() {
    [[ $1 =~ ^[A-Za-z0-9_]+$ ]] || die "Nombre de base de datos inválido: $1"
    mariadb -e "CREATE DATABASE IF NOT EXISTS \`$1\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
    ok "Base de datos '$1' lista"
}

db_name_default() {
    resolve_site ""
    echo "${SITE_NAME//-/_}"
}

cmd_db() {
    local sub=${1:-help} name
    shift || true
    case $sub in
        create)
            db_create "${1:-$(db_name_default)}"
            echo "  host: localhost (o 127.0.0.1)   usuario: $DB_USER   contraseña: $DB_PASS"
            ;;
        drop)
            name=${1:-$(db_name_default)}
            [[ $name =~ ^[A-Za-z0-9_]+$ ]] || die "Nombre de base de datos inválido: $name"
            read -r -p "¿Borrar la base de datos '$name'? Escribe su nombre para confirmar: " confirm
            [[ $confirm == "$name" ]] || die "Cancelado"
            mariadb -e "DROP DATABASE IF EXISTS \`$name\`"
            ok "Base de datos '$name' borrada"
            ;;
        list)
            mariadb -N -e "SHOW DATABASES" | grep -vE '^(information_schema|performance_schema|mysql|sys)$'
            ;;
        import)
            [[ $# -ge 1 ]] || die "Uso: cheka db import <archivo.sql[.gz]> [base]"
            local file=$1
            name=${2:-$(db_name_default)}
            [[ -f $file ]] || die "No existe $file"
            mariadb -e "CREATE DATABASE IF NOT EXISTS \`$name\` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
            if [[ $file == *.gz ]]; then zcat "$file" | mariadb "$name"; else mariadb "$name" <"$file"; fi
            ok "Importado $file en '$name'"
            ;;
        export)
            name=${1:-$(db_name_default)}
            local out=${2:-$name-$(date +%Y%m%d-%H%M%S).sql.gz}
            mariadb-dump --single-transaction --routines "$name" | gzip >"$out"
            ok "Exportado a $out"
            ;;
        *)
            cat <<EOF
Uso: cheka db <create|drop|list|import|export> [...]
  create [base]                   Crea la base (por defecto: nombre del sitio actual)
  drop [base]                     Borra la base (pide confirmación)
  list                            Lista las bases
  import <archivo.sql[.gz]> [base]
  export [base] [archivo.sql.gz]
Credenciales para tus proyectos: usuario '$DB_USER', contraseña '$DB_PASS', host localhost.
EOF
            ;;
    esac
}

# ---------------------------------------------------- servicios / estado ----

php_units() { systemctl list-units --all --plain --no-legend 'cheka-php@*.service' 2>/dev/null | awk '{print $1}'; }

cmd_services() {
    need_root "$1"
    local action=$1 u
    for u in cheka-dns apache2 mariadb $(php_units); do
        systemctl "$action" "$u" && ok "$action $u" || warn "$action $u falló"
    done
}

cmd_status() {
    local u state
    for u in apache2 cheka-dns mariadb cheka-watch.path cheka-refresh.timer $(php_units); do
        state=$(systemctl is-active "$u" 2>/dev/null || true)
        if [[ $state == active ]]; then echo "${C_G}●${C_0} $u"; else echo "${C_R}●${C_0} $u ($state)"; fi
    done
    if getent hosts "cheka-check.$TLD" >/dev/null; then
        echo "${C_G}●${C_0} DNS: *.$TLD → $(getent hosts "cheka-check.$TLD" | awk '{print $1; exit}')"
    else
        echo "${C_R}●${C_0} DNS: *.$TLD no resuelve"
    fi
    echo "PHP por defecto: $(default_php)"
}

# ------------------------------------------------------------ instalación ----

write_units() {
    mkdir -p "$UNITS"
    cat >"$UNITS/cheka-php@.service" <<EOF
# Generado por cheka
[Unit]
Description=cheka PHP-FPM %i
After=network.target

[Service]
ExecStart=$BIN/cheka _fpm %i
ExecReload=/bin/kill -USR2 \$MAINPID
RuntimeDirectory=cheka/php-%i
RuntimeDirectoryMode=0755
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
    cat >"$UNITS/cheka-dns.service" <<EOF
# Generado por cheka
[Unit]
Description=cheka DNS (*.$TLD → 127.0.0.1)
After=network.target

[Service]
ExecStart=/usr/sbin/dnsmasq --keep-in-foreground --conf-file=/dev/null --no-resolv --no-hosts --no-poll --bind-interfaces --listen-address=127.0.0.1 --port=$DNS_PORT --address=/$TLD/127.0.0.1 --user=nobody --group=nogroup --pid-file=/run/cheka-dns/dnsmasq.pid
RuntimeDirectory=cheka-dns
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
    cat >"$UNITS/cheka-refresh.service" <<EOF
# Generado por cheka
[Unit]
Description=cheka: regenera los sitios
After=apache2.service

[Service]
Type=oneshot
ExecStartPre=/bin/sleep 1
ExecStart=$BIN/cheka refresh --quiet
EOF
    cat >"$UNITS/cheka-refresh.timer" <<EOF
# Generado por cheka
[Unit]
Description=cheka: revisa los sitios cada minuto

[Timer]
OnBootSec=1min
OnUnitActiveSec=1min

[Install]
WantedBy=timers.target
EOF
}

install_mkcert() {
    command -v mkcert >/dev/null && return 0
    if ! apt-get install -y -qq mkcert >/dev/null 2>&1; then
        info "mkcert no está en apt; descargando binario oficial…"
        local arch
        case $(uname -m) in x86_64) arch=amd64 ;; aarch64) arch=arm64 ;; *) arch=$(uname -m) ;; esac
        curl -fsSL -o /usr/local/bin/mkcert "https://dl.filippo.io/mkcert/latest?for=linux/$arch"
        chmod 755 /usr/local/bin/mkcert
    fi
}

cmd_install() {
    need_root install "$@"
    [[ $CHEKA_USER != root ]] || die "Ejecuta 'sudo ./cheka install' desde tu usuario normal (no como root directo)."
    [[ -z $PREFIX ]] || die "install no está disponible con CHEKA_PREFIX"
    local sysphp group
    sysphp=$(system_php)
    group=$(id -gn "$CHEKA_USER")

    step "Paquetes"
    apt-get update -qq || warn "apt-get update falló; continúo"
    apt-get install -y -qq "php$sysphp-fpm" dnsmasq-base libnss3-tools curl >/dev/null
    install_mkcert
    ok "php$sysphp-fpm, dnsmasq, mkcert"

    step "Archivos de cheka"
    if [[ $SELF != "$BIN/cheka" ]]; then install -m 755 "$SELF" "$BIN/cheka"; fi
    mkdir -p "$ETC" "$OPT/php" "$APACHE_SITES" "$LOG_DIR"
    echo "$CHEKA_USER" >"$ETC/user"
    chown "$CHEKA_USER:$group" "$LOG_DIR"
    as_user mkdir -p "$CONF/links" "$CONF/isolated" "$CONF/secured" "$CONF/certs" "$CONF/docroot" "$USER_HOME/Sites"
    [[ -f $CONF/config ]] || config_set default_php "$sysphp"
    as_user touch "$CONF/paths" "$CONF/.refresh-request"
    [[ -s $CONF/paths ]] || echo "$USER_HOME/Sites" | as_user tee "$CONF/paths" >/dev/null
    write_units
    systemctl daemon-reload
    ok "$BIN/cheka instalado, sitios en ~/Sites"

    step "DNS para *.$TLD"
    mkdir -p "$(dirname "$RESOLVED_DROPIN")"
    cat >"$RESOLVED_DROPIN" <<EOF
# Generado por cheka: solo las consultas *.$TLD van al dnsmasq local.
[Resolve]
DNS=127.0.0.1:$DNS_PORT
Domains=~$TLD
EOF
    systemctl enable --now cheka-dns >/dev/null 2>&1
    systemctl restart cheka-dns
    systemctl restart systemd-resolved
    sleep 1
    ok "dnsmasq en 127.0.0.1:$DNS_PORT"

    step "PHP $sysphp (FPM)"
    php_install "$sysphp"

    step "Apache"
    a2dismod -q -f "php$sysphp" mpm_prefork >/dev/null 2>&1 || true
    a2enmod -q mpm_event proxy proxy_fcgi setenvif rewrite ssl headers socache_shmcb >/dev/null
    sed -i '/# >>> cheka/,/# <<< cheka/d' "$APACHE_ENVVARS"
    cat >>"$APACHE_ENVVARS" <<EOF
# >>> cheka
export APACHE_RUN_USER=$CHEKA_USER
export APACHE_RUN_GROUP=$group
# <<< cheka
EOF
    cat >"$APACHE_CONF" <<EOF
# Generado por cheka
ServerName localhost
ProxyTimeout 600
EOF
    # Como sitio (sites-enabled/cheka.conf) para quedar después de 000-default: así
    # localhost sigue siendo el sitio por defecto y no el primer proyecto.
    cat >"$APACHE_SITE_CONF" <<EOF
# Generado por cheka
IncludeOptional $APACHE_SITES/*.conf

# Cualquier otro *.$TLD: aviso claro en vez de mostrar otro proyecto.
<VirtualHost *:80>
    ServerName no-encontrado.$TLD
    ServerAlias *.$TLD
    RewriteEngine On
    RewriteRule ^ - [R=404,L]
    ErrorDocument 404 "cheka: no hay un sitio para este dominio. Revisa: cheka sites"
</VirtualHost>
EOF
    a2ensite -q cheka >/dev/null
    a2disconf -q "php$sysphp-fpm" >/dev/null 2>&1 || true
    a2enconf -q cheka >/dev/null
    ok "mpm_event + proxy_fcgi, Apache corre como $CHEKA_USER"

    step "Sitios"
    cmd_refresh
    systemctl restart apache2
    systemctl enable --now cheka-watch.path cheka-refresh.timer >/dev/null 2>&1
    ok "Vigilando ~/Sites: las carpetas nuevas se publican solas"

    step "MariaDB"
    if systemctl is-active -q mariadb; then
        mariadb <<EOF
CREATE USER IF NOT EXISTS '$CHEKA_USER'@'localhost' IDENTIFIED VIA unix_socket;
GRANT ALL PRIVILEGES ON *.* TO '$CHEKA_USER'@'localhost' WITH GRANT OPTION;
CREATE USER IF NOT EXISTS '$DB_USER'@'localhost' IDENTIFIED BY '$DB_PASS';
CREATE USER IF NOT EXISTS '$DB_USER'@'127.0.0.1' IDENTIFIED BY '$DB_PASS';
GRANT ALL PRIVILEGES ON *.* TO '$DB_USER'@'localhost';
GRANT ALL PRIVILEGES ON *.* TO '$DB_USER'@'127.0.0.1';
FLUSH PRIVILEGES;
EOF
        ok "Usuario '$CHEKA_USER' (sin contraseña, por socket) y '$DB_USER'/'$DB_PASS' para tus proyectos"
    else
        warn "MariaDB no está activo; omito la creación de usuarios"
    fi

    step "HTTPS local (mkcert)"
    local caroot
    caroot=$(as_user mkcert -CAROOT)
    CAROOT=$caroot TRUST_STORES=system mkcert -install >/dev/null 2>&1 || warn "No pude instalar la CA en el sistema"
    chown -R "$CHEKA_USER:$group" "$caroot"
    as_user env TRUST_STORES=nss mkcert -install >/dev/null 2>&1 || warn "No pude instalar la CA en los navegadores (NSS)"
    ok "CA local instalada"

    step "Verificación"
    if getent hosts "cheka-check.$TLD" | grep -q 127.0.0.1; then ok "*.$TLD resuelve a 127.0.0.1"; else warn "*.$TLD no resuelve todavía"; fi
    if getent hosts ubuntu.com >/dev/null; then ok "El DNS normal sigue funcionando"; else warn "No resuelve ubuntu.com: revisa $RESOLVED_DROPIN"; fi

    echo
    echo "${C_G}${C_B}cheka está listo.${C_0} Crea o clona un proyecto en ~/Sites y ábrelo en http://<carpeta>.$TLD"
    echo "Ayuda: cheka help"
}

cmd_uninstall() {
    need_root uninstall "$@"
    local sysphp u
    sysphp=$(system_php)
    step "Desinstalando cheka"
    systemctl disable --now cheka-watch.path cheka-refresh.timer cheka-dns >/dev/null 2>&1 || true
    for u in $(php_units); do systemctl disable --now "$u" >/dev/null 2>&1 || true; done
    rm -f "$UNITS"/cheka-php@.service "$UNITS"/cheka-dns.service "$UNITS"/cheka-refresh.{service,timer} "$UNITS"/cheka-watch.path
    systemctl daemon-reload
    rm -f "$RESOLVED_DROPIN"
    systemctl restart systemd-resolved
    a2disconf -q cheka >/dev/null 2>&1 || true
    a2dissite -q cheka >/dev/null 2>&1 || true
    rm -f "$APACHE_CONF" "$APACHE_SITE_CONF"
    rm -rf "$(dirname "$APACHE_SITES")"
    sed -i '/# >>> cheka/,/# <<< cheka/d' "$APACHE_ENVVARS"
    a2dismod -q -f mpm_event proxy_fcgi >/dev/null 2>&1 || true
    a2enmod -q mpm_prefork "php$sysphp" >/dev/null 2>&1 || true
    systemctl restart apache2 || warn "Apache no arrancó; revisa 'apache2ctl -t'"
    rm -f "$BIN"/php8.[0-9]
    if [[ ${1:-} == --purge ]]; then
        rm -rf "$OPT" "$ETC" "$LOG_DIR"
        as_user rm -rf "$CONF"
        ok "Eliminados también los binarios de PHP y la configuración de usuario"
    else
        ok "Se conservan $OPT, $ETC y $CONF (usa --purge para borrarlos)"
    fi
    rm -f "$BIN/cheka"
    ok "Apache vuelve a mod_php + prefork. Tus proyectos en ~/Sites no se tocaron."
}

cmd_fpm() {
    local v=${1:?versión}
    valid_version "$v"
    PHP_INI_SCAN_DIR=":$ETC/php/$v/conf.d" exec "$(php_fpm_bin "$v")" --nodaemonize --fpm-config "$ETC/php/$v/php-fpm.conf" -c "$ETC/php/$v/php.ini"
}

cmd_help() {
    cat <<EOF
${C_B}cheka $CHEKA_VERSION${C_0} — entorno local PHP (Apache + PHP-FPM + MariaDB + *.$TLD)

${C_B}Instalación${C_0}
  install                    Configura todo (pide sudo)
  uninstall [--purge]        Revierte la configuración
  start | stop | restart     Controla Apache, DNS, MariaDB y PHP
  status                     Estado de los servicios

${C_B}Proyectos nuevos${C_0}
  new wordpress <nombre> [--multisite[=subdominios]] [--locale=es_MX]
  new laravel <nombre>
  new codeigniter <nombre>
  new php <nombre>
      Opciones para todos: --php=8.2  --secure

${C_B}Sitios${C_0}
  park [dir]                 Cada carpeta dentro de dir → <carpeta>.$TLD (por defecto: ~/Sites)
  forget [dir]               Deja de aparcar dir
  paths                      Carpetas aparcadas
  link [nombre]              Publica el directorio actual como nombre.$TLD
  unlink [nombre]            Elimina un enlace
  sites                      Lista sitios, tipo detectado, PHP y URL
  open [sitio]               Abre el sitio en el navegador
  secure [sitio]             HTTPS con certificado local
  unsecure [sitio]           Vuelve a HTTP
  docroot [subcarpeta]       Fuerza la carpeta pública (sin argumento: detección automática)
  log [sitio]                Sigue los logs de Apache y PHP del sitio
  refresh                    Regenera la configuración (se hace solo)

${C_B}PHP${C_0}
  versions                   Versiones disponibles e instaladas
  use <versión>              Versión por defecto (ej: cheka use 8.3)
  isolate <versión>          Versión para el sitio actual (ej: cheka isolate 8.1)
  unisolate                  El sitio actual vuelve a la versión por defecto
  php [args]                 Ejecuta el PHP del sitio actual (ej: cheka php artisan migrate)
  composer [args]            Composer con el PHP del sitio actual
  wp [args]                  WP-CLI con el PHP del sitio actual (ej: cheka wp plugin list)
  which-php                  Ruta del PHP del sitio actual

${C_B}Base de datos (MariaDB)${C_0}
  db create|drop|list|import|export    (cheka db para ver detalles)
EOF
}

# ------------------------------------------------------------------ main ----

cmd=${1:-help}
shift || true
case $cmd in
    install) cmd_install "$@" ;;
    uninstall) cmd_uninstall "$@" ;;
    refresh) cmd_refresh "$@" ;;
    park) cmd_park "$@" ;;
    forget) cmd_forget "$@" ;;
    paths) cmd_paths ;;
    link) cmd_link "$@" ;;
    unlink) cmd_unlink "$@" ;;
    sites | links | ls) cmd_sites ;;
    isolate) cmd_isolate "$@" ;;
    unisolate) cmd_unisolate "$@" ;;
    use) cmd_use "$@" ;;
    docroot) cmd_docroot "$@" ;;
    php) cmd_php "$@" ;;
    composer) cmd_composer "$@" ;;
    wp) cmd_wp "$@" ;;
    new) cmd_new "$@" ;;
    which-php) cmd_which_php ;;
    versions) cmd_versions ;;
    php:install) need_root php:install "$@"; php_install "$(norm_version "${1:?versión}")" ;;
    secure) cmd_secure "$@" ;;
    unsecure) cmd_unsecure "$@" ;;
    open) cmd_open "$@" ;;
    log) cmd_log "$@" ;;
    db) cmd_db "$@" ;;
    start | stop | restart) cmd_services "$cmd" ;;
    status) cmd_status ;;
    _fpm) cmd_fpm "$@" ;;
    help | -h | --help) cmd_help ;;
    -v | --version) echo "cheka $CHEKA_VERSION" ;;
    *) die "Comando desconocido: $cmd (usa: cheka help)" ;;
esac
