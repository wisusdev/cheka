// Panel de cheka. Toda la lógica vive en el binario `cheka`; esto solo la muestra y la
// invoca. El texto variable se inserta siempre como texto (nunca como HTML).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const state = { sites: [], versions: [], status: null, page: "sites", creating: false, error: null };

/** En Windows cambian algunos textos (UAC en vez de contraseña, rutas, origen de PHP). */
const WIN = navigator.userAgent.includes("Windows");
const INSTALL_CMD = WIN ? ".\\target\\release\\cheka.exe install" : "sudo ./target/release/cheka install";
const PHP_ORIGIN = (source) => (source === "apt" ? "paquete del sistema" : WIN ? "zip de windows.php.net" : "binario estático");
if (WIN) {
  for (const el of document.querySelectorAll("[data-win]")) el.textContent = el.dataset.win;
  for (const el of document.querySelectorAll("[data-win-placeholder]")) el.placeholder = el.dataset.winPlaceholder;
}

// ------------------------------------------------------------------ utilidades ----

const $ = (sel) => document.querySelector(sel);

/** Crea un elemento: h("button", { class: "btn", onclick }, "texto", hijo…) */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v == null) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

function toast(message, error = false) {
  const el = h("div", { class: error ? "toast error" : "toast", role: error ? "alert" : "status" }, message);
  $("#toasts").append(el);
  setTimeout(() => el.remove(), error ? 9000 : 4000);
}

/** Texto útil de la salida de cheka (sin los símbolos ✔ › !). */
function summary(out) {
  const text = (out.ok ? out.stdout : out.stderr || out.stdout).trim();
  return text.replace(/^[✔›!✘] /gm, "") || (out.ok ? "Listo" : "Falló");
}

/** Deshabilita el botón mientras corre la acción y muestra el resultado. */
async function action(button, fn) {
  if (button) button.disabled = true;
  try {
    const out = await fn();
    if (out && "ok" in out) toast(summary(out), !out.ok);
    await reload();
  } catch (e) {
    toast(String(e), true);
  } finally {
    if (button) button.disabled = false;
    // Tras una acción (aunque haya fallado) se redibuja todo: así un interruptor o un
    // selector vuelve a mostrar el estado real.
    for (const k in lastRender) delete lastRender[k];
    render();
  }
}

const run = (...args) => invoke("run", { args });
const runRoot = (...args) => invoke("run_root", { args });
const installedVersions = () => state.versions.filter((v) => v.installed);
const defaultVersion = () => state.versions.find((v) => v.default)?.version ?? "";

// ------------------------------------------------------------------ datos ----

/** Ejecuta `fn` solo si `data` cambió desde la última vez para esta `key` (evita
 *  redibujar cada 4 s, lo que quitaría el foco o anularía un clic en curso). */
const lastRender = {};
function changed(key, data) {
  const sig = JSON.stringify(data);
  if (lastRender[key] === sig) return false;
  lastRender[key] = sig;
  return true;
}

async function reload() {
  const [sites, versions, status] = await Promise.allSettled([invoke("sites"), invoke("versions"), invoke("status")]);
  if (sites.status === "fulfilled") state.sites = sites.value;
  if (versions.status === "fulfilled") state.versions = versions.value;
  if (status.status === "fulfilled") state.status = status.value;
  if (state.page === "services") await loadServices();
  const failed = [sites, versions, status].find((r) => r.status === "rejected");
  state.error = failed ? String(failed.reason) : null;
  render();
}

/** Si no se pudo leer el estado, decirlo claramente (nunca mostrar listas vacías como si
 *  todo estuviera bien). */
function renderBanner() {
  const banner = $("#banner");
  banner.classList.toggle("hidden", !state.error);
  if (!state.error) return;
  const outdated = /unexpected argument '--json'/.test(state.error);
  const missing = /No pude ejecutar cheka/.test(state.error);
  const detail =
    outdated
      ? ["El cheka instalado es anterior a este panel. Actualízalo desde el repositorio:\n",
         h("code", {}, `cargo build --release && ${INSTALL_CMD}`)]
      : missing
        ? ["No encuentro cheka instalado. Instálalo con ", h("code", {}, INSTALL_CMD)]
        : [state.error];
  banner.replaceChildren(h("strong", {}, "No pude leer el estado de cheka. "), ...detail);
}

function render() {
  renderBanner();
  renderHealth();
  renderSites();
  renderVersions();
  renderServices();
  renderNewForm();
  renderLogSites();
}

// ------------------------------------------------------------------ salud ----

