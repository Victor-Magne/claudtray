// CloudTray dashboard logic. Receives snapshots from Rust via window.updateData
// and sends user actions back through the wry IPC bridge.

let DATA = null;
let activeProvider = null;
let themePref = "dark";

const STATUS_LABEL = {
  healthy: "HEALTHY",
  warning: "WARNING",
  critical: "LOW",
  depleted: "EMPTY",
};

// NOTE: wry reserves `window.ipc` for its bridge, so we must use a
// different name to avoid "Identifier 'ipc' has already been declared".
function sendIpc(msg) {
  try {
    window.ipc.postMessage(JSON.stringify(msg));
  } catch (e) {
    /* not running inside wry (preview) */
  }
}

// ---- Theme ----
function applyTheme(pref) {
  themePref = pref || "dark";
  let resolved = themePref;
  if (themePref === "system") {
    resolved =
      window.matchMedia &&
      window.matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark";
  }
  document.documentElement.setAttribute("data-theme", resolved);
  // reflect in settings segmented control
  document.querySelectorAll("#theme-seg .tab").forEach((el) => {
    el.classList.toggle("active", el.dataset.themeValue === themePref);
  });
}

// ---- Data entry point (called from Rust) ----
window.updateData = function (snapshot) {
  DATA = snapshot;
  if (snapshot.theme) applyTheme(snapshot.theme);

  const ids = (DATA.providers || []).map((p) => p.id);
  if (!activeProvider || !ids.includes(activeProvider)) {
    const firstAvail = (DATA.providers || []).find((p) => p.available);
    activeProvider = firstAvail ? firstAvail.id : ids[0] || null;
  }
  render();
};

// ---- Rendering ----
function render() {
  if (!DATA) return;
  renderGlobal();
  renderTabs();
  renderCards();
  renderUpdated();
  // a fresh snapshot always follows a refresh — restore the button label
  document.getElementById("btn-refresh").innerHTML = "↻ Atualizar";
}

function renderGlobal() {
  let worst = "healthy";
  const rank = { healthy: 0, warning: 1, depleted: 2, critical: 3 };
  for (const p of DATA.providers) {
    if (!p.available) continue;
    for (const w of p.windows) {
      if (rank[w.status] > rank[worst]) worst = w.status;
    }
  }
  document.getElementById("global-dot").className = "dot s-" + worst;
  document.getElementById("global-status").textContent = STATUS_LABEL[worst];
}

function renderTabs() {
  const wrap = document.getElementById("tabs");
  wrap.innerHTML = "";
  for (const p of DATA.providers) {
    const el = document.createElement("div");
    el.className = "tab" + (p.id === activeProvider ? " active" : "");
    el.textContent = p.name;
    if (!p.available) el.style.opacity = "0.55";
    el.onclick = () => {
      activeProvider = p.id;
      render();
    };
    wrap.appendChild(el);
  }
}

function renderCards() {
  const wrap = document.getElementById("cards");
  wrap.innerHTML = "";
  const p = DATA.providers.find((x) => x.id === activeProvider);
  if (!p) return;

  const hasWindows = p.windows && p.windows.length > 0;
  const hasModels = p.local_models && p.local_models.length > 0;

  if (!p.available || (!hasWindows && !hasModels)) {
    const div = document.createElement("div");
    div.className = "empty";
    div.innerHTML =
      '<div class="big">⌀</div><div>' +
      (p.note || "Sem dados disponíveis") +
      "</div>";
    wrap.appendChild(div);
    return;
  }

  if (hasWindows) {
    p.windows.forEach((w) => {
      const wide = w.key === "opus" || p.windows.length === 1;
      wrap.appendChild(card(w, wide));
    });
  }

  if (p.total_tokens != null && p.total_tokens > 0) {
    wrap.appendChild(totalTokensCard(p.total_tokens));
  }

  if (hasModels) {
    p.local_models.forEach((m) => wrap.appendChild(localModelCard(m)));
  }
}

function formatTokens(n) {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return n.toString();
}

function totalTokensCard(tokens) {
  const el = document.createElement("div");
  el.className = "card wide";

  const top = document.createElement("div");
  top.className = "top";
  const lbl = document.createElement("span");
  lbl.className = "wlabel";
  lbl.textContent = "TOKENS GASTOS (30d)";
  top.appendChild(lbl);

  const val = document.createElement("div");
  val.className = "pct";
  val.style.fontSize = "26px";
  val.textContent = formatTokens(tokens);

  el.appendChild(top);
  el.appendChild(val);
  return el;
}

