"use strict";
const TABS = [
  ["providers", "供应商"],
  ["mcp", "MCP"],
  ["skills", "Skills"],
  ["agent", "Agent"],
  ["usage", "用量"],
];
let current = "providers";
let render_seq = 0; // 渲染序号：await 返回后若已过期则丢弃，防串台。

const $ = (sel) => document.querySelector(sel);
const logEl = $("#log");

function pad2(n) { return String(n).padStart(2, "0"); }
function updateStat(sel, html) { const el = $(sel); if (el) el.innerHTML = html; }

function log(text) {
  logEl.textContent += (logEl.textContent ? "\n" : "") + (text || "");
  logEl.scrollTop = logEl.scrollHeight;
  const stamp = $("#log-stamp");
  if (stamp) stamp.textContent = "· 最后输出 " + new Date().toLocaleTimeString();
}

/* ── 全局忙态：命令运行中顶栏出旋转点，页面动作按钮统一降透明防连点 ── */
let busyCount = 0;
function setBusy(delta) {
  busyCount = Math.max(0, busyCount + delta);
  const dot = $("#busy-dot");
  if (dot) dot.hidden = busyCount === 0;
  document.body.classList.toggle("busy", busyCount > 0);
}

/* ── toast：替代 alert 的轻提示，4s 自动消失 ── */
function toast(text, kind) {
  const wrap = $("#toasts");
  if (!wrap) { alert(text); return; }
  const el = document.createElement("div");
  el.className = "toast" + (kind ? " toast-" + kind : "");
  el.textContent = text;
  wrap.appendChild(el);
  requestAnimationFrame(() => el.classList.add("show"));
  setTimeout(() => {
    el.classList.remove("show");
    setTimeout(() => el.remove(), 300);
  }, 4000);
}

function csrfHeaders(opts) {
  const method = (opts && opts.method) || "GET";
  const headers = Object.assign({}, (opts && opts.headers) || {});
  if (method !== "GET" && window.XCC_CSRF) headers["x-xcc-csrf"] = window.XCC_CSRF;
  return headers;
}

async function api(path, opts) {
  opts = Object.assign({}, opts, { headers: csrfHeaders(opts) });
  const res = await fetch(path, opts);
  let body = null;
  try { body = await res.json(); } catch (_) { body = { ok: false, error: "响应解析失败" }; }
  return body;
}

const inflight = new Set(); // 同一命令防重复提交。

async function command(args, key) {
  const dedupeKey = key || args.join(" ");
  if (inflight.has(dedupeKey)) return { ok: false, error: "命令执行中，请稍候" };
  inflight.add(dedupeKey);
  setBusy(1);
  try {
    const res = await api("/api/command", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ args }),
    });
    log("$ spec " + args.join(" "));
    log(res && res.ok ? res.output : (res && res.error) || "命令失败");
    return res;
  } finally {
    inflight.delete(dedupeKey);
    setBusy(-1);
  }
}

function fmtTokens(n) {
  if (n == null) return "-";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return String(n);
}

function pct(x) {
  if (x == null) return "-";
  return (x * 100).toFixed(1) + "%";
}

/* ── 面板「更新于 x 前」时间戳 ── */
const loadedAt = {};
function stampPanel(id) { loadedAt[id] = Date.now(); paintStamp(); }
function paintStamp() {
  const t = loadedAt[current];
  const el = $("#panel-stamp");
  if (!el) return;
  el.textContent = t ? "· 更新于 " + ago(t) : "";
}
function ago(t) {
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 60) return s + "s 前";
  return Math.round(s / 60) + "m 前";
}
setInterval(paintStamp, 5000);

function renderTabs() {
  const nav = $("#tabs");
  nav.innerHTML = "";
  TABS.forEach(([id, label], i) => {
    const b = document.createElement("button");
    b.className = "tab-btn" + (id === current ? " active" : "");
    b.textContent = label;
    b.title = "快捷键 " + (i + 1);
    b.onclick = () => switchTab(id);
    nav.appendChild(b);
  });
}

function switchTab(id) {
  if (current === id) return renderPanel();
  current = id;
  renderTabs();
  renderPanel();
}