function renderHealth() {
  const st = state.status;
  const set = (id, ok) => ($(id).className = "dot " + (ok == null ? "" : ok ? "ok" : "bad"));
  set("#dot-daemon", st?.daemon);
  set("#dot-dns", st ? Boolean(st.dns) : null);
  set("#dot-apache", st ? st.services.some((s) => (s.name === "apache2" || s.name === "cheka-apache") && s.state === "active") : null);
  $("#sites-count").textContent = state.sites.length || "";
}

// ------------------------------------------------------------------ sitios ----

const KIND_NAMES = {
  wordpress: "WordPress",
  "wp-multisite": "WordPress Multisite",
  "wp-multisite-subdominios": "WordPress Multisite · subdominios",
  "wordpress-bedrock": "WordPress (Bedrock)",
  laravel: "Laravel",
  codeigniter4: "CodeIgniter 4",
  codeigniter3: "CodeIgniter 3",
  php: "PHP",
  personalizado: "Carpeta pública propia",
};

function phpSelect(site) {
  const sel = h("select", { "aria-label": `PHP de ${site.name}` });
  sel.append(h("option", { value: "" }, `Por defecto (${defaultVersion()})`));
  for (const v of state.versions) {
    const label = v.installed ? v.version : `${v.version} (instalar)`;
    sel.append(h("option", { value: v.version }, label));
  }
  sel.value = site.isolated ? site.php : "";
  sel.addEventListener("change", () =>
    action(sel, async () => {
      const v = sel.value;
      if (!v) return run("unisolate", `--site=${site.name}`);
      const info = state.versions.find((x) => x.version === v);
      if (!info.installed) {
        toast(`Instalando PHP ${v}… (se descarga, puede tardar un poco)`);
        const inst = await runRoot("php:install", v);
        if (!inst.ok) return inst;
      }
      return run("isolate", v, `--site=${site.name}`);
    })
  );
  return sel;
}

function httpsSwitch(site) {
  const input = h("input", { type: "checkbox", "aria-label": `HTTPS de ${site.name}` });
  input.checked = site.secure;
  input.addEventListener("change", () =>
    action(input, () => run(input.checked ? "secure" : "unsecure", site.name))
  );
  return h("label", { class: "switch" }, input, h("span"));
}

function renderSites() {
  const filter = $("#site-filter").value.trim().toLowerCase();
  const body = $("#sites-body");
  const rows = state.sites.filter((s) => !filter || s.name.includes(filter) || s.path.toLowerCase().includes(filter));
  if (!changed("sites", [rows, state.versions])) return;
  body.replaceChildren(
    ...rows.map((s) =>
      h(
        "tr",
        {},
        h(
          "td",
          {},
          h("div", { class: "site-title" }, h("span", { class: "site-name" }, s.name), h("span", { class: "badge", title: s.kind }, KIND_NAMES[s.kind] ?? s.kind),
            s.linked && h("span", { class: "badge" }, "enlace"),
            !s.php_installed && h("span", { class: "badge warn", title: "Usa la versión por defecto" }, `PHP ${s.php} no instalado`)),
          h("div", { class: "site-path" }, s.path)
        ),
        h("td", { class: "url-cell" }, h("a", { class: "link", tabindex: 0, onclick: () => invoke("open_url", { url: s.url }) }, s.url)),
        h("td", {}, phpSelect(s)),
        h("td", {}, httpsSwitch(s)),
        h(
          "td",
          {},
          h("div", { class: "row-actions" },
            h("button", { class: "btn small", onclick: () => invoke("open_path", { path: s.path }) }, "Carpeta"),
            h("button", { class: "btn small", onclick: () => showLogs(s.name) }, "Logs"),
            s.linked && h("button", { class: "btn small danger", onclick: (e) => action(e.currentTarget, () => run("unlink", s.name)) }, "Quitar"))
        )
      )
    )
  );
  $("#sites-empty").classList.toggle("hidden", state.sites.length > 0 || Boolean(state.error));
}

// ------------------------------------------------------------------ PHP ----

const SETTING_HINTS = {
  memory_limit: "Memoria máxima por petición",
  upload_max_filesize: "Tamaño máximo de un archivo subido",
  post_max_size: "Tamaño máximo de un envío (debe ser ≥ la subida)",
  max_execution_time: "Segundos máximos por petición",
  max_input_time: "Segundos máximos para recibir datos",
  max_input_vars: "Campos máximos por formulario",
  display_errors: "Mostrar errores en la página (On/Off)",
  error_reporting: "Nivel de errores (p. ej. E_ALL)",
  "date.timezone": "Zona horaria",
  "opcache.enable": "Caché de código (On/Off)",
  short_open_tag: "Permitir <? como etiqueta de apertura",
};

