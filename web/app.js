const API = "/api/v1";
const PROTOCOL_VERSION = 2;
const TOKEN_KEY = "sumireToken";
const LEGACY_TOKEN_KEY = "zjuPortalToken";

const state = {
  token: localStorage.getItem(TOKEN_KEY) || sessionStorage.getItem(TOKEN_KEY) || localStorage.getItem(LEGACY_TOKEN_KEY) || sessionStorage.getItem(LEGACY_TOKEN_KEY) || "",
  config: null,
  configSnapshot: null,
  sessionId: "default",
  eventSource: null,
  traffic: null,
  trafficTimer: null,
  statusTimer: null,
  connectionsTimer: null,
  currentView: "overview",
  connectionsLoading: false,
  managed: false,
  connected: false,
  managedConnecting: false,
  runtimeTimer: null,
  logCursor: 0,
  logTimer: null,
  logsLoading: false,
  sessionState: "idle",
  sessionToggleBusy: false,
  systemProxySupported: false,
  systemProxySOCKSSupported: false,
  systemProxyEnabled: false,
  systemProxyBusy: false,
  speedHistory: { download: [], upload: [] },
  connections: [],
  selectedConnection: null,
  challenge: null,
  selectedChoice: "",
  graphPoints: [],
};

const elements = Object.fromEntries([...document.querySelectorAll("[id]")].map(element => [element.id, element]));
const titles = { overview: "运行概览", config: "配置管理", connections: "连接管理", events: "实时事件", logs: "Core 日志" };
const stateNames = {
  idle: "空闲", discovering_auth: "发现认证方式", authenticating: "等待认证", fetching_resources: "获取资源",
  selecting_nodes: "选择节点", establishing_tunnel: "建立隧道", ready: "已连接", reconnecting: "正在重连",
  failed: "连接失败", stopping: "正在停止", stopped: "已停止",
};

document.querySelectorAll(".nav-item").forEach(button => button.addEventListener("click", () => showView(button.dataset.view)));
elements.connectButton.addEventListener("click", openConnectDialog);
elements.connectForm.addEventListener("submit", event => { event.preventDefault(); connect(); });
elements.refreshButton.addEventListener("click", refreshAll);
elements.refreshSessionInfoButton.addEventListener("click", refreshSessionInfo);
elements.refreshConnectionsButton.addEventListener("click", loadConnections);
elements.activeConnectionsButton.addEventListener("click", () => showView("connections"));
elements.clearEventsButton.addEventListener("click", () => { elements.eventLog.innerHTML = "尚未收到事件"; elements.eventLog.classList.add("empty"); });
elements.clearCoreLogsButton.addEventListener("click", () => { elements.coreLogViewer.innerHTML = "暂无 Core 日志"; elements.coreLogViewer.classList.add("empty"); });
elements.saveConfigButton.addEventListener("click", saveConfig);
elements.applySessionConfigButton.addEventListener("click", applySessionConfig);
elements.restartCoreButton.addEventListener("click", restartManagedCore);
elements.configEditor.addEventListener("input", renderConfigHighlights);
elements.configEditor.addEventListener("scroll", syncConfigHighlightScroll);
elements.sessionToggle.addEventListener("click", toggleSession);
elements.systemProxyToggle.addEventListener("click", toggleSystemProxy);
elements.routingMode.addEventListener("change", changeRoutingMode);
elements.authForm.addEventListener("submit", event => { event.preventDefault(); submitAuth(); });
elements.authImage.addEventListener("click", addGraphPoint);
elements.authImage.addEventListener("load", renderGraphPoints);
window.addEventListener("resize", () => { renderGraphPoints(); drawTrafficCharts(); });
elements.undoGraphClickButton.addEventListener("click", () => { state.graphPoints.pop(); renderGraphPoints(); });
elements.clearGraphClicksButton.addEventListener("click", () => { state.graphPoints = []; renderGraphPoints(); });
elements.closeConnectionDialogButton.addEventListener("click", closeConnectionDialog);
elements.dismissConnectionDialogButton.addEventListener("click", closeConnectionDialog);
elements.connectionDialog.addEventListener("close", () => { state.selectedConnection = null; });
elements.closeSelectedConnectionButton.addEventListener("click", () => {
  if (state.selectedConnection) closeConnection(state.selectedConnection.id);
});

initialize();
drawTrafficCharts();

async function initialize() {
  try {
    const bootstrap = await webui("/bootstrap");
    state.managed = Boolean(bootstrap.managed);
    if (state.managed) {
      state.token = bootstrap.token;
      sessionStorage.setItem(TOKEN_KEY, state.token);
      sessionStorage.removeItem(LEGACY_TOKEN_KEY);
      localStorage.removeItem(LEGACY_TOKEN_KEY);
      elements.connectButton.hidden = true;
      elements.coreLogsNav.hidden = false;
      await loadSystemProxyStatus();
      await loadRuntime();
      await loadManagedConfigEditor();
      state.runtimeTimer = setInterval(async () => {
        const runtime = await loadRuntime();
        if (runtime?.running && !state.connected) connectManagedCore();
      }, 3000);
      await connectManagedCore();
      return;
    }
  } catch (error) {
    console.warn("load WebUI bootstrap", error);
  }
  if (state.token) connect(true); else openConnectDialog();
}

async function connectManagedCore() {
  if (state.managedConnecting) return false;
  state.managedConnecting = true;
  try {
    for (let attempt = 0; attempt < 12; attempt++) {
      if (await connect(true)) return true;
      await delay(500);
    }
    setConnected(false);
    return false;
  } finally {
    state.managedConnecting = false;
  }
}