let panel_owner = 0; // 当前持有面板的渲染序号；防旧 tab 渲染器抢占。

function panel() {
  const c = $("#content");
  panel_owner = render_seq;
  c.innerHTML = '<div class="loading">加载中…</div>';
  return c;
}

function stateHtml(active) {
  return active
    ? '<span class="state ok"><span class="glyph">●</span>健康</span>'
    : '<span class="state warn"><span class="glyph">○</span>未检</span>';
}

async function renderProviders() {
  const seq = ++render_seq;
  const c = panel();
  const res = await api("/api/providers");
  if (seq !== render_seq || panel_owner !== seq) return;
  stampPanel("providers");
  if (!res || !res.ok) { c.innerHTML = '<div class="error-box">无法连接后端：' + (res && res.error || "未知错误") + '</div>'; return; }
  const list = res.data || [];
  updateStat("#stat-providers", pad2(list.length));
  updateStat("#stat-healthy", pad2(list.filter((p) => p.health_active).length));
  if (!list.length) { c.innerHTML = '<div class="empty">暂无供应商</div>'; return; }

  function listView() {
    c.innerHTML = '<div class="section-head"><span class="section-title">PROVIDERS — ' + pad2(list.length) + ' ENTRIES <span id="panel-stamp" class="panel-stamp"></span></span></div>';
    const t = document.createElement("table");
    t.className = "ledger";
    const heads = ["名称", "协议", "状态", "模型", "ENDPOINT", "默认模型", "操作"];
    const thead = document.createElement("thead");
    const hr = document.createElement("tr");
    for (const h of heads) { const th = document.createElement("th"); th.textContent = h; hr.appendChild(th); }
    thead.appendChild(hr); t.appendChild(thead);
    const tb = document.createElement("tbody");
    for (const p of list) {
      const tr = document.createElement("tr");
      const mk = (label, cls) => { const td = document.createElement("td"); if (label) td.dataset.label = label; if (cls) td.className = cls; tr.appendChild(td); return td; };
      const tdName = mk(null, "cell-name"); tdName.textContent = p.name || p.id;
      mk("协议", "cell-mono").textContent = p.protocol || "-";
      mk("状态").innerHTML = stateHtml(p.health_active);
      const tdModels = mk("模型", "num"); tdModels.textContent = String((p.models || []).length);
      mk("ENDPOINT", "cell-mono").textContent = p.endpoint || "";
      mk("默认模型", "cell-mono").textContent = p.default_model || "-";
      const tdAct = mk("操作");
      tdAct.innerHTML =
        '<span class="row-actions">' +
        '<button class="act">详情</button><button class="act">测试</button><button class="act">模型</button><button class="act danger">删除</button>' +
        "</span>";
      const [detail, test, models, del] = tdAct.querySelectorAll("button");
      test.onclick = () => command(["provider", "test", p.id]);
      models.onclick = () => command(["provider", "models", p.id]);
      del.onclick = () => { if (confirm("删除供应商 " + p.name + "？")) command(["provider", "delete", p.id]); };
      detail.onclick = async () => {
        const out = await api("/api/command", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ args: ["provider", "show", p.id] }) });
        detailView(out && out.output || (out && out.error) || "");
      };
      tb.appendChild(tr);
    }
    t.appendChild(tb);
    c.appendChild(t);
    paintStamp();
  }

  function detailView(output) {
    c.innerHTML =
      '<div class="section-head"><span class="row-actions"><button class="act" id="back-detail">← 返回列表</button>' +
      '<button class="act" id="copy-detail">复制</button></span></div>' +
      '<pre class="detail">' + escapeHtml(output) + "</pre>";
    $("#back-detail").onclick = listView;
    $("#copy-detail").onclick = async () => {
      try { await navigator.clipboard.writeText(output); toast("已复制到剪贴板"); }
      catch (_) { toast("复制失败，请手动选择", "warn"); }
    };
  }

  listView();
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

const TARGETS = ["opencode", "claude", "codex"];