state.updates = {};
state.phpDetail = null;

async function checkUpdates(button) {
  if (button) button.disabled = true;
  $("#php-updates-note").textContent = "Consultando…";
  try {
    const list = await invoke("php_updates");
    state.updates = Object.fromEntries(list.map((u) => [u.version, u]));
    const pending = list.filter((u) => u.available).length;
    $("#php-updates-note").textContent = pending ? `${pending} actualización${pending > 1 ? "es" : ""} disponible${pending > 1 ? "s" : ""}` : "Todo al día";
  } catch (e) {
    $("#php-updates-note").textContent = "";
    toast(`No pude consultar actualizaciones: ${e}`, true);
  } finally {
    if (button) button.disabled = false;
  }
  delete lastRender.versions;
  renderVersions();
  if (state.phpDetail) renderPhpDetailHeader();
}

function updateButton(version) {
  const u = state.updates[version];
  if (!u?.available) return null;
  return h("button", { class: "btn primary", onclick: (e) => action(e.currentTarget, async () => {
    toast(`Actualizando PHP ${version}… (puede tardar un poco)`);
    const out = await runRoot("php:update", version);
    await checkUpdates();
    if (state.phpDetail === version) await loadPhpDetail();
    return out;
  }) }, `Actualizar a ${u.latest}`);
}

function renderVersions() {
  if (!changed("versions", [state.versions, state.sites.map((s) => s.php), state.updates])) return;
  $("#php-cards").replaceChildren(
    ...state.versions.map((v) => {
      const used = state.sites.filter((s) => s.php === v.version).length;
      const u = state.updates[v.version];
      const buttons = [];
      if (!v.installed) {
        buttons.push(h("button", { class: "btn", onclick: (e) => action(e.currentTarget, () => runRoot("php:install", v.version)) }, "Instalar"));
      } else {
        buttons.push(h("button", { class: "btn", onclick: () => openPhpDetail(v.version) }, "Detalles"));
        if (!v.default) buttons.push(h("button", { class: "btn", onclick: (e) => action(e.currentTarget, () => run("use", v.version)) }, "Usar por defecto"));
        const up = updateButton(v.version);
        if (up) buttons.push(up);
      }
      return h(
        "div",
        { class: v.default ? "card default" : "card" },
        h("div", { class: "version" }, `PHP ${v.version}`),
        h("div", { class: "meta" },
          v.installed ? `${u?.current ? u.current.split("-")[0] + " · " : ""}${PHP_ORIGIN(v.source)}` : "No instalada",
          v.default ? " · por defecto" : "",
          used ? ` · ${used} sitio${used > 1 ? "s" : ""}` : ""),
        u?.available && h("div", {}, h("span", { class: "badge update" }, `Nueva versión: ${u.latest.split("-")[0]}`)),
        h("div", { class: "actions" }, buttons)
      );
    })
  );
}

$("#php-check-updates").addEventListener("click", (e) => checkUpdates(e.currentTarget));

// ---------- detalle de una versión ----------

function openPhpDetail(version) {
  state.phpDetail = version;
  state.phpInfo = null;
  $("#php-list").classList.add("hidden");
  $("#php-detail").classList.remove("hidden");
  $("#pd-title").textContent = `PHP ${version}`;
  $("#pd-sub").textContent = "Cargando…";
  for (const id of ["#pd-settings-rows", "#pd-ext", "#pd-install", "#pd-files"]) $(id).replaceChildren();
  loadPhpDetail();
}

function closePhpDetail() {
  state.phpDetail = null;
  $("#php-detail").classList.add("hidden");
  $("#php-list").classList.remove("hidden");
}
$("#php-back").addEventListener("click", closePhpDetail);

async function loadPhpDetail() {
  const version = state.phpDetail;
  if (!version) return;
  try {
    const info = await invoke("php_info", { version });
    if (state.phpDetail !== version) return; // el usuario ya cambió de vista
    state.phpInfo = info;
    renderPhpDetail();
  } catch (e) {
    $("#pd-sub").textContent = "";
    toast(String(e), true);
  }
}

/** Acción dentro del detalle: al terminar recarga el detalle (y los datos generales). */
function detailAction(button, fn) {
  return action(button, async () => {
    const out = await fn();
    await loadPhpDetail();
    return out;
  });
}

