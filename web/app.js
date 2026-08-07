"use strict";

const TOKEN_STORAGE = "screen-memory-tokens";
const VERIFIER_STORAGE = "screen-memory-code-verifier";
const LANG_STORAGE = "screen-memory-lang";

// ---- Internationalization ----
// Static texts carry a data-i18n attribute in index.html; dynamic texts go through t().
// No stored choice (or "auto") follows the browser language.

const MESSAGES = {
  ja: {
    tabRecords: "記録",
    tabReport: "日報",
    logout: "ログアウト",
    device: "端末",
    all: "すべて",
    downloadJson: "JSONダウンロード",
    thTime: "時刻",
    thDevice: "端末",
    thApp: "アプリ",
    thCategory: "分類",
    thSource: "解析元",
    thSummary: "要約",
    srcImage: "画像",
    srcLocalOcr: "ローカルOCR",
    count: "{shown} / {total}件",
    loadMore: "さらに読み込む（残り{n}件）",
    updated: "自動更新 {time}",
    noRecords: "記録なし",
    noReport: "日報なし",
    ariaPrevDay: "前日",
    ariaNextDay: "翌日",
    errToken: "トークン取得に失敗（{status}）",
    errRecords: "記録の取得に失敗（{status}）",
    errReport: "日報の取得に失敗（{status}）",
    errNetwork: "通信エラー",
    errInit: "初期化に失敗した",
  },
  en: {
    tabRecords: "Records",
    tabReport: "Report",
    logout: "Log out",
    device: "Device",
    all: "All",
    downloadJson: "Download JSON",
    thTime: "Time",
    thDevice: "Device",
    thApp: "App",
    thCategory: "Category",
    thSource: "Source",
    thSummary: "Summary",
    srcImage: "Image",
    srcLocalOcr: "Local OCR",
    count: "{shown} / {total}",
    loadMore: "Load more ({n} remaining)",
    updated: "Auto-refreshed {time}",
    noRecords: "No records",
    noReport: "No report",
    ariaPrevDay: "Previous day",
    ariaNextDay: "Next day",
    errToken: "Token request failed ({status})",
    errRecords: "Failed to fetch records ({status})",
    errReport: "Failed to fetch the report ({status})",
    errNetwork: "Network error",
    errInit: "Initialization failed",
  },
};

function detectLang() {
  const stored = localStorage.getItem(LANG_STORAGE);
  if (stored === "ja" || stored === "en") return stored;
  return (navigator.language || "").toLowerCase().startsWith("ja") ? "ja" : "en";
}

function t(key, vars = {}) {
  let text = MESSAGES[state.lang][key] ?? key;
  for (const [name, value] of Object.entries(vars)) {
    text = text.replaceAll(`{${name}}`, value);
  }
  return text;
}

function applyLanguage() {
  document.documentElement.lang = state.lang;
  for (const el of document.querySelectorAll("[data-i18n]")) {
    el.textContent = t(el.dataset.i18n);
  }
  for (const el of document.querySelectorAll("[data-i18n-aria]")) {
    el.setAttribute("aria-label", t(el.dataset.i18nAria));
  }
}

/** How many rows the records tab shows initially, and how many each "load more" adds */
const PAGE_SIZE = 10;
/** Auto-refresh interval in milliseconds. One capture per minute, so 30 seconds is enough */
const POLL_INTERVAL = 30_000;

const state = {
  lang: detectLang(),
  endpoint: null,
  auth: null, // { cognitoDomain, clientId }
  date: jstToday(),
  tab: "records",
  records: [],
  /** How many records the records tab currently shows (newest first) */
  visibleCount: PAGE_SIZE,
  /** Keys of records added by the latest auto-refresh; highlighted once */
  freshKeys: new Set(),
  pollTimer: null,
};

const $ = (id) => document.getElementById(id);

function jstToday() {
  return new Date(Date.now() + 9 * 3600 * 1000).toISOString().slice(0, 10);
}