function dotsHtml(enabled) {
  return TARGETS.map((t) =>
    '<span class="glyph ' + (enabled && enabled[t] ? "on" : "off") + '">' + t + "</span>"
  ).join(" ");
}

function collectOpenNames() {
  return Array.from(document.querySelectorAll(".entry.open .entry-name")).map((el) => el.textContent);
}

/// 重渲染后按名称恢复展开态；构建闭包存在 entry._buildBody 上（renderMcp/Skills 注入）。
function applyPendingOpen() {
  const names = pending_open;
  pending_open = null;
  if (!names || !names.length) return;
  for (const entry of document.querySelectorAll("#content .entry")) {
    const nameEl = entry.querySelector(".entry-name");
    if (!nameEl || !names.includes(nameEl.textContent)) continue;
    entry.classList.add("open");
    entry.dataset.loaded = "1";
    const body = entry.querySelector(".entry-body");
    if (typeof entry._buildBody === "function" && body) entry._buildBody(body);
  }
}

const toggling = new Set(); // 防同一开关双击发出两个相反请求。

async function toggleTarget(kind, id, target, next) {
  const key = kind + ":" + id + ":" + target;
  if (toggling.has(key)) return;
  toggling.add(key);
  try {
    const open = collectOpenNames();
    const res = await command([kind, next ? "enable" : "disable", id, "--target", target, "--yes"]);
    if (res && res.ok) {
      pending_open = open;
      await (current === "mcp" ? renderMcp() : current === "skills" ? renderSkills() : renderPanel());
      if (current === "mcp" || current === "skills") applyPendingOpen();
    }
  } finally {
    toggling.delete(key);
  }
}
let pending_open = null;

function toggleRow(kind, entry, enabled) {
  const row = document.createElement("div");
  row.className = "toggle-row";
  for (const t of TARGETS) {
    const on = !!(enabled && enabled[t]);
    const b = document.createElement("button");
    b.className = "toggle-chip" + (on ? " on" : "");
    b.innerHTML = '<span class="glyph ' + (on ? "on" : "off") + '">●</span>' + t +
      ' <span class="toggle-word">' + (on ? "启用" : "停用") + "</span>";
    b.onclick = () => toggleTarget(kind, entry.id, t, !on);
    row.appendChild(b);
  }
  return row;
}