function renderPhpDetailHeader() {
  const info = state.phpInfo;
  if (!info) return;
  $("#pd-title").textContent = `PHP ${info.full_version}`;
  $("#pd-sub").textContent = info.source === "apt" ? "Paquete del sistema (apt)" : WIN ? "Zip oficial de windows.php.net" : "Binario estático de cheka";
  $("#pd-actions").replaceChildren(...[updateButton(info.version)].filter(Boolean));
}

function renderPhpDetail() {
  const info = state.phpInfo;
  renderPhpDetailHeader();

  // Ajustes
  $("#pd-settings-rows").replaceChildren(
    ...info.settings.map((s) => {
      const input = h("input", { type: "text", value: s.value === "(sin definir)" ? "" : s.value, placeholder: "(sin definir)", "data-key": s.key, "data-orig": s.value === "(sin definir)" ? "" : s.value, "aria-label": s.key });
      input.addEventListener("input", () => input.classList.toggle("dirty", input.value !== input.dataset.orig));
      return h("div", { class: "setting-row" },
        h("span", { class: "key" }, s.key, s.custom && h("span", { class: "badge", title: "Cambiado en cheka.toml" }, " propio")),
        input,
        s.custom
          ? h("button", { class: "btn small ghost", type: "button", title: "Volver al valor de cheka", onclick: (e) => detailAction(e.currentTarget, () => run("php:ini", info.version, `${s.key}=`)) }, "Restablecer")
          : h("span"),
        SETTING_HINTS[s.key] && h("span", { class: "hint" }, SETTING_HINTS[s.key]));
    })
  );

  // Extensiones
  renderPhpExtensions();
  $("#pd-ext-note").textContent = info.can_manage_extensions
    ? "Activar o desactivar una extensión solo afecta a cheka; el PHP del sistema no cambia."
    : "Binario estático: sus extensiones vienen compiladas y no se pueden cambiar. Para gestionarlas, usa una versión instalada con apt.";
  $("#pd-install").classList.toggle("hidden", !info.can_manage_extensions || !info.installable.length);
  if (info.can_manage_extensions && info.installable.length) {
    const sel = h("select", { "aria-label": "Extensión para instalar" }, ...info.installable.map((p) => h("option", { value: p }, `php${info.version}-${p}`)));
    $("#pd-install").replaceChildren(sel, h("button", { class: "btn", onclick: (e) => detailAction(e.currentTarget, () => runRoot("php:ext", info.version, "install", sel.value)) }, "Instalar extensión"));
  }

  // Archivos
  $("#pd-files").replaceChildren(...[info.ini_file, ...info.ini_files].filter(Boolean).map((f) => h("li", {}, f)));
  $("#pd-files-summary").textContent = `php.ini + ${info.ini_files.length} archivo${info.ini_files.length === 1 ? "" : "s"} adicional${info.ini_files.length === 1 ? "" : "es"}`;
}

function renderPhpExtensions() {
  const info = state.phpInfo;
  if (!info) return;
  const filter = $("#pd-ext-filter").value.trim().toLowerCase();
  const exts = info.extensions.filter((e) => !filter || e.name.includes(filter));
  $("#pd-ext-count").textContent = `${info.extensions.filter((e) => e.enabled).length} activas`;
  if (!info.can_manage_extensions) {
    $("#pd-ext").replaceChildren(h("div", { class: "chips" }, ...exts.map((e) => h("span", { class: "badge" }, e.name))));
    return;
  }
  $("#pd-ext").replaceChildren(
    ...exts.map((e) => {
      const input = h("input", { type: "checkbox", "aria-label": `Extensión ${e.name}` });
      input.checked = e.enabled;
      input.addEventListener("change", () =>
        detailAction(input, () => run("php:ext", info.version, input.checked ? "enable" : "disable", e.name)));
      return h("div", { class: e.enabled ? "ext" : "ext off" },
        h("span", {}, e.name, e.enabled !== e.loaded && h("span", { class: "pending", title: "Se aplica en unos segundos, al reiniciar ese PHP" }, " · aplicando…")),
        h("label", { class: "switch" }, input, h("span")));
    })
  );
}
$("#pd-ext-filter").addEventListener("input", renderPhpExtensions);