function shiftDate(date, days) {
  const d = new Date(`${date}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

function showError(message) {
  const banner = $("error");
  banner.textContent = message;
  banner.hidden = !message;
}

// ---- Authentication (Cognito authorization code grant + PKCE) ----

function redirectUri() {
  return location.origin + "/";
}

function loadTokens() {
  try {
    return JSON.parse(localStorage.getItem(TOKEN_STORAGE));
  } catch {
    return null;
  }
}

function saveTokens(tokens) {
  localStorage.setItem(TOKEN_STORAGE, JSON.stringify(tokens));
}

function clearTokens() {
  localStorage.removeItem(TOKEN_STORAGE);
}

function jwtExp(token) {
  const payload = token.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
  return JSON.parse(atob(payload)).exp;
}

function base64url(bytes) {
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

async function login() {
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  sessionStorage.setItem(VERIFIER_STORAGE, verifier);
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(verifier),
  );
  const params = new URLSearchParams({
    response_type: "code",
    client_id: state.auth.clientId,
    redirect_uri: redirectUri(),
    scope: "openid",
    code_challenge: base64url(new Uint8Array(digest)),
    code_challenge_method: "S256",
  });
  location.assign(`${state.auth.cognitoDomain}/oauth2/authorize?${params}`);
}

async function tokenRequest(params) {
  const response = await fetch(`${state.auth.cognitoDomain}/oauth2/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: state.auth.clientId, ...params }),
  });
  if (!response.ok) throw new Error(t("errToken", { status: response.status }));
  return response.json();
}

// Returning false means a redirect to the Hosted UI is under way, so stop initializing
async function ensureLogin() {
  const code = new URLSearchParams(location.search).get("code");
  if (code) {
    const verifier = sessionStorage.getItem(VERIFIER_STORAGE);
    sessionStorage.removeItem(VERIFIER_STORAGE);
    const data = await tokenRequest({
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri(),
      code_verifier: verifier,
    });
    saveTokens({ idToken: data.id_token, refreshToken: data.refresh_token });
    history.replaceState(null, "", location.pathname);
    return true;
  }
  if (loadTokens()) return true;
  await login();
  return false;
}

// A valid ID token. Refreshes it once we are within 60 seconds of expiry
async function idToken() {
  const tokens = loadTokens();
  if (!tokens) return null;
  if (jwtExp(tokens.idToken) - Date.now() / 1000 > 60) return tokens.idToken;
  try {
    const data = await tokenRequest({
      grant_type: "refresh_token",
      refresh_token: tokens.refreshToken,
    });
    saveTokens({ ...tokens, idToken: data.id_token });
    return data.id_token;
  } catch {
    return null;
  }
}

function logout() {
  clearTokens();
  const params = new URLSearchParams({
    client_id: state.auth.clientId,
    logout_uri: redirectUri(),
  });
  location.assign(`${state.auth.cognitoDomain}/logout?${params}`);
}

async function callApi(path) {
  const token = await idToken();
  if (!token) {
    clearTokens();
    await login();
    return null;
  }
  const response = await fetch(state.endpoint + path, {
    headers: { authorization: `Bearer ${token}` },
  });
  if (response.status === 401) {
    clearTokens();
    await login();
    return null;
  }
  return response;
}

// ---- Records tab ----

/** Key that uniquely identifies a record: one device has only one record per timestamp */
function recordKey(record) {
  return record.imageKey || `${record.device}|${record.capturedAt}`;
}

// silent = auto-refresh. Grow the visible count by the number of new rows so the
// rows already on screen are not pushed out
async function loadRecords({ silent = false } = {}) {
  const response = await callApi(`/days/${state.date}/records`);
  if (!response) return;
  if (!response.ok) throw new Error(t("errRecords", { status: response.status }));
  const data = await response.json();
  const known = new Set(state.records.map(recordKey));
  const fresh = data.records.filter((r) => !known.has(recordKey(r)));
  state.records = data.records;
  if (silent) {
    state.freshKeys = new Set(fresh.map(recordKey));
    state.visibleCount += fresh.length;
  } else {
    state.freshKeys = new Set();
  }
  buildDeviceFilter();
  renderRecords();
  markUpdated();
}