function showView(name) {
  state.currentView = name;
  document.querySelectorAll(".nav-item").forEach(item => item.classList.toggle("active", item.dataset.view === name));
  document.querySelectorAll(".view").forEach(view => view.classList.toggle("active", view.id === `${name}View`));
  elements.pageTitle.textContent = titles[name];
  if (name === "connections") {
    loadConnections();
    startConnectionsPolling();
  } else {
    stopConnectionsPolling();
  }
  if (name === "logs") {
    loadCoreLogs();
    startLogPolling();
  } else {
    stopLogPolling();
  }
  if (name === "overview") drawTrafficCharts();
}

async function loadCoreLogs() {
  if (!state.managed || state.logsLoading) return;
  state.logsLoading = true;
  try {
    const result = await webui(`/logs?after=${state.logCursor}`);
    state.logCursor = result.next;
    if (!result.entries?.length) return;
    const viewer = elements.coreLogViewer;
    const followTail = viewer.scrollHeight - viewer.scrollTop - viewer.clientHeight < 60;
    if (viewer.classList.contains("empty")) {
      viewer.innerHTML = "";
      viewer.classList.remove("empty");
    }
    for (const entry of result.entries) {
      const line = document.createElement("div");
      line.className = `core-log-line ${entry.stream}`;
      const streamLabel = entry.stream === "stderr" ? "CORE" : entry.stream.toUpperCase();
      line.innerHTML = `<time>${new Date(entry.timestamp).toLocaleTimeString()}</time><span class="stream">${escapeHTML(streamLabel)}</span><span class="message">${escapeHTML(entry.message)}</span>`;
      viewer.append(line);
    }
    while (viewer.children.length > 2000) viewer.firstElementChild.remove();
    if (followTail) viewer.scrollTop = viewer.scrollHeight;
  } catch (error) { console.warn("load Core logs", error); }
  finally { state.logsLoading = false; }
}

function startLogPolling() {
  stopLogPolling();
  state.logTimer = setInterval(loadCoreLogs, 1000);
}

function stopLogPolling() {
  clearInterval(state.logTimer);
  state.logTimer = null;
}

function openConnectDialog() {
  elements.tokenInput.value = state.token;
  elements.rememberToken.checked = Boolean(localStorage.getItem(TOKEN_KEY) || localStorage.getItem(LEGACY_TOKEN_KEY));
  elements.connectDialog.showModal();
}

async function connect(silent = false) {
  const token = silent ? state.token : elements.tokenInput.value.trim();
  state.token = token;
  try {
    const hello = await api("/hello");
    if (hello.protocol_version !== PROTOCOL_VERSION) throw new Error(`不支持控制协议版本 ${hello.protocol_version}`);
    sessionStorage.setItem(TOKEN_KEY, token);
    sessionStorage.removeItem(LEGACY_TOKEN_KEY);
    localStorage.removeItem(LEGACY_TOKEN_KEY);
    if (!silent && elements.rememberToken.checked) localStorage.setItem(TOKEN_KEY, token);
    else if (!silent) localStorage.removeItem(TOKEN_KEY);
    elements.connectDialog.close();
    setConnected(true);
    elements.coreVersion.textContent = `${hello.core_version} · Protocol ${hello.protocol_version}`;
    await loadConfig();
    subscribeEvents();
    startPolling();
    await refreshAll();
    if (!silent) toast("Core 连接成功");
    return true;
  } catch (error) {
    setConnected(false);
    if (silent && !state.managed) openConnectDialog();
    if (!silent) toast(error.message, true);
    return false;
  }
}

async function webui(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (state.token) headers.set("Authorization", `Bearer ${state.token}`);
  if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
  const response = await fetch(`/webui${path}`, { ...options, headers });
  const payload = await response.json();
  if (!response.ok) throw new Error(payload?.error?.message || `WebUI 请求失败 (${response.status})`);
  return payload.result;
}

async function loadRuntime() {
  if (!state.managed) return;
  try {
    const runtime = await webui("/runtime");
    if (!runtime.running) {
      setConnected(false);
      renderSessionStatus("stopped", runtime.last_error ? { message: `Core 正在自动重启：${runtime.last_error}` } : null);
    }
    return runtime;
  } catch (error) { console.warn("load managed runtime", error); }
}

async function loadSystemProxyStatus() {
  if (!state.managed) return;
  try {
    applySystemProxyState(await webui("/system-proxy"));
  } catch (error) {
    console.warn("load system proxy status", error);
  }
}

function applySystemProxyState(proxyState) {
  state.systemProxySupported = Boolean(proxyState?.supported);
  state.systemProxySOCKSSupported = Boolean(proxyState?.socks_supported);
  state.systemProxyEnabled = Boolean(proxyState?.enabled);
  updateSystemProxyToggle();
}

function activeSystemProxyAddresses() {
  const inbounds = state.configSnapshot?.active?.inbounds || {};
  return {
    http: inbounds.http?.enabled ? inbounds.http.listen || "" : "",
    socks: state.systemProxySOCKSSupported && inbounds.socks5?.enabled ? inbounds.socks5.listen || "" : "",
  };
}

function updateSystemProxyToggle() {
  elements.systemProxyToggle.hidden = !state.managed || !state.systemProxySupported;
  elements.systemProxyToggle.setAttribute("aria-checked", String(state.systemProxyEnabled));
  elements.systemProxyToggleLabel.textContent = "系统代理";
  const addresses = activeSystemProxyAddresses();
  const enabledAddresses = [addresses.http && `HTTP ${addresses.http}`, addresses.socks && `SOCKS5 ${addresses.socks}`].filter(Boolean);
  const canEnable = state.connected && state.sessionState === "ready" && enabledAddresses.length > 0;
  elements.systemProxyToggle.disabled = state.systemProxyBusy || (!state.systemProxyEnabled && !canEnable);
  elements.systemProxyToggle.title = state.systemProxyEnabled
    ? "关闭系统代理和强制代理守卫"
    : canEnable ? `覆盖系统代理并每 5 秒强制检查：${enabledAddresses.join("、")}` : "会话就绪且本地代理入站运行后可用";
}