$("#pd-settings").addEventListener("submit", (e) => {
  e.preventDefault();
  const info = state.phpInfo;
  const pairs = [...document.querySelectorAll("#pd-settings-rows input")]
    .filter((i) => i.value.trim() !== i.dataset.orig)
    .map((i) => `${i.dataset.key}=${i.value.trim()}`);
  const key = $("#pd-new-key").value.trim();
  if (key) pairs.push(`${key}=${$("#pd-new-value").value.trim()}`);
  if (!pairs.length) return toast("No hay cambios que guardar");
  detailAction(e.submitter, async () => {
    const out = await run("php:ini", info.version, ...pairs);
    if (out.ok) {
      $("#pd-new-key").value = "";
      $("#pd-new-value").value = "";
    }
    return out;
  });
});

// ------------------------------------------------------------------ servicios ----

const STATE_NAMES = { active: "activo", inactive: "detenido", failed: "con error", activating: "iniciando", deactivating: "deteniéndose" };

function humanBytes(b) {
  return b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.round(b / (1 << 20))} MB`;
}

function humanDuration(s) {
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
  return `${Math.floor(s / 86400)} d ${Math.floor((s % 86400) / 3600)} h`;
}

/** Acción de root sobre un servicio; al terminar recarga los detalles. */
function serviceAction(button, id, verb) {
  return action(button, async () => {
    const out = await runRoot("service", id, verb);
    await loadServices();
    return out;
  });
}

async function loadServices() {
  try {
    state.services = await invoke("services");
  } catch (e) {
    state.error = String(e);
  }
  renderServices();
}

function renderServices() {
  const list = state.services;
  if (!list || !changed("services", list.map(({ uptime_secs, memory_bytes, ...rest }) => rest))) {
    // Tiempo activo y memoria cambian siempre: se actualizan sin redibujar las tarjetas.
    for (const s of list ?? []) {
      const el = document.querySelector(`[data-service="${CSS.escape(s.id)}"]`);
      if (!el) continue;
      el.querySelector(".uptime")?.replaceChildren(s.uptime_secs != null ? `hace ${humanDuration(s.uptime_secs)}` : "—");
      el.querySelector(".memory")?.replaceChildren(s.memory_bytes != null ? humanBytes(s.memory_bytes) : "—");
    }
    return;
  }
  $("#service-cards").replaceChildren(
    ...list.map((s) => {
      const active = s.state === "active";
      const boot = h("input", { type: "checkbox", "aria-label": `${s.label}: iniciar con el sistema` });
      boot.checked = s.enabled;
      boot.addEventListener("change", () => serviceAction(boot, s.id, boot.checked ? "enable" : "disable"));
      const buttons = active
        ? [h("button", { class: "btn small", onclick: (e) => serviceAction(e.currentTarget, s.id, "restart") }, "Reiniciar"),
           h("button", { class: "btn small danger", onclick: (e) => serviceAction(e.currentTarget, s.id, "stop") }, "Detener")]
        : [h("button", { class: "btn small primary", onclick: (e) => serviceAction(e.currentTarget, s.id, "start") }, "Iniciar")];
      buttons.push(h("button", { class: "btn small", onclick: () => showServiceLogs(s) }, "Logs"));
      if (s.config) buttons.push(h("button", { class: "btn small", title: s.config, onclick: () => invoke("open_path", { path: s.config }).catch((e) => toast(String(e), true)) }, "Configuración"));
      return h("div", { class: "card", "data-service": s.id },
        h("div", { class: "service-head" },
          h("span", { class: "dot " + (active ? "ok" : "bad") }),
          h("h3", {}, s.label),
          h("span", { class: `state-badge ${s.state}` }, STATE_NAMES[s.state] ?? s.state)),
        h("dl", { class: "facts" },
          h("dt", {}, "Versión"), h("dd", {}, s.version ?? "—"),
          h("dt", {}, "Escucha"), h("dd", {}, s.listen.join(", ") || "—"),
          h("dt", {}, "Activo"), h("dd", { class: "uptime" }, s.uptime_secs != null ? `hace ${humanDuration(s.uptime_secs)}` : "—"),
          h("dt", {}, "Memoria"), h("dd", { class: "memory" }, s.memory_bytes != null ? humanBytes(s.memory_bytes) : "—"),
          h("dt", {}, "PID"), h("dd", {}, s.pid ?? "—")),
        h("label", { class: "boot" }, h("span", { class: "switch" }, boot, h("span")), "Iniciar con el sistema"),
        h("div", { class: "actions" }, buttons));
    })
  );
}

// ---------- logs de un servicio ----------

state.logService = null;

async function showServiceLogs(service) {
  state.logService = service;
  $("#service-logs").classList.remove("hidden");
  $("#service-logs-title").textContent = `Logs de ${service.label}`;
  $("#service-logs-body").replaceChildren(h("p", { class: "muted" }, "Cargando…"));
  $("#service-logs").scrollIntoView({ behavior: "smooth", block: "start" });
  try {
    const logs = await invoke("service_logs", { id: service.id });
    const block = (title, lines) => [
      h("h3", {}, title),
      h("pre", { class: "console" }, lines.length ? lines.join("\n") : "(vacío)"),
    ];
    $("#service-logs-body").replaceChildren(
      ...(WIN && !logs.journal.length ? [] : block("journal de systemd", logs.journal)),
      ...logs.files.flatMap((f) => block(f.file, f.lines))
    );
    for (const pre of $("#service-logs-body").querySelectorAll("pre")) pre.scrollTop = pre.scrollHeight;
  } catch (e) {
    $("#service-logs-body").replaceChildren(h("p", { class: "muted" }, String(e)));
  }
}
$("#service-logs-refresh").addEventListener("click", () => state.logService && showServiceLogs(state.logService));
$("#service-logs-close").addEventListener("click", () => {
  state.logService = null;
  $("#service-logs").classList.add("hidden");
});

for (const btn of document.querySelectorAll("[data-root]")) {
  btn.addEventListener("click", () =>
    action(btn, async () => {
      const out = await runRoot(btn.dataset.root);
      await loadServices();
      return out;
    }));
}

// ------------------------------------------------------------------ nuevo proyecto ----

function renderNewForm() {
  const sel = $("#new-php");
  if (sel.options.length && sel.dataset.sig === JSON.stringify(installedVersions())) return;
  const current = sel.value;
  sel.replaceChildren(h("option", { value: "" }, `Por defecto (${defaultVersion()})`), ...installedVersions().map((v) => h("option", { value: v.version }, v.version)));
  sel.value = current;
  sel.dataset.sig = JSON.stringify(installedVersions());
}

function updateNewFormVisibility() {
  const wp = $("#new-type").value === "wordpress";
  for (const el of document.querySelectorAll(".wp-only")) el.classList.toggle("hidden", !wp);
  const name = $("#new-name").value.trim().toLowerCase().replace(/[^a-z0-9-]/g, "-");
  $("#new-url-preview").textContent = name ? `${$("#new-secure").checked ? "https" : "http"}://${name}.test` : "";
}
["#new-type", "#new-name", "#new-secure"].forEach((id) => $(id).addEventListener("input", updateNewFormVisibility));