function buildDeviceFilter() {
  const select = $("device-filter");
  const selected = select.value;
  const devices = [...new Set(state.records.map((r) => r.device))].sort();
  select.innerHTML = "";
  select.append(new Option(t("all"), ""));
  for (const device of devices) select.append(new Option(device, device));
  if (devices.includes(selected)) select.value = selected;
}

/** Records filtered by device and sorted newest first */
function visibleRecords() {
  const device = $("device-filter").value;
  const records = device
    ? state.records.filter((r) => r.device === device)
    : [...state.records];
  return records.sort((a, b) =>
    (b.capturedAt || "").localeCompare(a.capturedAt || ""),
  );
}

function renderRecords() {
  const records = visibleRecords();
  const shown = records.slice(0, state.visibleCount);
  const tbody = $("records-table").querySelector("tbody");
  tbody.innerHTML = "";
  for (const record of shown) {
    const tr = document.createElement("tr");
    if (state.freshKeys.has(recordKey(record))) tr.classList.add("fresh");
    const cells = [
      (record.capturedAt || "").slice(11, 19),
      record.device,
      record.app,
      record.category,
      // A record analyzed from an uploaded image carries its imageKey; local OCR mode sends text only
      record.imageKey ? t("srcImage") : t("srcLocalOcr"),
      record.summary,
    ];
    for (const value of cells) {
      const td = document.createElement("td");
      td.textContent = value ?? "";
      tr.append(td);
    }
    tbody.append(tr);
  }
  const remaining = records.length - shown.length;
  $("records-count").textContent = t("count", {
    shown: shown.length,
    total: records.length,
  });
  $("load-more").hidden = remaining === 0;
  $("load-more").textContent = t("loadMore", { n: remaining });
  $("records-table").hidden = records.length === 0;
  $("records-empty").hidden = records.length !== 0;
}

function loadMore() {
  state.visibleCount += PAGE_SIZE;
  state.freshKeys = new Set();
  renderRecords();
}

function downloadRecords() {
  const json = JSON.stringify({ date: state.date, records: state.records }, null, 2);
  const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `records-${state.date}.json`;
  a.click();
  URL.revokeObjectURL(url);
}

// ---- Daily report tab ----

async function loadReport() {
  const body = $("report-body");
  body.innerHTML = "";
  const response = await callApi(`/days/${state.date}/report`);
  if (!response) return;
  if (response.status === 404) {
    $("report-empty").hidden = false;
    return;
  }
  if (!response.ok) throw new Error(t("errReport", { status: response.status }));
  const data = await response.json();
  $("report-empty").hidden = true;
  body.innerHTML = renderMarkdown(data.markdown);
}

// ---- Minimal Markdown renderer ----
// Handles only the elements that appear in a daily report. Everything is escaped
// before conversion.