async function configureSystemProxy(enabled, announce = true) {
  state.systemProxyBusy = true;
  updateSystemProxyToggle();
  try {
    const addresses = enabled ? activeSystemProxyAddresses() : { http: "", socks: "" };
    if (enabled && (state.sessionState !== "ready" || (!addresses.http && !addresses.socks))) throw new Error("会话就绪且本地代理入站运行后才能启用系统代理");
    const result = await webui("/system-proxy", {
      method: "PUT",
      body: JSON.stringify({ enabled, http_address: addresses.http, socks_address: addresses.socks }),
    });
    applySystemProxyState(result);
    if (announce) {
      const applied = [addresses.http && `HTTP ${addresses.http}`, addresses.socks && `SOCKS5 ${addresses.socks}`].filter(Boolean).join("、");
      toast(enabled ? `系统代理已设置：${applied}` : "系统代理已关闭");
    }
  } finally {
    state.systemProxyBusy = false;
    updateSystemProxyToggle();
  }
}

async function toggleSystemProxy() {
  if (state.systemProxyBusy) return;
  try {
    await configureSystemProxy(!state.systemProxyEnabled);
  } catch (error) {
    toast(error.message, true);
  }
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (state.token) headers.set("Authorization", `Bearer ${state.token}`);
  if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
  const response = await fetch(`${API}${path}`, { ...options, headers });
  const contentType = response.headers.get("content-type") || "";
  const payload = contentType.includes("application/json") ? await response.json() : null;
  if (!response.ok) {
    const coreError = payload?.error;
    const message = coreError ? `${coreError.message}${coreError.detail ? `：${coreError.detail}` : ""}` : `请求失败 (${response.status})`;
    const error = new Error(message);
    error.code = coreError?.code;
    error.status = response.status;
    throw error;
  }
  return payload?.result ?? payload;
}

function setConnected(connected) {
  state.connected = connected;
  elements.connectionDot.classList.toggle("online", connected);
  elements.connectionText.textContent = connected ? "已连接 Core" : "未连接 Core";
  if (!connected) elements.coreVersion.textContent = "连接已断开";
}

async function loadConfig(refreshEditor = true) {
  const snapshot = await api("/config");
  updateConfigSnapshot(snapshot);
  if (!refreshEditor) return;
  if (state.managed) await loadManagedConfigEditor();
  else {
    elements.configEditor.value = JSON.stringify(state.config, null, 2);
    elements.configTitle.textContent = "Configured 配置（JSON）";
    renderConfigHighlights();
  }
}

function updateConfigSnapshot(snapshot) {
  state.configSnapshot = snapshot;
  state.config = snapshot?.configured || null;
  const active = snapshot?.active || state.config || {};
  state.sessionId = active.session?.id || state.config?.session?.id || "default";
  elements.sessionId.textContent = state.sessionId;
  elements.serverAddress.textContent = `${active.atrust?.server || "—"}:${active.atrust?.port || "—"}`;
  elements.username.textContent = active.atrust?.username || "—";
  elements.routingMode.value = active.routing?.mode || "rule";
  renderConfigLifecycle();
  updateSystemProxyToggle();
}

function renderConfigLifecycle() {
  const snapshot = state.configSnapshot || {};
  const pending = snapshot.pending || [];
  const sessionPending = pending.filter(change => change.requires === "session_restart");
  const corePending = pending.filter(change => change.requires === "core_restart");
  elements.configPendingList.replaceChildren();
  if (pending.length === 0) {
    elements.configPendingList.className = "config-pending empty";
    elements.configPendingList.textContent = "configured 与 active 已同步";
  } else {
    elements.configPendingList.className = "config-pending";
    for (const change of pending) {
      const item = document.createElement("span");
      item.className = `config-change ${change.requires}`;
      item.textContent = `${change.path} · ${change.requires === "core_restart" ? "重启 Core" : "重启 Session"}`;
      elements.configPendingList.append(item);
    }
  }
  elements.applySessionConfigButton.hidden = sessionPending.length === 0 || corePending.length > 0;
  elements.restartCoreButton.hidden = !state.managed || corePending.length === 0;
  if (corePending.length > 0) {
    elements.configPendingHint.textContent = state.managed
      ? "Core 级配置已经写入 YAML，重启 Core 后生效。"
      : "Core 级配置必须修改 Core 使用的 YAML 文件并重启进程。";
  } else if (sessionPending.length > 0) {
    elements.configPendingHint.textContent = "配置已保存，但当前 Session 仍使用 active 快照。";
  } else {
    elements.configPendingHint.textContent = "当前没有待应用配置。";
  }
  renderConfigHighlights();
}

function renderConfigHighlights() {
  const text = elements.configEditor.value || "";
  const pending = state.configSnapshot?.pending || [];
  const paths = state.managed ? yamlPathsByLine(text) : [];
  const lines = text.split("\n");
  elements.configHighlight.innerHTML = lines.map((line, index) => {
    const path = paths[index] || "";
    const change = pending.find(item => path === item.path || path.startsWith(`${item.path}.`));
    const className = change ? ` config-editor-line-${change.requires}` : "";
    return `<span class="config-editor-line${className}">${line ? escapeHTML(line) : "&nbsp;"}</span>`;
  }).join("");
  syncConfigHighlightScroll();
}

function yamlPathsByLine(text) {
  const stack = [];
  return text.split("\n").map(line => {
    const match = /^(\s*)(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_.-]+))\s*:(?:\s|$)/.exec(line);
    if (!match) return "";
    const indent = match[1].replaceAll("\t", "  ").length;
    const key = match[2] || match[3] || match[4];
    while (stack.length && stack[stack.length - 1].indent >= indent) stack.pop();
    const path = [...stack.map(item => item.key), key].join(".");
    stack.push({ indent, key });
    return path;
  });
}