async function renderMcp() {
  const seq = ++render_seq;
  const c = panel();
  const res = await api("/api/mcp");
  if (seq !== render_seq || panel_owner !== seq) return;
  stampPanel("mcp");
  if (!res || !res.ok) { c.innerHTML = '<div class="error-box">无法连接后端</div>'; return; }
  const list = res.data || [];
  if (!list.length) { c.innerHTML = '<div class="empty">暂无 MCP 服务</div>'; return; }
  c.innerHTML = '<div class="section-head"><span class="section-title">MCP — ' + pad2(list.length) +
    ' ENTRIES · 点行展开 <span id="panel-stamp" class="panel-stamp"></span></span><button class="act" id="mcp-refresh">刷新 ↻</button></div>';
  $("#mcp-refresh").onclick = renderMcp;
  for (const m of list) {
    const entry = document.createElement("section");
    entry.className = "entry";
    entry.innerHTML =
      '<header class="entry-head"><span class="entry-name">' + escapeHtml(m.name || m.id) + '</span>' +
      '<span class="entry-meta">' + escapeHtml(m.transport || "-") + "</span>" +
      '<span class="entry-dots">' + dotsHtml(m.enabled) + '</span>' +
      '<span class="chev">▸</span></header>' +
      '<div class="entry-body"></div>';
    const head = entry.querySelector(".entry-head");
    head.onclick = () => {
      entry.classList.toggle("open");
      entry._buildBody = () => buildMcpBody(entry.querySelector(".entry-body"), m);
      if (entry.classList.contains("open") && !entry.dataset.loaded) {
        entry.dataset.loaded = "1";
        entry._buildBody();
      }
    };
    c.appendChild(entry);
  }
  paintStamp();

  function buildMcpBody(body, m) {
    body.innerHTML = "";
    const info = document.createElement("div");
    info.className = "entry-info";
    info.innerHTML =
      (m.description ? "<div>" + escapeHtml(m.description) + "</div>" : "") +
      (m.command ? '<div class="muted">CMD ' + escapeHtml(m.command) + "</div>" : "") +
      (m.url ? '<div class="muted">URL ' + escapeHtml(m.url) + "</div>" : "") +
      '<div class="muted">ID ' + escapeHtml(m.id) + "</div>";
    body.appendChild(info);
    body.appendChild(toggleRow("mcp", m, m.enabled));
    const actions = document.createElement("div");
    actions.className = "entry-actions";
    actions.innerHTML = '<button class="act" data-edit>✎ 编辑字段</button>' +
      '<button class="act danger" data-del>删除</button>';
    body.appendChild(actions);
    actions.querySelector("[data-edit]").onclick = () => buildMcpForm(body, m, actions);
    actions.querySelector("[data-del]").onclick = async () => {
      if (!confirm("删除 MCP " + (m.name || m.id) + "？")) return;
      const res = await command(["mcp","delete", m.id, "--yes"]);
      if (res && res.ok) { current === "mcp" ? renderMcp() : renderPanel(); }
    };
  }

  function buildMcpForm(body, m, actions) {
    let form = body.querySelector("form.edit-grid");
    if (form) { form.remove(); return; }
    form = document.createElement("form");
    form.className = "edit-grid";
    form.innerHTML =
      "<label>名称<input name=\"name\" value=\"" + escapeHtml(m.name || "") + "\"></label>" +
      "<label>传输<select name=\"transport\">" +
      ["stdio", "http", "sse"].map((t) => '<option' + (m.transport === t ? " selected" : "") + ">" + t + "</option>").join("") +
      "</select></label>" +
      "<label>命令<input name=\"command\" value=\"" + escapeHtml(m.command || "") + '\"></label>' +
      "<label>URL<input name=\"url\" value=\"" + escapeHtml(m.url || "") + '" placeholder="http/sse 端点"></label>' +
      "<label>描述<input name=\"description\" value=\"" + escapeHtml(m.description || "") + '\"></label>' +
      '<div class="edit-actions"><button type="submit" class="act">保存</button>' +
      '<button type="button" class="act" data-cancel>取消</button></div>';
    form.querySelector("[data-cancel]").onclick = () => form.remove();
    form.onsubmit = async (ev) => {
      ev.preventDefault();
      const f = new FormData(form);
      const args = ["mcp","update", m.id];
      const namev = String(f.get("name") || "").trim();
      if (namev && namev !== m.name) args.push("--name", namev);
      args.push("--transport", String(f.get("transport")));
      const descv = String(f.get("description") || "").trim();
      if (descv && descv !== (m.description || "")) args.push("--description", descv);
      const cmdv = String(f.get("command") || "").trim();
      if (cmdv && cmdv !== (m.command || "")) args.push("--command", cmdv);
      const urlv = String(f.get("url") || "").trim();
      if (urlv && urlv !== (m.url || "")) args.push("--url", urlv);
      args.push("--yes");
      const res = await command(args);
      if (res && res.ok) {
        pending_open = collectOpenNames();
        current === "mcp" ? await renderMcp() : await renderPanel();
        applyPendingOpen();
      }
    };
    body.insertBefore(form, actions);
  }
}