function escapeHtml(text) {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function inline(text) {
  return text
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
}

function renderMarkdown(markdown) {
  const lines = escapeHtml(markdown).split("\n");
  const html = [];
  let list = null; // "ul" | "ol" | null
  let table = false;
  let fence = false;

  const closeBlocks = () => {
    if (list) { html.push(`</${list}>`); list = null; }
    if (table) { html.push("</tbody></table>"); table = false; }
  };

  for (const line of lines) {
    if (line.startsWith("```")) {
      closeBlocks();
      html.push(fence ? "</code></pre>" : "<pre><code>");
      fence = !fence;
      continue;
    }
    if (fence) { html.push(line); continue; }

    const heading = line.match(/^(#{1,4}) +(.*)/);
    if (heading) {
      closeBlocks();
      const level = heading[1].length;
      html.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      continue;
    }

    const item = line.match(/^[-*] +(.*)/) || line.match(/^\d+\. +(.*)/);
    if (item) {
      const kind = /^[-*]/.test(line) ? "ul" : "ol";
      if (list !== kind) { closeBlocks(); html.push(`<${kind}>`); list = kind; }
      html.push(`<li>${inline(item[1])}</li>`);
      continue;
    }

    if (/^\|.*\|\s*$/.test(line)) {
      const cells = line.replace(/^\||\|\s*$/g, "").split("|").map((c) => c.trim());
      if (cells.every((c) => /^:?-+:?$/.test(c))) continue; // separator row
      if (!table) {
        closeBlocks();
        html.push("<table><tbody>");
        table = true;
        html.push(`<tr>${cells.map((c) => `<th>${inline(c)}</th>`).join("")}</tr>`);
      } else {
        html.push(`<tr>${cells.map((c) => `<td>${inline(c)}</td>`).join("")}</tr>`);
      }
      continue;
    }

    closeBlocks();
    if (line.trim() !== "") html.push(`<p>${inline(line)}</p>`);
  }
  closeBlocks();
  if (fence) html.push("</code></pre>");
  return html.join("\n");
}

// ---- Auto-refresh ----
// Records arrive every minute, so refetch at a fixed interval to keep up without a reload.
// Stop when no records can arrive (a past date, the report tab, a hidden tab) to avoid
// pointless calls.

function autoUpdatable() {
  return state.tab === "records" && state.date === jstToday();
}

function markUpdated() {
  $("updated-at").textContent = autoUpdatable()
    ? t("updated", {
        time: new Date().toLocaleTimeString(state.lang === "ja" ? "ja-JP" : "en-US"),
      })
    : "";
}

function startPolling() {
  stopPolling();
  if (autoUpdatable()) state.pollTimer = setInterval(poll, POLL_INTERVAL);
}

function stopPolling() {
  clearInterval(state.pollTimer);
  state.pollTimer = null;
}

async function poll() {
  if (document.hidden || !autoUpdatable()) return;
  try {
    await loadRecords({ silent: true });
  } catch {
    // Leave transient failures to the next cycle instead of showing a banner
  }
}

// ---- View control ----

async function refresh() {
  showError("");
  state.visibleCount = PAGE_SIZE;
  state.freshKeys = new Set();
  $("records-empty").hidden = true;
  $("report-empty").hidden = true;
  $("records-view").hidden = state.tab !== "records";
  $("report-view").hidden = state.tab !== "report";
  markUpdated();
  try {
    if (state.tab === "records") await loadRecords();
    else await loadReport();
  } catch (error) {
    showError(error.message || t("errNetwork"));
  }
  startPolling();
}

function setTab(tab) {
  state.tab = tab;
  $("tab-records").classList.toggle("active", tab === "records");
  $("tab-report").classList.toggle("active", tab === "report");
  refresh();
}

async function init() {
  applyLanguage();
  const config = await fetch("config.json").then((r) => r.json());
  state.endpoint = config.apiEndpoint;
  state.auth = { cognitoDomain: config.cognitoDomain, clientId: config.clientId };
  if (!(await ensureLogin())) return;

  $("lang-select").value = localStorage.getItem(LANG_STORAGE) || "auto";
  $("lang-select").addEventListener("change", () => {
    const choice = $("lang-select").value;
    if (choice === "auto") localStorage.removeItem(LANG_STORAGE);
    else localStorage.setItem(LANG_STORAGE, choice);
    state.lang = detectLang();
    applyLanguage();
    buildDeviceFilter();
    renderRecords();
    markUpdated();
  });
  $("date").value = state.date;
  $("date").addEventListener("change", () => {
    if (!$("date").value) return;
    state.date = $("date").value;
    refresh();
  });
  $("prev-day").addEventListener("click", () => {
    state.date = shiftDate(state.date, -1);
    $("date").value = state.date;
    refresh();
  });
  $("next-day").addEventListener("click", () => {
    state.date = shiftDate(state.date, 1);
    $("date").value = state.date;
    refresh();
  });
  $("device-filter").addEventListener("change", () => {
    state.visibleCount = PAGE_SIZE;
    renderRecords();
  });
  $("load-more").addEventListener("click", loadMore);
  $("download-json").addEventListener("click", downloadRecords);
  // Pause while the tab is out of view, and catch up as soon as it comes back
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) poll();
  });
  $("tab-records").addEventListener("click", () => setTab("records"));
  $("tab-report").addEventListener("click", () => setTab("report"));
  $("logout").addEventListener("click", logout);

  refresh();
}

init().catch((error) => showError(error.message || t("errInit")));