function syncConfigHighlightScroll() {
  elements.configHighlight.scrollTop = elements.configEditor.scrollTop;
  elements.configHighlight.scrollLeft = elements.configEditor.scrollLeft;
}

async function loadManagedConfigEditor() {
  if (!state.managed) return;
  const response = await fetch("/webui/config");
  if (!response.ok) throw new Error(`读取 YAML 配置失败 (${response.status})`);
  elements.configEditor.value = await response.text();
  elements.configTitle.textContent = "磁盘配置（YAML）";
  renderConfigHighlights();
}

async function saveConfig() {
  try {
    let snapshot;
    if (state.managed) {
      const response = await fetch("/webui/config", {
        method: "PUT",
        headers: { "Authorization": `Bearer ${state.token}`, "Content-Type": "application/yaml" },
        body: elements.configEditor.value,
      });
      const payload = await response.json();
      if (!response.ok) throw new Error(payload?.error?.message || `应用 YAML 配置失败 (${response.status})`);
      snapshot = payload.result;
    } else {
      const config = JSON.parse(elements.configEditor.value);
      snapshot = await api("/config", { method: "PUT", body: JSON.stringify(config) });
    }
    if (snapshot?.configured) updateConfigSnapshot(snapshot);
    await loadConfig();
    const pending = state.configSnapshot?.pending || [];
    if (pending.some(change => change.requires === "core_restart")) toast("修改已保存到磁盘，需要重启 Core 后生效");
    else if (pending.some(change => change.requires === "session_restart")) toast("修改已保存到磁盘，尚未应用到当前 Session");
    else toast(state.managed ? "修改已保存到磁盘并生效" : "配置已应用");
  } catch (error) {
    const message = error.code === "RESTART_REQUIRED" && !state.managed
      ? "该字段必须修改 Core 的 YAML 配置文件并重启进程"
      : error.message;
    toast(error instanceof SyntaxError ? `JSON 格式错误：${error.message}` : message, true);
  }
}

async function applySessionConfig() {
  elements.applySessionConfigButton.disabled = true;
  try {
    if (state.systemProxyEnabled) await configureSystemProxy(false, false);
    const snapshot = await api("/config/apply", { method: "POST", body: JSON.stringify({ mode: "restart-session" }) });
    updateConfigSnapshot(snapshot);
    await loadConfig();
    await refreshAll();
    toast("Session 配置已应用");
  } catch (error) {
    await Promise.allSettled([loadConfig(), loadSessionStatus()]);
    const rollback = state.sessionState === "ready"
      ? "当前 Session 已回退到之前的 active 配置，磁盘修改仍然保留。"
      : "磁盘修改仍然保留，但当前 Session 未恢复到 ready。";
    toast(`Session 配置应用失败：${error.message}。${rollback}`, true);
  }
  finally { elements.applySessionConfigButton.disabled = false; }
}

async function restartManagedCore() {
  if (!state.managed || !window.confirm("重启 Core 会暂时中断当前连接，是否继续？")) return;
  elements.restartCoreButton.disabled = true;
  try {
    if (state.systemProxyEnabled) await configureSystemProxy(false, false);
    await webui("/restart", { method: "POST" });
    setConnected(false);
    renderSessionStatus("stopped", { message: "Core 正在重启" });
    if (!await connectManagedCore()) throw new Error("Core 重启后暂时无法连接");
    toast("Core 已重启，新配置已生效");
  } catch (error) { toast(error.message, true); }
  finally { elements.restartCoreButton.disabled = false; }
}

async function startSession() {
  setSessionToggleBusy(true);
  try {
    if (!state.config) await loadConfig();
    if (["failed", "stopped"].includes(state.sessionState)) {
      try {
        await api(`/sessions/${encodeURIComponent(state.sessionId)}`, { method: "DELETE" });
      } catch (error) {
        if (error.status !== 404) throw error;
      }
    }
    const result = await api("/sessions", {
      method: "POST",
      body: JSON.stringify({ session_id: state.sessionId, resume: "auto" }),
    });
    state.sessionId = result.session_id || state.sessionId;
    toast("会话启动请求已提交");
    await refreshSessionInfo();
  } catch (error) { toast(error.message, true); }
  finally { setSessionToggleBusy(false); }
}

async function stopSession() {
  setSessionToggleBusy(true);
  try {
    if (state.systemProxyEnabled) await configureSystemProxy(false, false);
    await api(`/sessions/${encodeURIComponent(state.sessionId)}`, { method: "DELETE" });
    toast("会话已停止");
    await loadSessionStatus();
  } catch (error) { toast(error.message, true); }
  finally { setSessionToggleBusy(false); }
}

function toggleSession() {
  if (state.sessionToggleBusy) return;
  if (isSessionRunning(state.sessionState)) stopSession();
  else startSession();
}

function isSessionRunning(sessionState) {
  return !["idle", "stopped", "failed", ""].includes(sessionState || "");
}

function setSessionToggleBusy(busy) {
  state.sessionToggleBusy = busy;
  updateSessionToggle();
}

function updateSessionToggle() {
  const running = isSessionRunning(state.sessionState);
  elements.sessionToggle.setAttribute("aria-checked", String(running));
  elements.sessionToggle.disabled = state.sessionToggleBusy || state.sessionState === "stopping";
  elements.sessionToggleLabel.textContent = state.sessionToggleBusy ? "处理中" : running ? "停止会话" : "启动会话";
}