async function renderSkills() {
  const seq = ++render_seq;
  const c = panel();
  const res = await api("/api/skills");
  if (seq !== render_seq || panel_owner !== seq) return;
  stampPanel("skills");
  if (!res || !res.ok) { c.innerHTML = '<div class="error-box">无法连接后端</div>'; return; }
  const list = res.data || [];
  if (!list.length) { c.innerHTML = '<div class="empty">暂无 Skill</div>'; return; }
  c.innerHTML = '<div class="section-head"><span class="section-title">SKILLS — ' + pad2(list.length) +
    ' ENTRIES · 点行展开 <span id="panel-stamp" class="panel-stamp"></span></span><button class="act" id="skills-refresh">刷新 ↻</button></div>';
  $("#skills-refresh").onclick = renderSkills;
  for (const s of list) {
    const entry = document.createElement("section");
    entry.className = "entry";
    entry.innerHTML =
      '<header class="entry-head"><span class="entry-name">' + escapeHtml(s.name || s.id) + '</span>' +
      '<span class="entry-meta">' + escapeHtml(s.id || "") + "</span>" +
      '<span class="entry-dots">' + dotsHtml(s.enabled) + '</span>' +
      '<span class="chev">▸</span></header>' +
      '<div class="entry-body"></div>';
    const head = entry.querySelector(".entry-head");
    entry._buildBody = () => {
      const body = entry.querySelector(".entry-body");
      body.innerHTML = "";
      const info = document.createElement("div");
      info.className = "entry-info";
      info.innerHTML = (s.path ? '<div class="muted">PATH ' + escapeHtml(s.path) + "</div>" : "") +
        '<div class="muted">ID ' + escapeHtml(s.id) + "</div>";
      body.appendChild(info);
      body.appendChild(toggleRow("skill", s, s.enabled));
    };
    head.onclick = () => {
      entry.classList.toggle("open");
      if (entry.classList.contains("open") && !entry.dataset.loaded) {
        entry.dataset.loaded = "1";
        entry._buildBody();
      }
    };
    c.appendChild(entry);
  }
  paintStamp();
}

async function renderAgent() {
  const seq = ++render_seq;
  const c = panel();
  const res = await api("/api/overview");
  if (seq !== render_seq || panel_owner !== seq) return;
  stampPanel("agent");
  if (!res || !res.ok) { c.innerHTML = '<div class="error-box">无法连接后端</div>'; return; }
  const d = res.data || {};
  const running = d.runtime && d.runtime.running;
  updateStat("#stat-runtime",
    running
      ? '<span class="dot on"></span><span class="runtime-word">RUNNING</span>'
      : '<span class="dot"></span><span class="runtime-word">STOPPED</span>');
  const rt = running ? '<span class="state ok"><span class="glyph">●</span>运行中</span>' : '<span class="state warn"><span class="glyph">○</span>未运行</span>';
  const agents = d.agents || [];
  c.innerHTML = '<div class="section-head"><span class="section-title">AGENT — RUNTIME ' + rt + ' <span id="panel-stamp" class="panel-stamp"></span></span>' +
    '<span class="row-actions"><button class="act" id="agent-refresh">刷新 ↻</button>' +
    '<button class="act" id="agent-doctor">诊断</button>' +
    '<button class="act" id="agent-setup">全部更新</button></span></div>';
  $("#agent-refresh").onclick = () => command(["agent", "status", "--latest"]);
  $("#agent-doctor").onclick = () => command(["agent", "doctor"]);
  $("#agent-setup").onclick = () => { if (confirm("更新所有 Agent？")) command(["agent", "setup", "--yes"]); };
  for (const a of agents) {
    const card = document.createElement("div");
    card.className = "card";
    const st = a.ready ? '<span class="state ok"><span class="glyph">●</span>就绪</span>' : '<span class="state warn"><span class="glyph">○</span>未就绪</span>';
    const updatable = a.ready && a.latest_version && a.current_version && a.current_version !== a.latest_version;
    card.innerHTML = '<div class="card-title">' + escapeHtml(a.name || "-") + " " + st +
      '<span class="muted" style="margin-left:auto">当前 ' + escapeHtml(a.current_version || "未安装") +
      (a.latest_version ? " · 最新 " + escapeHtml(a.latest_version) : "") + "</span></div>" +
      (updatable ? '<button class="act card-update">更新到 ' + escapeHtml(a.latest_version) + '</button>' : "");
    const ub = card.querySelector(".card-update");
    if (ub) ub.onclick = () => { if (confirm("更新 " + (a.name || "") + "？")) command(["agent", "install", a.name, "--yes"]); };
    c.appendChild(card);
  }
  paintStamp();
}