function localModelCard(m) {
  const el = document.createElement("div");
  el.className = "card";

  const top = document.createElement("div");
  top.className = "top";

  const lbl = document.createElement("span");
  lbl.className = "wlabel";
  lbl.textContent = m.name.split(":")[0].toUpperCase();
  top.appendChild(lbl);

  const badge = document.createElement("span");
  badge.className = "badge " + (m.loaded ? "s-healthy" : "s-depleted");
  badge.textContent = m.loaded ? "A CORRER" : "PARADO";
  top.appendChild(badge);

  const paramSize = document.createElement("div");
  paramSize.className = "pct";
  paramSize.style.fontSize = "20px";
  paramSize.textContent = m.parameter_size || "—";

  const sub = document.createElement("div");
  sub.className = "reset";
  const sizeGB =
    m.size_bytes > 0 ? (m.size_bytes / 1e9).toFixed(1) + " GB" : "";
  const quant = m.quantization || "";
  sub.textContent = [quant, sizeGB].filter(Boolean).join(" · ") || "—";

  el.appendChild(top);
  el.appendChild(paramSize);
  el.appendChild(sub);
  return el;
}

function card(w, wide) {
  const el = document.createElement("div");
  el.className = "card" + (wide ? " wide" : "");
  const sc = "s-" + w.status;

  const top = document.createElement("div");
  top.className = "top";
  top.innerHTML =
    '<span class="wlabel">' +
    w.label +
    "</span>" +
    '<span class="badge ' +
    sc +
    '">' +
    STATUS_LABEL[w.status] +
    "</span>";

  const pct = document.createElement("div");
  pct.className = "pct";
  pct.innerHTML = w.remaining_pct + "<small>%</small>";

  const bar = document.createElement("div");
  bar.className = "bar";
  bar.innerHTML =
    '<span class="' + sc + '" style="width:' + w.remaining_pct + '%"></span>';

  const reset = document.createElement("div");
  reset.className = "reset";
  reset.dataset.resetAt = w.reset_at || "";
  reset.textContent = resetText(w.reset_at);

  el.appendChild(top);
  el.appendChild(pct);
  el.appendChild(bar);
  el.appendChild(reset);
  return el;
}

function resetText(iso) {
  if (!iso) return "Reinicia: —";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return "Reinicia: —";
  const now = new Date();
  const diff = d - now;
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (diff <= 0) return "Reinicia agora · " + time;
  const mins = Math.floor(diff / 60000);
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  const rel = h > 0 ? h + "h " + m + "m" : m + "m";
  return "Reinicia em " + rel + " · " + time;
}

function renderUpdated() {
  const el = document.getElementById("updated");
  if (!DATA || !DATA.updated_at) {
    el.textContent = "";
    return;
  }
  const d = new Date(DATA.updated_at);
  const secs = Math.max(0, Math.floor((new Date() - d) / 1000));
  let rel;
  if (secs < 5) rel = "agora mesmo";
  else if (secs < 60) rel = "há " + secs + "s";
  else if (secs < 3600) rel = "há " + Math.floor(secs / 60) + "m";
  else rel = "há " + Math.floor(secs / 3600) + "h";
  el.textContent = "Atualizado " + rel;
}

// tick the relative labels + reset countdowns every second
setInterval(() => {
  renderUpdated();
  document.querySelectorAll(".reset").forEach((el) => {
    el.textContent = resetText(el.dataset.resetAt);
  });
}, 1000);

// ---- Buttons ----
document.getElementById("btn-refresh").onclick = () => {
  const b = document.getElementById("btn-refresh");
  b.innerHTML = '<span class="spin">↻</span> A sincronizar…';
  sendIpc({ type: "refresh" });
};

document.getElementById("btn-close").onclick = () => {
  sendIpc({ type: "close" });
};

document.getElementById("btn-theme").onclick = () => {
  const cur = document.documentElement.getAttribute("data-theme");
  const next = cur === "dark" ? "light" : "dark";
  applyTheme(next);
  sendIpc({ type: "setTheme", theme: next });
};

const settings = document.getElementById("settings");
document.getElementById("btn-settings").onclick = () => {
  document.getElementById("copilot-token").value = "";
  settings.classList.add("open");
};
document.getElementById("btn-settings-close").onclick = () =>
  settings.classList.remove("open");
document.querySelectorAll("#theme-seg .tab").forEach((el) => {
  el.onclick = () => {
    applyTheme(el.dataset.themeValue);
    sendIpc({ type: "setTheme", theme: el.dataset.themeValue });
  };
});
document.getElementById("btn-save").onclick = () => {
  const token = document.getElementById("copilot-token").value.trim();
  if (token) sendIpc({ type: "setCopilotToken", token: token });
  sendIpc({ type: "refresh" });
  settings.classList.remove("open");
};

// react to OS theme changes when in "system" mode
if (window.matchMedia) {
  window
    .matchMedia("(prefers-color-scheme: light)")
    .addEventListener("change", () => {
      if (themePref === "system") applyTheme("system");
    });
}

// Close on click-away: a window-level blur means another app/window was
// activated. Intra-page focus moves (tabs, the settings input) don't fire it.
window.addEventListener("blur", function () {
  sendIpc({ type: "blur" });
});

// Close on Escape key press
window.addEventListener("keydown", function (e) {
  if (e.key === "Escape") {
    sendIpc({ type: "close" });
  }
});

// Take focus so the blur signal is meaningful once shown.
window.addEventListener("focus", function () {});
try {
  window.focus();
} catch (e) {}

// signal readiness so Rust can push the first snapshot
sendIpc({ type: "ready" });