async function changeRoutingMode() {
  try {
    if (state.managed) {
      const snapshot = await webui("/routing", { method: "PUT", body: JSON.stringify({ mode: elements.routingMode.value }) });
      updateConfigSnapshot(snapshot);
    } else {
      await api(`/sessions/${encodeURIComponent(state.sessionId)}/routing`, { method: "PUT", body: JSON.stringify({ mode: elements.routingMode.value }) });
    }
    await loadConfig();
    toast("路由模式已切换");
  } catch (error) { toast(error.message, true); }
}

async function refreshAll() {
  await Promise.allSettled([loadSessionStatus(), loadTraffic(), loadServices(), loadRoutingMode()]);
}

async function refreshSessionInfo() {
  elements.refreshSessionInfoButton.disabled = true;
  try {
    await Promise.allSettled([loadConfig(false), loadSessionStatus(), loadTraffic()]);
  } finally {
    elements.refreshSessionInfoButton.disabled = false;
  }
}

async function loadSessionStatus() {
  try {
    const status = await api(`/sessions/${encodeURIComponent(state.sessionId)}`);
    renderSessionStatus(status.state, status.last_error);
  } catch (error) {
    if (error.status === 404) renderSessionStatus("idle"); else handleBackgroundError(error);
  }
}

function renderSessionStatus(sessionState, lastError) {
  state.sessionState = sessionState || "idle";
  elements.sessionState.textContent = stateNames[sessionState] || sessionState || "未知";
  elements.sessionPulse.className = "pulse";
  if (sessionState === "ready") elements.sessionPulse.classList.add("ready");
  else if (sessionState === "failed") elements.sessionPulse.classList.add("failed");
  else if (!["idle", "stopped"].includes(sessionState)) elements.sessionPulse.classList.add("working");
  elements.sessionHint.textContent = lastError?.message || (sessionState === "ready" ? "隧道与本地服务运行正常。" : "Core 正在等待操作或处理连接流程。");
  updateSessionToggle();
  updateSystemProxyToggle();
  if (state.systemProxyEnabled && !state.systemProxyBusy && ["idle", "stopped", "failed"].includes(state.sessionState)) {
    configureSystemProxy(false, false).catch(error => toast(`关闭系统代理失败：${error.message}`, true));
  }
}

async function loadTraffic() {
  try {
    const traffic = await api(`/sessions/${encodeURIComponent(state.sessionId)}/traffic`);
    const previous = state.traffic;
    state.traffic = traffic;
    elements.downloaded.textContent = formatBytes(traffic.downloaded_bytes);
    elements.uploaded.textContent = formatBytes(traffic.uploaded_bytes);
    elements.totalTraffic.textContent = formatBytes(traffic.downloaded_bytes + traffic.uploaded_bytes);
    elements.activeConnections.textContent = traffic.active_connections;
    elements.uptime.textContent = formatDuration(Date.now() - new Date(traffic.started_at).getTime());
    let downloadRate = 0;
    let uploadRate = 0;
    if (previous) {
      const seconds = Math.max(.1, (new Date(traffic.timestamp) - new Date(previous.timestamp)) / 1000);
      downloadRate = Math.max(0, traffic.downloaded_bytes - previous.downloaded_bytes) / seconds;
      uploadRate = Math.max(0, traffic.uploaded_bytes - previous.uploaded_bytes) / seconds;
    }
    elements.downloadRate.textContent = `${formatBytes(downloadRate)}/s`;
    elements.uploadRate.textContent = `${formatBytes(uploadRate)}/s`;
    pushSpeedSample(downloadRate, uploadRate);
    drawTrafficCharts();
  } catch (error) {
    if (![404, 409].includes(error.status)) handleBackgroundError(error);
  }
}

async function loadServices() {
  try {
    const services = await api(`/sessions/${encodeURIComponent(state.sessionId)}/services`);
    const list = Array.isArray(services) ? services : services.services || [];
    elements.servicesList.classList.toggle("empty", list.length === 0);
    elements.servicesList.innerHTML = list.length ? list.map(service => `<div class="service-item"><strong>${escapeHTML(service.type)}</strong><span>${service.running ? "运行中" : "已停止"}</span><small>${escapeHTML(service.address || service.error || "—")}</small></div>`).join("") : "暂无服务信息";
  } catch (error) {
    if (![404, 409].includes(error.status)) handleBackgroundError(error);
  }
}

async function loadRoutingMode() {
  try {
    const result = await api(`/sessions/${encodeURIComponent(state.sessionId)}/routing`);
    elements.routingMode.value = result.mode;
  } catch (error) {
    if (![404, 409].includes(error.status)) handleBackgroundError(error);
  }
}

async function loadConnections() {
  if (state.connectionsLoading) return;
  state.connectionsLoading = true;
  try {
    const result = await api(`/sessions/${encodeURIComponent(state.sessionId)}/connections`);
    const connections = Array.isArray(result) ? result : result.connections || [];
    state.connections = connections;
    elements.connectionsEmpty.hidden = connections.length > 0;
    elements.connectionsList.innerHTML = connections.map(connection => `<button type="button" class="connection-card" data-connection-id="${escapeHTML(connection.id)}"><strong class="connection-destination">${escapeHTML(connection.destination)}</strong><span class="connection-card-meta"><span><small>活跃时间</small><strong>${escapeHTML(formatConnectionActivity(connection))}</strong></span><span class="route"><small>路由</small><strong>${escapeHTML(formatConnectionRoute(connection))}</strong></span><span><small>上行流量</small><strong>${formatBytes(connection.uploaded_bytes)}</strong></span><span><small>下行流量</small><strong>${formatBytes(connection.downloaded_bytes)}</strong></span></span></button>`).join("");
    elements.connectionsList.querySelectorAll(".connection-card").forEach(card => card.addEventListener("click", () => showConnectionDetails(card.dataset.connectionId)));
  } catch (error) {
    if ([404, 409].includes(error.status)) { state.connections = []; elements.connectionsList.innerHTML = ""; elements.connectionsEmpty.hidden = false; }
    else toast(error.message, true);
  } finally { state.connectionsLoading = false; }
}