$("#new-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (state.creating) return;
  const type = $("#new-type").value;
  const args = [type, $("#new-name").value.trim()];
  if ($("#new-php").value) args.push(`--php=${$("#new-php").value}`);
  if ($("#new-secure").checked) args.push("--secure");
  if (type === "wordpress") {
    if ($("#new-multisite").value) args.push(`--multisite=${$("#new-multisite").value}`);
    if ($("#new-locale").value.trim()) args.push(`--locale=${$("#new-locale").value.trim()}`);
  }
  const consoleEl = $("#new-console");
  consoleEl.replaceChildren();
  consoleEl.classList.remove("hidden");
  state.creating = true;
  $("#new-submit").disabled = true;
  $("#new-submit").textContent = "Creando…";
  try {
    await invoke("new_project", { args });
  } catch (err) {
    finishNew(false, String(err));
  }
});

function finishNew(ok, message) {
  state.creating = false;
  $("#new-submit").disabled = false;
  $("#new-submit").textContent = "Crear proyecto";
  toast(message ?? (ok ? "Proyecto creado" : "La creación falló; revisa la salida"), !ok);
  reload();
}

listen("new-output", ({ payload }) => {
  const consoleEl = $("#new-console");
  consoleEl.append(h("span", { class: payload.error ? "err" : "" }, payload.line + "\n"));
  consoleEl.scrollTop = consoleEl.scrollHeight;
});
listen("new-done", ({ payload }) => finishNew(payload));
listen("services-restarted", ({ payload }) => {
  toast(payload ? "Servicios reiniciados" : "No se reiniciaron los servicios", !payload);
  reload();
});

// ------------------------------------------------------------------ logs ----

function renderLogSites() {
  const sel = $("#log-site");
  const names = state.sites.map((s) => s.name);
  if (sel.dataset.sig === names.join(",")) return;
  const current = sel.value;
  sel.replaceChildren(...names.map((n) => h("option", { value: n }, n)));
  if (names.includes(current)) sel.value = current;
  sel.dataset.sig = names.join(",");
}