async function renderUsage() {
  const seq = ++render_seq;
  const c = panel();
  const res = await api("/api/stats");
  if (seq !== render_seq || panel_owner !== seq) return;
  stampPanel("usage");
  if (!res || !res.ok) { c.innerHTML = '<div class="error-box">无法连接后端</div>'; return; }
  const periods = (res.data && res.data.periods) || {};
  const keys = ["24h", "48h", "7d", "30d"];
  const heads = ["周期", "输入(fresh)", "输出", "缓存读", "缓存写", "命中率", "成功率", "请求数"];
  const t = document.createElement("table");
  t.className = "ledger";
  const head = document.createElement("thead"); const hr = document.createElement("tr");
  for (const h of heads) {
    const th = document.createElement("th"); if (h !== "周期") th.className = "num"; th.textContent = h; hr.appendChild(th);
  }
  head.appendChild(hr); t.appendChild(head);
  const tb = document.createElement("tbody");
  for (const k of keys) {
    const s = periods[k] || {};
    const tr = document.createElement("tr");
    let col = 0;
    for (const cell of [k, { v: fmtTokens(s.fresh_input), num: true }, { v: fmtTokens(s.output), num: true },
        { v: fmtTokens(s.cached), num: true }, { v: fmtTokens(s.cache_creation), num: true },
        { v: pct(s.cache_hit_rate), num: true }, { v: pct(s.success_rate), num: true },
        { v: s.requests != null ? String(s.requests) : "-", num: true }]) {
      const td = document.createElement("td");
      td.dataset.label = heads[col];
      if (cell && cell.num) { td.className = "num"; td.textContent = cell.v; }
      else { td.textContent = cell; }
      tr.appendChild(td);
      col += 1;
    }
    tb.appendChild(tr);
  }
  t.appendChild(tb);
  c.innerHTML = '<div class="section-head"><span class="section-title">USAGE — 缓存命中已计入 <span id="panel-stamp" class="panel-stamp"></span></span><button class="act" id="usage-refresh">刷新 ↻</button></div>';
  $("#usage-refresh").onclick = renderUsage;
  c.appendChild(t);
  paintStamp();
}

function init() {
  renderTabs();
  renderPanel();

  /* 日志面板：清空 / 折叠 */
  $("#log-clear").onclick = () => { logEl.textContent = ""; };
  $("#log-toggle").onclick = () => {
    const sec = $("#log-section");
    sec.classList.toggle("collapsed");
    $("#log-toggle").textContent = sec.classList.contains("collapsed") ? "展开" : "收起";
  };
  if (window.matchMedia && matchMedia("(max-width: 768px)").matches) {
    $("#log-section").classList.add("collapsed");
    $("#log-toggle").textContent = "展开";
  }

  /* 快捷键：1-5 切 tab，r 刷新当前页；输入框聚焦时忽略 */
  document.addEventListener("keydown", (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
    const tag = (ev.target && ev.target.tagName) || "";
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(tag)) return;
    const n = parseInt(ev.key, 10);
    if (n >= 1 && n <= TABS.length) { switchTab(TABS[n - 1][0]); return; }
    if (ev.key === "r" || ev.key === "R") { renderPanel(); }
  });

  /* 运行时状态轮询：15s 一次，静默更新顶栏 RUNNING */
  async function refreshRuntime() {
    const res = await api("/api/overview");
    if (res && res.ok) {
      const running = res.data && res.data.runtime && res.data.runtime.running;
      updateStat("#stat-runtime",
        running
          ? '<span class="dot on"></span><span class="runtime-word">RUNNING</span>'
          : '<span class="dot"></span><span class="runtime-word">STOPPED</span>');
    }
  }
  refreshRuntime();
  setInterval(refreshRuntime, 15000);

  $("#stop-btn").onclick = async () => {
    const res = await api("/api/web/stop", { method: "POST" });
    if (res && res.ok) {
      toast(res.warning ? "已停止。注意：" + res.warning : "已切回 TUI，下次启动将直接进入 TUI", "ok");
    } else {
      toast((res && res.error) || "停止失败", "warn");
    }
  };
  setInterval(() => { if (current === "usage") renderUsage(); }, 10000);
}
function renderPanel() {
  const handlers = { providers: renderProviders, mcp: renderMcp, skills: renderSkills, agent: renderAgent, usage: renderUsage };
  return handlers[current]();
}

window.addEventListener("DOMContentLoaded", init);