async function closeConnection(id) {
  try {
    await api(`/sessions/${encodeURIComponent(state.sessionId)}/connections/${encodeURIComponent(id)}`, { method: "DELETE" });
    if (state.selectedConnection?.id === id) closeConnectionDialog();
    await loadConnections();
  } catch (error) { toast(error.message, true); }
}

function showConnectionDetails(id) {
  const connection = state.connections.find(item => item.id === id);
  if (!connection) return;
  state.selectedConnection = connection;
  elements.connectionDialogTitle.textContent = connection.destination || "连接详情";
  const details = [
    ["连接 ID", connection.id], ["Session ID", connection.session_id],
    ["目标地址", connection.destination], ["来源地址", connection.source],
    ["网络协议", connection.network], ["状态", connection.state],
    ["入站", connection.inbound], ["出站", connection.outbound],
    ["路由原因", connection.route_reason], ["传输连接 ID", connection.transport_connection_id],
    ["上传流量", formatBytes(connection.uploaded_bytes)], ["下载流量", formatBytes(connection.downloaded_bytes)],
    ["建立时间", formatDateTime(connection.opened_at)], ["最后活动", formatDateTime(connection.last_activity_at)],
  ];
  elements.connectionDetails.innerHTML = details.map(([label, value]) => `<div><dt>${escapeHTML(label)}</dt><dd>${escapeHTML(value || "—")}</dd></div>`).join("");
  if (!elements.connectionDialog.open) elements.connectionDialog.showModal();
}

function closeConnectionDialog() {
  state.selectedConnection = null;
  if (elements.connectionDialog.open) elements.connectionDialog.close();
}

function formatConnectionActivity(connection) {
  const openedAt = new Date(connection.opened_at).getTime();
  return Number.isFinite(openedAt) ? formatPreciseDuration(Date.now() - openedAt) : "—";
}

function formatConnectionRoute(connection) {
  const outbound = connection.outbound || "—";
  return connection.route_reason ? `${outbound} · ${connection.route_reason}` : outbound;
}

function subscribeEvents() {
  state.eventSource?.close();
  const query = new URLSearchParams({ access_token: state.token });
  state.eventSource = new EventSource(`${API}/events?${query}`);
  const types = ["session.state_changed", "auth.required", "auth.browser_required", "auth.completed", "resources.updated", "node.selected", "service.started", "service.stopped", "session.error", "session.reconnect_scheduled", "session.reconnect_failed", "session.reconnected", "routing.mode_changed", "shutdown.completed", "session.resume_state_updated", "session.resume_state_invalidated", "log"];
  types.forEach(type => state.eventSource.addEventListener(type, event => handleEvent(JSON.parse(event.data))));
  state.eventSource.onopen = () => setConnected(true);
  state.eventSource.onerror = () => {
    setConnected(false);
    elements.connectionText.textContent = "事件流重连中";
  };
}

function handleEvent(event) {
  appendEvent(event);
  if (event.session_id) state.sessionId = event.session_id;
  if (event.type === "session.state_changed") {
    renderSessionStatus(event.state);
    if (event.state === "ready") loadConfig(false).catch(handleBackgroundError);
  }
  if (["auth.required", "auth.browser_required"].includes(event.type) && event.auth) showAuth(event.auth);
  if (event.type === "auth.completed") closeAuth();
  if (event.type === "routing.mode_changed") elements.routingMode.value = event.routing_mode;
  if (["resources.updated", "service.started", "service.stopped", "session.reconnected"].includes(event.type)) refreshAll();
  if (event.error) toast(event.error.message || "Core 发生错误", true);
}

function appendEvent(event) {
  if (elements.eventLog.classList.contains("empty")) { elements.eventLog.innerHTML = ""; elements.eventLog.classList.remove("empty"); }
  const row = document.createElement("div");
  row.className = "event-item";
  row.innerHTML = `<time>${new Date(event.timestamp || Date.now()).toLocaleTimeString()}</time><strong>${escapeHTML(event.type)}</strong><code>${escapeHTML(summarizeEvent(event))}</code>`;
  elements.eventLog.prepend(row);
  while (elements.eventLog.children.length > 100) elements.eventLog.lastElementChild.remove();
}

function summarizeEvent(event) {
  const copy = { ...event };
  delete copy.type; delete copy.timestamp; delete copy.session_id;
  if (copy.auth?.image) copy.auth = { ...copy.auth, image: "[base64 image]" };
  return JSON.stringify(copy);
}

function showAuth(challenge) {
  state.challenge = challenge;
  state.selectedChoice = "";
  state.graphPoints = [];
  elements.authTitle.textContent = authKindName(challenge.kind);
  elements.authPrompt.textContent = challenge.prompt || "请完成 Core 请求的认证步骤。";
  elements.authValue.value = "";
  const isGraphClick = challenge.kind === "graph_click";
  elements.authImageStage.classList.toggle("visible", Boolean(challenge.image));
  elements.authImage.classList.toggle("clickable", isGraphClick);
  if (challenge.image) elements.authImage.src = `data:image/png;base64,${challenge.image}`;
  elements.authLink.classList.toggle("visible", Boolean(challenge.url));
  if (challenge.url) elements.authLink.href = challenge.url;
  const choices = challenge.choices || [];
  elements.authChoices.innerHTML = choices.map(choice => `<button type="button" class="choice" data-choice="${escapeHTML(choice.id)}"><strong>${escapeHTML(choice.label)}</strong>${choice.description ? `<small>${escapeHTML(choice.description)}</small>` : ""}</button>`).join("");
  elements.authChoices.querySelectorAll(".choice").forEach(button => button.addEventListener("click", () => {
    state.selectedChoice = button.dataset.choice;
    elements.authChoices.querySelectorAll(".choice").forEach(item => item.classList.toggle("selected", item === button));
  }));
  const isChoice = challenge.kind === "select_authentication_method";
  const canSkipSecondaryAuth = challenge.kind === "secondary_sms" && challenge.allow_skip;
  elements.authValueLabel.hidden = isChoice || isGraphClick;
  elements.graphClickPanel.hidden = !isGraphClick;
  renderGraphPoints();
  elements.skipSecondaryAuth.checked = false;
  elements.skipSecondaryAuthLabel.hidden = !canSkipSecondaryAuth;
  elements.authValue.type = challenge.kind === "password" ? "password" : "text";
  if (!elements.authDialog.open) elements.authDialog.showModal();
}