async function loadLogs() {
  const site = state.sites.find((s) => s.name === $("#log-site").value);
  for (const id of ["#log-apache", "#log-php"]) $(id).textContent = site ? "" : "Elige un sitio.";
  if (!site) return;
  try {
    const logs = await invoke("read_log", { site: site.name, php: site.php });
    for (const [id, log] of [["#log-apache", logs.apache], ["#log-php", logs.php]]) {
      $(id).textContent = log.lines.length ? log.lines.join("\n") : `(vacío) ${log.file}`;
      $(id).scrollTop = $(id).scrollHeight;
    }
  } catch (e) {
    toast(String(e), true);
  }
}

function showLogs(name) {
  showPage("logs");
  $("#log-site").value = name;
  loadLogs();
}

$("#log-site").addEventListener("change", loadLogs);
$("#log-refresh").addEventListener("click", loadLogs);

// ------------------------------------------------------------------ enlazar carpeta ----

$("#btn-link").addEventListener("click", () => {
  $("#link-form").classList.remove("hidden");
  $("#link-path").focus();
});
$("#link-cancel").addEventListener("click", () => $("#link-form").classList.add("hidden"));
$("#link-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const btn = e.submitter;
  action(btn, async () => {
    const out = await invoke("link_folder", { path: $("#link-path").value.trim(), name: $("#link-name").value });
    if (out.ok) {
      $("#link-form").reset();
      $("#link-form").classList.add("hidden");
    }
    return out;
  });
});

// ------------------------------------------------------------------ herramientas ----

state.tools = null;
state.installingTools = false;
state.toolInfo = {};     // id → { version, path, packages, size_bytes? }
state.toolOpen = new Set();

async function loadTools() {
  try {
    state.tools = await invoke("tools");
  } catch (e) {
    toast(String(e), true);
    return;
  }
  renderTools();
  // Versiones y carpetas: tardan un par de segundos, se completan después.
  try {
    for (const info of await invoke("tools_info", { ids: [], size: false })) {
      state.toolInfo[info.id] = { ...state.toolInfo[info.id], ...info, size_bytes: state.toolInfo[info.id]?.size_bytes };
    }
    renderTools();
  } catch (e) {
    console.warn(e);
  }
}

async function toggleToolDetails(id) {
  if (state.toolOpen.has(id)) state.toolOpen.delete(id);
  else state.toolOpen.add(id);
  renderTools();
  if (state.toolOpen.has(id) && state.toolInfo[id]?.size_bytes == null) {
    try {
      const [info] = await invoke("tools_info", { ids: [id], size: true });
      state.toolInfo[id] = { ...info, size_bytes: info.size_bytes ?? -1 };
      renderTools();
    } catch (e) {
      toast(String(e), true);
    }
  }
}

function toolDetails(t) {
  const info = state.toolInfo[t.id] ?? {};
  const size = info.size_bytes == null ? "calculando…" : info.size_bytes < 0 ? "—" : humanBytes(info.size_bytes);
  return h("div", { class: "details", onclick: (e) => e.preventDefault() },
    h("dl", { class: "facts" },
      h("dt", {}, "Versión"), h("dd", {}, info.version ?? "—"),
      h("dt", {}, "Ubicación"), h("dd", {}, info.path ?? "—"),
      h("dt", {}, "Tamaño"), h("dd", {}, size),
      info.packages?.length && [h("dt", {}, "Paquetes"), h("dd", {}, info.packages.map((p) => `${p.name} ${p.version}`).join(", "))]),
    info.path && h("button", { class: "btn small", onclick: (e) => { e.preventDefault(); invoke("open_path", { path: info.path }).catch((err) => toast(String(err), true)); } }, "Abrir carpeta"),
    h("div", { class: "desc", style: "margin-top:6px" }, "Volver a instalarla la actualiza a la última versión."));
}

function selectedTools() {
  return [...document.querySelectorAll("#tools-list input:checked")].map((i) => i.value);
}

function updateToolsButton() {
  const n = selectedTools().length;
  const btn = $("#tools-install");
  // Solo se muestra si hay algo marcado (o mientras instala).
  btn.classList.toggle("hidden", n === 0 && !state.installingTools);
  btn.disabled = state.installingTools;
  btn.textContent = state.installingTools ? "Instalando…" : `Instalar seleccionadas (${n})`;
}

function showToolsOutput(show) {
  $("#tools-output").classList.toggle("hidden", !show);
}
$("#tools-output-close").addEventListener("click", () => showToolsOutput(false));