async function submitAuth() {
  if (!state.challenge) return;
  const response = { challenge_id: state.challenge.id };
  if (state.challenge.kind === "select_authentication_method") response.choice_id = state.selectedChoice;
  else if (state.challenge.kind === "graph_click") {
    if (state.graphPoints.length === 0) {
      toast("请先点击验证码中的目标位置", true);
      return;
    }
    response.value = JSON.stringify(graphClickPayload());
  } else response.value = elements.authValue.value;
  if (state.challenge.kind === "secondary_sms" && elements.skipSecondaryAuth.checked) response.skip = true;
  try {
    await api("/auth/responses", { method: "POST", body: JSON.stringify(response) });
    closeAuth();
    toast("认证信息已提交");
  } catch (error) { toast(error.message, true); }
}

function closeAuth() {
  state.challenge = null;
  state.graphPoints = [];
  if (elements.authDialog.open) elements.authDialog.close();
}

function addGraphPoint(event) {
  if (state.challenge?.kind !== "graph_click" || !elements.authImage.naturalWidth || !elements.authImage.naturalHeight) return;
  const bounds = elements.authImage.getBoundingClientRect();
  const x = Math.max(0, Math.min(elements.authImage.naturalWidth - 1, Math.round((event.clientX - bounds.left) * elements.authImage.naturalWidth / bounds.width)));
  const y = Math.max(0, Math.min(elements.authImage.naturalHeight - 1, Math.round((event.clientY - bounds.top) * elements.authImage.naturalHeight / bounds.height)));
  state.graphPoints.push([x, y]);
  renderGraphPoints();
}

function graphClickPayload() {
  return {
    coordinates: state.graphPoints,
    width: elements.authImage.naturalWidth,
    height: elements.authImage.naturalHeight,
  };
}

function renderGraphPoints() {
  const width = elements.authImage.naturalWidth;
  const height = elements.authImage.naturalHeight;
  elements.authMarkers.replaceChildren();
  if (width && height) {
    elements.authMarkers.setAttribute("viewBox", `0 0 ${width} ${height}`);
    const unitsPerPixel = width / Math.max(elements.authImage.clientWidth, 1);
    state.graphPoints.forEach(([x, y], index) => {
      const group = document.createElementNS("http://www.w3.org/2000/svg", "g");
      const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      const number = document.createElementNS("http://www.w3.org/2000/svg", "text");
      const coordinate = document.createElementNS("http://www.w3.org/2000/svg", "text");
      const radius = 12 * unitsPerPixel;

      circle.setAttribute("class", "auth-marker-circle");
      circle.setAttribute("cx", x);
      circle.setAttribute("cy", y);
      circle.setAttribute("r", radius);
      number.setAttribute("class", "auth-marker-number");
      number.setAttribute("x", x);
      number.setAttribute("y", y);
      number.setAttribute("font-size", 11 * unitsPerPixel);
      number.textContent = String(index + 1);
      coordinate.setAttribute("class", "auth-marker-coordinate");
      coordinate.setAttribute("x", x + 17 * unitsPerPixel);
      coordinate.setAttribute("y", y - 13 * unitsPerPixel);
      coordinate.setAttribute("font-size", 11 * unitsPerPixel);
      coordinate.textContent = `(${x}, ${y})`;
      group.append(circle, number, coordinate);
      elements.authMarkers.append(group);
    });
  }
  elements.undoGraphClickButton.disabled = state.graphPoints.length === 0;
  elements.clearGraphClicksButton.disabled = state.graphPoints.length === 0;
}

function authKindName(kind) {
  return ({ password: "密码认证", sms: "短信验证码", secondary_sms: "二次短信认证", totp: "动态验证码", cas_callback: "CAS 认证", oauth_callback: "OAuth2 认证", graph_text: "图形验证码", graph_click: "图形点击认证", select_authentication_method: "选择认证方式" })[kind] || "需要认证";
}

function startPolling() {
  clearInterval(state.trafficTimer); clearInterval(state.statusTimer);
  state.trafficTimer = setInterval(loadTraffic, 2000);
  state.statusTimer = setInterval(loadSessionStatus, 8000);
}

function startConnectionsPolling() {
  stopConnectionsPolling();
  state.connectionsTimer = setInterval(() => {
    if (state.currentView === "connections") loadConnections();
  }, 2000);
}

function stopConnectionsPolling() {
  clearInterval(state.connectionsTimer);
  state.connectionsTimer = null;
}

function pushSpeedSample(download, upload) {
  state.speedHistory.download.push(download);
  state.speedHistory.upload.push(upload);
  if (state.speedHistory.download.length > 60) state.speedHistory.download.shift();
  if (state.speedHistory.upload.length > 60) state.speedHistory.upload.shift();
}

function drawTrafficCharts() {
  drawSpeedChart();
  drawTrafficDonut();
}

function prepareCanvas(canvas) {
  const bounds = canvas.getBoundingClientRect();
  if (bounds.width <= 0 || bounds.height <= 0) return null;
  const ratio = window.devicePixelRatio || 1;
  const pixelWidth = Math.round(bounds.width * ratio);
  const pixelHeight = Math.round(bounds.height * ratio);
  if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
    canvas.width = pixelWidth;
    canvas.height = pixelHeight;
  }
  const context = canvas.getContext("2d");
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, bounds.width, bounds.height);
  return { context, width: bounds.width, height: bounds.height };
}

function drawSpeedChart() {
  const prepared = prepareCanvas(elements.speedChart);
  if (!prepared) return;
  const { context, width, height } = prepared;
  const padding = { top: 16, right: 8, bottom: 20, left: 8 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;
  const download = state.speedHistory.download;
  const upload = state.speedHistory.upload;
  const maximum = Math.max(1024, ...download, ...upload);

  context.lineWidth = 1;
  context.strokeStyle = "rgba(137, 162, 154, .13)";
  for (let row = 0; row <= 4; row++) {
    const y = padding.top + chartHeight * row / 4;
    context.beginPath();
    context.moveTo(padding.left, y);
    context.lineTo(width - padding.right, y);
    context.stroke();
  }

  drawSpeedSeries(context, download, maximum, padding, chartWidth, chartHeight, "#55e6a5", "rgba(85, 230, 165, .13)");
  drawSpeedSeries(context, upload, maximum, padding, chartWidth, chartHeight, "#68a9ff", "rgba(104, 169, 255, .09)");

  context.fillStyle = "#6f8c82";
  context.font = "10px ui-monospace, monospace";
  context.fillText(`${formatBytes(maximum)}/s`, padding.left, 10);
  context.textAlign = "right";
  context.fillText("最近 2 分钟", width - padding.right, height - 3);
  context.textAlign = "left";
}

function drawSpeedSeries(context, values, maximum, padding, chartWidth, chartHeight, strokeColor, fillColor) {
  if (values.length === 0) return;
  const points = values.map((value, index) => ({
    x: padding.left + (values.length === 1 ? chartWidth : chartWidth * index / (values.length - 1)),
    y: padding.top + chartHeight - Math.min(1, value / maximum) * chartHeight,
  }));
  context.beginPath();
  context.moveTo(points[0].x, points[0].y);
  for (let index = 1; index < points.length; index++) {
    const previous = points[index - 1];
    const current = points[index];
    const middleX = (previous.x + current.x) / 2;
    context.bezierCurveTo(middleX, previous.y, middleX, current.y, current.x, current.y);
  }
  context.lineWidth = 2;
  context.strokeStyle = strokeColor;
  context.stroke();
  context.lineTo(points[points.length - 1].x, padding.top + chartHeight);
  context.lineTo(points[0].x, padding.top + chartHeight);
  context.closePath();
  context.fillStyle = fillColor;
  context.fill();
}

function drawTrafficDonut() {
  const prepared = prepareCanvas(elements.trafficDonut);
  if (!prepared) return;
  const { context, width, height } = prepared;
  const downloaded = state.traffic?.downloaded_bytes || 0;
  const uploaded = state.traffic?.uploaded_bytes || 0;
  const total = downloaded + uploaded;
  const radius = Math.max(10, Math.min(width, height) / 2 - 12);
  const centerX = width / 2;
  const centerY = height / 2;
  const start = -Math.PI / 2;

  context.lineWidth = 15;
  context.lineCap = "round";
  context.strokeStyle = "rgba(137, 162, 154, .13)";
  context.beginPath();
  context.arc(centerX, centerY, radius, 0, Math.PI * 2);
  context.stroke();
  if (total <= 0) return;

  const downloadAngle = Math.PI * 2 * downloaded / total;
  context.lineCap = "butt";
  context.strokeStyle = "#55e6a5";
  context.beginPath();
  context.arc(centerX, centerY, radius, start, start + downloadAngle);
  context.stroke();
  context.strokeStyle = "#68a9ff";
  context.beginPath();
  context.arc(centerX, centerY, radius, start + downloadAngle, start + Math.PI * 2);
  context.stroke();
}

let lastBackgroundError = "";
function handleBackgroundError(error) {
  if (error.status === 401) {
    setConnected(false);
    if (!state.managed) openConnectDialog();
  }
  if (error.message !== lastBackgroundError) { lastBackgroundError = error.message; console.warn(error); }
}

function formatBytes(value = 0) {
  if (!Number.isFinite(value) || value < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let size = value, unit = 0;
  while (size >= 1024 && unit < units.length - 1) { size /= 1024; unit++; }
  return `${size.toFixed(unit === 0 ? 0 : size >= 100 ? 0 : size >= 10 ? 1 : 2)} ${units[unit]}`;
}

function formatDuration(milliseconds) {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  const days = Math.floor(seconds / 86400), hours = Math.floor(seconds % 86400 / 3600), minutes = Math.floor(seconds % 3600 / 60);
  return days ? `${days}天 ${hours}小时` : hours ? `${hours}小时 ${minutes}分钟` : `${minutes}分钟`;
}

function formatPreciseDuration(milliseconds) {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return "—";
  const seconds = Math.floor(milliseconds / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds % 3600 / 60);
  const remainder = seconds % 60;
  if (hours) return `${hours}时 ${minutes}分`;
  if (minutes) return `${minutes}分 ${remainder}秒`;
  return `${remainder}秒`;
}

function formatDateTime(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "—" : date.toLocaleString();
}

function escapeHTML(value = "") {
  return String(value).replace(/[&<>'"]/g, char => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[char]);
}

function toast(message, error = false) {
  const item = document.createElement("div");
  item.className = `toast${error ? " error" : ""}`;
  item.textContent = message;
  elements.toastContainer.append(item);
  setTimeout(() => item.remove(), 4500);
}

function delay(milliseconds) {
  return new Promise(resolve => setTimeout(resolve, milliseconds));
}