function renderTools() {
  const tools = state.tools ?? [];
  const keep = new Set(selectedTools());
  const groups = new Map();
  for (const t of tools) {
    if (!groups.has(t.category)) groups.set(t.category, []);
    groups.get(t.category).push(t);
  }
  $("#tools-list").replaceChildren(
    ...[...groups].map(([category, list]) =>
      h("section", { class: "tool-group" },
        h("h2", {}, category),
        h("div", { class: "tool-grid" },
          ...list.map((t) => {
            const box = h("input", { type: "checkbox", value: t.id, disabled: state.installingTools });
            box.checked = keep.has(t.id);
            box.addEventListener("change", updateToolsButton);
            return h("label", { class: "tool" },
              box,
              h("div", {},
                t.installed && h("button", { class: "btn small ghost more", onclick: (e) => { e.preventDefault(); toggleToolDetails(t.id); } },
                  state.toolOpen.has(t.id) ? "Ocultar" : "Detalles"),
                h("div", {}, h("span", { class: "name" }, t.name), " ",
                  t.installed && h("span", { class: "badge ok" }, "instalada"),
                  t.needs_root && !t.installed && h("span", { class: "badge", title: WIN ? "Pide permisos de administrador" : "Pide tu contraseña" }, "sistema")),
                t.installed && state.toolInfo[t.id] && h("div", { class: "meta", title: state.toolInfo[t.id].path ?? "" },
                  h("span", { class: "ver" }, state.toolInfo[t.id].version ?? "?"),
                  state.toolInfo[t.id].path ? ` · ${state.toolInfo[t.id].path}` : ""),
                t.description && h("div", { class: "desc" }, t.description),
                state.toolOpen.has(t.id) && toolDetails(t)));
          })))
    )
  );
  updateToolsButton();
}

$("#tools-install").addEventListener("click", async () => {
  const ids = selectedTools();
  if (!ids.length || state.installingTools) return;
  state.installingTools = true;
  renderTools();
  const consoleEl = $("#tools-console");
  consoleEl.replaceChildren();
  $("#tools-output-title").textContent = "Instalando…";
  $("#tools-output-close").classList.add("hidden");
  showToolsOutput(true);
  try {
    await invoke("install_tools", { ids });
  } catch (e) {
    state.installingTools = false;
    renderTools();
    toast(String(e), true);
  }
});

listen("tools-output", ({ payload }) => {
  const consoleEl = $("#tools-console");
  consoleEl.append(h("span", { class: payload.error ? "err" : "" }, payload.line + "\n"));
  consoleEl.scrollTop = consoleEl.scrollHeight;
});

listen("tools-done", ({ payload }) => {
  state.installingTools = false;
  const names = (ids) => ids.map((id) => state.tools?.find((t) => t.id === id)?.name ?? id).join(", ");
  if (payload.failed.length) toast(`Con errores: ${names(payload.failed)}. Revisa la salida.`, true);
  if (payload.ok.length) toast(`Instalado: ${names(payload.ok)}. Abre una terminal nueva para cargar el PATH.`);
  $("#tools-output-close").classList.remove("hidden");
  if (payload.failed.length) {
    // Con errores, la salida se queda para poder leerla.
    $("#tools-output-title").textContent = `Con errores: ${names(payload.failed)}`;
  } else {
    $("#tools-output-title").textContent = "Instalación terminada";
    setTimeout(() => { if (!state.installingTools) showToolsOutput(false); }, 2500);
  }
  for (const box of document.querySelectorAll("#tools-list input")) box.checked = false;
  for (const id of [...payload.ok, ...payload.failed]) delete state.toolInfo[id]; // versión nueva
  loadTools();
});

// ------------------------------------------------------------------ navegación ----

function showPage(page) {
  state.page = page;
  for (const el of document.querySelectorAll(".nav-item")) el.classList.toggle("active", el.dataset.page === page);
  for (const el of document.querySelectorAll(".page")) el.classList.toggle("active", el.id === `page-${page}`);
  if (page === "logs") loadLogs();
  if (page === "services") loadServices();
  if (page === "tools" && !state.tools) loadTools();
  if (page === "php") {
    if (state.phpDetail) loadPhpDetail();
    if (!Object.keys(state.updates).length) checkUpdates();
  }
}

for (const el of document.querySelectorAll(".nav-item")) el.addEventListener("click", () => showPage(el.dataset.page));
$("#site-filter").addEventListener("input", renderSites);

// Mantener los datos al día mientras la ventana está visible (el daemon publica solo).
setInterval(() => {
  if (document.visibilityState === "visible" && !state.creating) reload();
}, 4000);
window.addEventListener("focus", reload);

updateNewFormVisibility();
reload();
