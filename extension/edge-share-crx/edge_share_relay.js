"use strict";

const PROTOCOL_VERSION = 1;
const DEFAULT_RELAY_URL = "http://127.0.0.1:8765";
const STORAGE_KEY = "edgeShareState";
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const RELAY_KEEPALIVE_MS = 20 * 1000;
const PLATFORM_CONNECT_TTL_MS = 2 * 60 * 1000;
const PLATFORM_RELAY_CONNECT_TIMEOUT_MS = 8000;
const MAX_RECORDED_ACTIONS = 500;

let state = {
  sharing: false,
  connected: false,
  relayUrl: DEFAULT_RELAY_URL,
  sessionId: "",
  browserToken: "",
  agentToken: "",
  shareUrl: "",
  status: "idle",
  lastError: "",
  activeTabId: null,
  platformOrigin: "",
  platformHref: "",
  workspaceId: "",
  poolId: "",
  browserName: "",
  recording: false,
  recordingTabId: null,
  recorderMode: "record",
  recordingStartedAt: "",
  recordingStoppedAt: "",
  recordedActions: [],
  lastInspect: null,
  playbackRunning: false,
  playbackError: "",
  playbackStepId: "",
  updatedAt: ""
};

let socket = null;
let reconnectTimer = null;
let keepAliveTimer = null;
let reconnectDelayMs = RECONNECT_MIN_MS;
let intentionallyClosed = false;
const attachedTabs = new Set();
const pendingPlatformRequests = new Map();
let crxRecorderState = {
  mode: "none",
  sources: []
};

chrome.runtime.onInstalled.addListener(() => {
  restoreState().then(connectIfNeeded).catch(reportError);
});

chrome.runtime.onStartup.addListener(() => {
  restoreState().then(connectIfNeeded).catch(reportError);
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  handleExtensionMessage(message, sender)
    .then((result) => sendResponse({ ok: true, result }))
    .catch((error) => sendResponse({ ok: false, error: errorMessage(error) }));
  return true;
});

chrome.debugger.onDetach.addListener((source) => {
  if (Number.isInteger(source.tabId)) {
    attachedTabs.delete(source.tabId);
  }
});

restoreState().then(connectIfNeeded).catch(reportError);

async function handleExtensionMessage(message, sender) {
  if (!message || typeof message.type !== "string") {
    throw new Error("Invalid message");
  }

  if (message.type === "edge_share_crx_recorder_update") {
    crxRecorderState = {
      ...crxRecorderState,
      mode: typeof message.mode === "string" ? message.mode : crxRecorderState.mode,
      sources: Array.isArray(message.sources) ? message.sources : crxRecorderState.sources
    };
    notifyRecordingChanged();
    return recordingState();
  }

  if (message.type === "get_state") {
    await restoreState();
    return publicState();
  }

  if (message.type === "get_recording") {
    await restoreState();
    return recordingState();
  }

  if (message.type === "start_share") {
    return startShare(message.relayUrl || DEFAULT_RELAY_URL);
  }

  if (message.type === "start_recording") {
    await restoreState();
    return startRecording(message.params || {});
  }

  if (message.type === "set_recording_mode") {
    await restoreState();
    return setRecordingMode(message.mode);
  }

  if (message.type === "stop_recording") {
    await restoreState();
    return stopRecording();
  }

  if (message.type === "clear_recording") {
    await restoreState();
    return clearRecording();
  }

  if (message.type === "recorder_action") {
    await restoreState();
    return recordContentAction(message.action || {}, sender);
  }

  if (message.type === "inspect_target") {
    await restoreState();
    return recordInspectTarget(message.target || {}, sender);
  }

  if (message.type === "play_recording") {
    await restoreState();
    return playRecording();
  }

  if (message.type === "platform_request") {
    return handlePlatformRequest(message, sender);
  }

  if (message.type === "get_platform_connect_request") {
    return getPlatformConnectRequest(message.requestId);
  }

  if (message.type === "approve_platform_connect") {
    return approvePlatformConnect(message.requestId);
  }

  if (message.type === "reject_platform_connect") {
    return rejectPlatformConnect(message.requestId);
  }

  if (message.type === "stop_share") {
    return stopShare();
  }

  throw new Error(`Unknown popup message: ${message.type}`);
}

async function startShare(relayUrlInput, options = {}) {
  const relayUrl = normalizeRelayUrl(relayUrlInput);
  const sessionId = randomHex(12);
  const browserToken = randomHex(24);
  const agentToken = randomHex(24);
  const shareUrl = buildShareUrl(relayUrl, sessionId, agentToken);

  intentionallyClosed = false;
  reconnectDelayMs = RECONNECT_MIN_MS;

  await persistState({
    sharing: true,
    connected: false,
    relayUrl,
    sessionId,
    browserToken,
    agentToken,
    shareUrl,
    status: "connecting",
    lastError: "",
    activeTabId: null,
    platformOrigin: options.platformOrigin || "",
    platformHref: options.platformHref || "",
    workspaceId: options.workspaceId || "",
    poolId: options.poolId || "",
    browserName: options.browserName || ""
  });

  connectRelay();
  return publicState();
}

async function stopShare() {
  intentionallyClosed = true;
  clearReconnectTimer();
  clearKeepAliveTimer();

  if (socket) {
    try {
      socket.close(1000, "sharing stopped");
    } catch (_error) {
      // Ignore close errors while stopping.
    }
    socket = null;
  }

  await detachAllDebuggers();

  await persistState({
    sharing: false,
    connected: false,
    sessionId: "",
    browserToken: "",
    agentToken: "",
    shareUrl: "",
    status: "stopped",
    lastError: "",
    recording: false,
    recordingTabId: null,
    recorderMode: "record",
    recordingStoppedAt: new Date().toISOString(),
    playbackRunning: false,
    playbackError: "",
    playbackStepId: "",
    platformOrigin: "",
    platformHref: "",
    workspaceId: "",
    poolId: "",
    browserName: ""
  });

  return publicState();
}

async function restoreState() {
  const stored = await chromeCall(chrome.storage.local.get, chrome.storage.local, STORAGE_KEY);
  if (stored && stored[STORAGE_KEY]) {
    state = { ...state, ...stored[STORAGE_KEY] };
  }
}

async function persistState(patch) {
  state = {
    ...state,
    ...patch,
    updatedAt: new Date().toISOString()
  };

  await chromeCall(chrome.storage.local.set, chrome.storage.local, {
    [STORAGE_KEY]: state
  });

  notifyPopup();
}

function publicState() {
  return {
    sharing: state.sharing,
    connected: state.connected,
    relayUrl: state.relayUrl || DEFAULT_RELAY_URL,
    sessionId: state.sessionId,
    shareUrl: state.shareUrl,
    status: state.status,
    lastError: state.lastError,
    activeTabId: state.activeTabId,
    recording: state.recording,
    recordingTabId: state.recordingTabId,
    recorderMode: state.recorderMode || "record",
    recordingCount: Array.isArray(state.recordedActions) ? state.recordedActions.length : 0,
    recordingStartedAt: state.recordingStartedAt,
    recordingStoppedAt: state.recordingStoppedAt,
    platformOrigin: state.platformOrigin,
    workspaceId: state.workspaceId,
    poolId: state.poolId,
    browserName: state.browserName,
    updatedAt: state.updatedAt
  };
}

function notifyPopup() {
  try {
    chrome.runtime.sendMessage({ type: "state_changed", state: publicState() }, () => {
      // Popup may be closed; reading lastError prevents noisy extension logs.
      void chrome.runtime.lastError;
    });
  } catch (_error) {
    // Popup may be closed.
  }
}

function notifyRecordingChanged() {
  const message = {
    type: "recording_state_changed",
    recording: recordingState()
  };

  try {
    chrome.tabs.query({}, (tabs) => {
      const error = chrome.runtime.lastError;
      if (error || !Array.isArray(tabs)) {
        return;
      }
      for (const tab of tabs) {
        if (!Number.isInteger(tab.id) || !isRecordableUrl(tab.url || "")) {
          continue;
        }
        chrome.tabs.sendMessage(tab.id, message, () => {
          // Some pages will not have this content script yet.
          void chrome.runtime.lastError;
        });
      }
    });
  } catch (_error) {
    // Tabs may be unavailable in restricted extension contexts.
  }
}

function connectIfNeeded() {
  if (state.sharing) {
    connectRelay();
  }
}

function connectRelay() {
  if (!state.sharing || !state.relayUrl || !state.sessionId || !state.browserToken) {
    return;
  }

  clearReconnectTimer();
  clearKeepAliveTimer();

  if (socket) {
    try {
      socket.close(1000, "reconnecting");
    } catch (_error) {
      // Continue with a new socket.
    }
  }

  const wsUrl = buildBrowserWsUrl(
    state.relayUrl,
    state.sessionId,
    state.browserToken,
    state.agentToken
  );
  socket = new WebSocket(wsUrl);

  persistState({ connected: false, status: "connecting", lastError: "" }).catch(reportError);

  socket.addEventListener("open", () => {
    reconnectDelayMs = RECONNECT_MIN_MS;
    persistState({ connected: true, status: "connected", lastError: "" }).catch(reportError);
    sendSocket({
      type: "hello",
      role: "browser",
      protocolVersion: PROTOCOL_VERSION,
      sessionId: state.sessionId,
      userAgent: navigator.userAgent
    });
    startKeepAliveTimer();
  });

  socket.addEventListener("message", (event) => {
    handleRelayMessage(event.data).catch((error) => {
      sendSocket({
        type: "event",
        event: "handler_error",
        error: errorMessage(error)
      });
    });
  });

  socket.addEventListener("close", (event) => {
    clearKeepAliveTimer();
    socket = null;
    persistState({
      connected: false,
      status: state.sharing ? "disconnected" : "stopped",
      lastError: event.reason || ""
    }).catch(reportError);

    if (state.sharing && !intentionallyClosed) {
      scheduleReconnect();
    }
  });

  socket.addEventListener("error", () => {
    clearKeepAliveTimer();
    persistState({ lastError: "WebSocket error" }).catch(reportError);
  });
}

function scheduleReconnect() {
  clearReconnectTimer();
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, RECONNECT_MAX_MS);
    connectRelay();
  }, reconnectDelayMs);
}

function clearReconnectTimer() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

function startKeepAliveTimer() {
  clearKeepAliveTimer();
  keepAliveTimer = setInterval(() => {
    sendSocket({
      type: "keepalive",
      at: new Date().toISOString()
    });
  }, RELAY_KEEPALIVE_MS);
}

function clearKeepAliveTimer() {
  if (keepAliveTimer) {
    clearInterval(keepAliveTimer);
    keepAliveTimer = null;
  }
}

async function handlePlatformRequest(message, sender) {
  if (message.method !== "connect") {
    throw new Error(`Unsupported platform request method: ${message.method}`);
  }
  if (!sender || !sender.tab || !Number.isInteger(sender.tab.id) || !sender.url) {
    throw new Error("Platform requests must come from a browser tab");
  }

  const pageUrl = new URL(sender.url);
  if (pageUrl.protocol !== "http:" && pageUrl.protocol !== "https:") {
    throw new Error("Platform requests must come from an http or https page");
  }
  if (message.origin !== pageUrl.origin) {
    throw new Error("Platform request origin did not match the sender tab");
  }

  const params = message.params && typeof message.params === "object" ? message.params : {};
  const requestId = randomHex(12);
  const request = {
    requestId,
    origin: pageUrl.origin,
    href: sender.url,
    title: message.title || sender.tab.title || "",
    tabId: sender.tab.id,
    workspaceId: stringParam(params.workspaceId),
    poolId: stringParam(params.poolId),
    browserName: stringParam(params.displayName || params.browserName) || "Edge",
    relayUrl: normalizeRelayUrl(stringParam(params.relayUrl) || pageUrl.origin),
    createdAt: Date.now()
  };

  if (new URL(request.relayUrl).origin !== request.origin) {
    throw new Error("Platform relayUrl must use the requesting page origin");
  }

  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      pendingPlatformRequests.delete(requestId);
      reject(new Error("Platform connect request expired"));
    }, PLATFORM_CONNECT_TTL_MS);

    pendingPlatformRequests.set(requestId, {
      request,
      resolve,
      reject,
      timeout
    });

    chrome.windows.create(
      {
        url: chrome.runtime.getURL(`connect.html?requestId=${encodeURIComponent(requestId)}`),
        type: "popup",
        width: 420,
        height: 560
      },
      () => {
        const error = chrome.runtime.lastError;
        if (error) {
          clearTimeout(timeout);
          pendingPlatformRequests.delete(requestId);
          reject(new Error(error.message));
        }
      }
    );
  });
}

async function getPlatformConnectRequest(requestId) {
  const entry = pendingPlatformRequests.get(String(requestId || ""));
  if (!entry) {
    throw new Error("Platform connect request was not found or expired");
  }
  return sanitizePlatformRequest(entry.request);
}

async function approvePlatformConnect(requestId) {
  const id = String(requestId || "");
  const entry = pendingPlatformRequests.get(id);
  if (!entry) {
    throw new Error("Platform connect request was not found or expired");
  }

  pendingPlatformRequests.delete(id);
  clearTimeout(entry.timeout);

  let started = false;
  try {
    await startShare(entry.request.relayUrl, {
      platformOrigin: entry.request.origin,
      platformHref: entry.request.href,
      workspaceId: entry.request.workspaceId,
      poolId: entry.request.poolId,
      browserName: entry.request.browserName
    });
    started = true;
    if (!(await waitForRelayConnection(PLATFORM_RELAY_CONNECT_TIMEOUT_MS))) {
      throw new Error(state.lastError || "Timed out connecting to browser relay");
    }
    const result = publicState();
    const response = {
      shareUrl: result.shareUrl,
      sessionId: result.sessionId,
      connected: result.connected,
      relayUrl: result.relayUrl,
      platformOrigin: entry.request.origin,
      workspaceId: entry.request.workspaceId,
      poolId: entry.request.poolId,
      browserName: entry.request.browserName
    };
    entry.resolve(response);
    return response;
  } catch (error) {
    if (started) {
      await stopShare().catch(reportError);
    }
    entry.reject(error);
    throw error;
  }
}

async function rejectPlatformConnect(requestId) {
  const id = String(requestId || "");
  const entry = pendingPlatformRequests.get(id);
  if (!entry) {
    return { rejected: true };
  }

  pendingPlatformRequests.delete(id);
  clearTimeout(entry.timeout);
  const error = new Error("User rejected browser connection");
  entry.reject(error);
  return { rejected: true };
}

function sanitizePlatformRequest(request) {
  return {
    requestId: request.requestId,
    origin: request.origin,
    href: request.href,
    title: request.title,
    workspaceId: request.workspaceId,
    poolId: request.poolId,
    browserName: request.browserName,
    relayUrl: request.relayUrl,
    createdAt: request.createdAt
  };
}

async function waitForRelayConnection(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (state.connected) {
      return true;
    }
    if (!state.sharing) {
      return false;
    }
    await sleep(100);
  }
  return state.connected;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function withTimeout(promise, timeoutMs, label) {
  let timer = null;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`${label} timed out after ${timeoutMs}ms`));
    }, timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer) {
      clearTimeout(timer);
    }
  });
}

async function handleRelayMessage(rawData) {
  let message;
  try {
    message = JSON.parse(rawData);
  } catch (_error) {
    sendSocket({ type: "event", event: "invalid_json" });
    return;
  }

  if (message.type === "ping") {
    sendSocket({ type: "pong", at: new Date().toISOString() });
    return;
  }

  const requestId = message.id || message.requestId;
  const command = message.command || message.method;
  const params = message.params || {};

  if (!requestId || !command) {
    sendSocket({
      type: "event",
      event: "ignored_message",
      reason: "missing id or command"
    });
    return;
  }

  try {
    const result = await handleCommand(command, params);
    sendSocket({
      type: "response",
      id: requestId,
      ok: true,
      result
    });
  } catch (error) {
    sendSocket({
      type: "response",
      id: requestId,
      ok: false,
      error: errorMessage(error)
    });
  }
}

async function handleCommand(command, params) {
  if (command === "navigate") {
    return commandNavigate(params);
  }
  if (command === "evaluate") {
    return commandEvaluate(params);
  }
  if (command === "snapshot") {
    return commandSnapshot(params);
  }
  if (command === "click") {
    return commandClick(params);
  }
  if (command === "type") {
    return commandType(params);
  }
  if (command === "press_key") {
    return commandPressKey(params);
  }
  if (command === "screenshot") {
    return commandScreenshot(params);
  }
  if (command === "list_tabs") {
    return commandListTabs();
  }
  if (command === "activate_tab") {
    return commandActivateTab(params);
  }
  if (command === "run_playwright") {
    return commandRunPlaywright(params);
  }
  if (command === "start_recording") {
    return startRecording(params);
  }
  if (command === "set_recording_mode") {
    return setRecordingMode(params.mode);
  }
  if (command === "stop_recording") {
    return stopRecording();
  }
  if (command === "clear_recording") {
    return clearRecording();
  }
  if (command === "play_recording") {
    return playRecording();
  }
  if (command === "recording_state") {
    return recordingState();
  }

  throw new Error(`Unsupported command: ${command}`);
}

async function commandNavigate(params) {
  if (!params.url || typeof params.url !== "string") {
    throw new Error("navigate requires params.url");
  }

  const tabId = await getTargetTabId(params);
  const tab = await chromeCall(chrome.tabs.update, chrome.tabs, tabId, {
    url: params.url,
    active: params.activate !== false
  });

  state.activeTabId = tab.id;
  return { tab: tabSummary(tab) };
}

async function commandEvaluate(params) {
  if (!params.expression || typeof params.expression !== "string") {
    throw new Error("evaluate requires params.expression");
  }

  const tabId = await getTargetTabId(params);
  const value = await evaluateInPage(tabId, params.expression);
  return {
    tab: await currentTabSummary(tabId),
    value
  };
}

async function commandSnapshot(params) {
  const tabId = await getTargetTabId(params);
  const snapshot = await runFunctionInPage(tabId, createSnapshot, [
    params.maxTextLength || 200000,
    params.maxItems || 200
  ]);

  return {
    tab: await currentTabSummary(tabId),
    snapshot
  };
}

async function commandClick(params) {
  if (!params.selector || typeof params.selector !== "string") {
    throw new Error("click requires params.selector");
  }

  const tabId = await getTargetTabId(params);
  const result = await runFunctionInPage(tabId, clickSelector, [params.selector]);
  return {
    tab: await currentTabSummary(tabId),
    action: result
  };
}

async function commandType(params) {
  if (typeof params.text !== "string") {
    throw new Error("type requires params.text");
  }

  const tabId = await getTargetTabId(params);
  const result = await runFunctionInPage(tabId, typeText, [
    params.selector || "",
    params.text,
    params.replace === true || params.clear === true
  ]);

  return {
    tab: await currentTabSummary(tabId),
    action: result
  };
}

async function commandPressKey(params) {
  if (!params.key || typeof params.key !== "string") {
    throw new Error("press_key requires params.key");
  }

  const tabId = await getTargetTabId(params);
  const key = normalizeKey(params.key);
  await sendKeyEvent(tabId, key, "rawKeyDown");

  if (key.text) {
    await sendKeyEvent(tabId, key, "char");
  }

  await sendKeyEvent(tabId, key, "keyUp");

  return {
    tab: await currentTabSummary(tabId),
    action: { key: key.key, code: key.code }
  };
}

async function commandScreenshot(params) {
  const tabId = await getTargetTabId(params);
  const format = params.format === "jpeg" ? "jpeg" : "png";
  const quality = format === "jpeg" ? clampNumber(params.quality || 80, 1, 100) : undefined;

  try {
    await sendCdp(tabId, "Page.enable", {});
    const result = await sendCdp(tabId, "Page.captureScreenshot", {
      format,
      quality,
      fromSurface: true,
      captureBeyondViewport: params.fullPage === true
    });

    return {
      tab: await currentTabSummary(tabId),
      mimeType: `image/${format}`,
      data: result.data
    };
  } catch (error) {
    const tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
    await chromeCall(chrome.tabs.update, chrome.tabs, tabId, { active: true });
    const dataUrl = await chromeCall(chrome.tabs.captureVisibleTab, chrome.tabs, tab.windowId, {
      format,
      quality
    });

    return {
      tab: tabSummary(tab),
      mimeType: `image/${format}`,
      dataUrl,
      fallback: "tabs.captureVisibleTab",
      warning: errorMessage(error)
    };
  }
}

async function commandListTabs() {
  const windows = await chromeCall(chrome.windows.getAll, chrome.windows, {
    populate: true
  });
  const summaries = windows.map(windowSummary);
  return {
    windows: summaries,
    tabs: summaries.flatMap((window) => window.tabs)
  };
}

async function commandActivateTab(params) {
  const tabId = await getTargetTabId(params);
  const tab = await chromeCall(chrome.tabs.update, chrome.tabs, tabId, { active: true });
  state.activeTabId = tab.id;
  await persistState({ activeTabId: tab.id });
  return { tab: tabSummary(tab) };
}

async function commandRunPlaywright(params = {}) {
  if (!params || typeof params.code !== "string" || !params.code.trim()) {
    throw new Error("run_playwright requires params.code");
  }
  if (typeof globalThis.getCrxApp !== "function") {
    throw new Error("Playwright CRX runtime is not available in this extension build");
  }
  if (params.allowClose !== true && /\.\s*close\s*\(/.test(params.code)) {
    throw new Error("run_playwright blocks close() by default; pass --allow-close to allow it");
  }

  let tabId = await getTargetTabId(params);
  let tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
  if (/^chrome:\/\//i.test(tab.url || "")) {
    tab = await chromeCall(chrome.tabs.create, chrome.tabs, {
      windowId: tab.windowId,
      url: "about:blank",
      active: true
    });
    tabId = tab.id;
  }
  if (!Number.isInteger(tabId)) {
    throw new Error("run_playwright could not resolve a target tab");
  }

  const code = normalizePlaywrightCrxCode(params.code);
  const timeoutMs = Number.isFinite(Number(params.timeoutMs))
    ? clampNumber(params.timeoutMs, 1000, 10 * 60 * 1000)
    : 0;

  try {
    const app = normalizeCrxApplication(await globalThis.getCrxApp(!!tab.incognito));
    if (!app || typeof app.attach !== "function" || typeof app.run !== "function") {
      throw new Error(`Playwright CRX application does not expose attach/run: ${describeCrxApplication(app)}`);
    }
    const page = await app.attach(tabId);
    const run = app.run(code, page);
    if (timeoutMs > 0) {
      await withTimeout(run, timeoutMs, "run_playwright");
    } else {
      await run;
    }
    state.activeTabId = tabId;
    await persistState({ activeTabId: tabId, lastError: "" });
    return {
      ok: true,
      mode: "playwright-crx",
      tab: await currentTabSummary(tabId)
    };
  } catch (error) {
    await persistState({ lastError: errorMessage(error) }).catch(reportError);
    throw error;
  }
}

function normalizeCrxApplication(app) {
  if (app && typeof app.attach === "function" && typeof app.run === "function") {
    return app;
  }
  if (
    app &&
    app.crxApplication &&
    typeof app.crxApplication.attach === "function" &&
    typeof app.crxApplication.run === "function"
  ) {
    return app.crxApplication;
  }
  if (
    app &&
    app._object &&
    typeof app._object.attach === "function" &&
    typeof app._object.run === "function"
  ) {
    return app._object;
  }
  return app;
}

function describeCrxApplication(app) {
  if (!app || typeof app !== "object") {
    return String(app);
  }
  const keys = Object.keys(app).slice(0, 20).join(",") || "no enumerable keys";
  const proto = Object.getPrototypeOf(app);
  const protoKeys = proto ? Object.getOwnPropertyNames(proto).slice(0, 20).join(",") : "no prototype";
  return `keys=[${keys}] proto=[${protoKeys}]`;
}

async function startRecording(params = {}) {
  const tabId = Number.isInteger(params.tabId) ? params.tabId : await getTargetTabId({});
  const tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
  await attachCrxRecorder(tab, normalizeCrxMode(params.mode));
  const startedAt = new Date().toISOString();
  const actions = [];
  if (tab.url && isRecordableUrl(tab.url)) {
    actions.push(recordedAction("navigate", {
      url: tab.url,
      title: tab.title || "",
      tabId,
      windowId: tab.windowId
    }));
  }

  await persistState({
    recording: true,
    recordingTabId: null,
    recorderMode: normalizeRecorderMode(params.mode),
    recordingStartedAt: startedAt,
    recordingStoppedAt: "",
    recordedActions: actions,
    lastInspect: null,
    playbackError: "",
    playbackStepId: "",
    activeTabId: tabId
  });

  const recording = recordingState();
  notifyRecordingChanged();
  return recording;
}

async function setRecordingMode(mode) {
  if (!state.recording) {
    return startRecording({ mode });
  }
  const tabId = await getTargetTabId({});
  const tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
  await attachCrxRecorder(tab, normalizeCrxMode(mode));
  await persistState({
    recorderMode: normalizeRecorderMode(mode),
    lastInspect: null
  });
  const recording = recordingState();
  notifyRecordingChanged();
  return recording;
}

async function attachCrxRecorder(tab, mode) {
  if (typeof globalThis.attach !== "function") {
    return;
  }
  try {
    await globalThis.attach(tab, mode);
  } catch (error) {
    await persistState({ lastError: errorMessage(error) }).catch(reportError);
    throw error;
  }
}

async function setCrxRecorderStandby() {
  if (typeof globalThis.getCrxApp !== "function") {
    return;
  }
  try {
    const app = await globalThis.getCrxApp(false);
    if (app && app.recorder && typeof app.recorder.setMode === "function") {
      await app.recorder.setMode("standby");
    }
  } catch (_error) {
    // The recorder may not be initialized yet; relay state can still stop.
  }
}

async function stopRecording() {
  await setCrxRecorderStandby();
  await persistState({
    recording: false,
    recorderMode: "record",
    recordingStoppedAt: new Date().toISOString(),
    lastInspect: null,
    playbackRunning: false
  });
  const recording = recordingState();
  notifyRecordingChanged();
  return recording;
}

async function clearRecording() {
  await setCrxRecorderStandby();
  await persistState({
    recording: false,
    recordingTabId: null,
    recorderMode: "record",
    recordingStartedAt: "",
    recordingStoppedAt: "",
    recordedActions: [],
    lastInspect: null,
    playbackRunning: false,
    playbackError: "",
    playbackStepId: ""
  });
  const recording = recordingState();
  notifyRecordingChanged();
  return recording;
}

async function recordContentAction(action, sender) {
  if (!state.recording || !sender?.tab || !Number.isInteger(sender.tab.id)) {
    return { recorded: false };
  }
  if ((state.recorderMode || "record") !== "record") {
    return { recorded: false };
  }
  if (!isRecordableUrl(sender.tab.url || action.url || "")) {
    return { recorded: false };
  }

  const normalized = recordedAction(action.kind, {
    ...action,
    tabId: sender.tab.id,
    windowId: sender.tab.windowId,
    title: action.title || sender.tab.title || "",
    url: action.url || sender.tab.url || ""
  });
  if (!normalized) {
    return { recorded: false };
  }

  const actions = Array.isArray(state.recordedActions) ? [...state.recordedActions] : [];
  upsertRecordedAction(actions, normalized);
  while (actions.length > MAX_RECORDED_ACTIONS) {
    actions.shift();
  }

  await persistState({ recordedActions: actions, activeTabId: sender.tab.id });
  notifyRecordingChanged();
  return { recorded: true, action: normalized, count: actions.length };
}

async function recordInspectTarget(target, sender) {
  if (!state.recording || (state.recorderMode || "record") !== "inspect") {
    return { inspected: false };
  }
  if (!sender?.tab || !Number.isInteger(sender.tab.id)) {
    return { inspected: false };
  }
  if (!isRecordableUrl(sender.tab.url || target.url || "")) {
    return { inspected: false };
  }

  const inspected = {
    at: new Date().toISOString(),
    selector: String(target.selector || "").slice(0, 1024),
    label: String(target.label || "").slice(0, 512),
    tag: String(target.tag || "").slice(0, 64),
    url: String(target.url || sender.tab.url || "").slice(0, 4096),
    title: String(target.title || sender.tab.title || "").slice(0, 512),
    tabId: sender.tab.id,
    windowId: sender.tab.windowId
  };

  await persistState({ lastInspect: inspected, activeTabId: sender.tab.id });
  notifyRecordingChanged();
  return { inspected: true, target: inspected };
}

async function playRecording() {
  const actions = Array.isArray(state.recordedActions) ? [...state.recordedActions] : [];
  if (actions.length === 0) {
    throw new Error("No recorded actions to play");
  }

  await persistState({
    playbackRunning: true,
    playbackError: "",
    playbackStepId: ""
  });
  notifyRecordingChanged();

  let caught = null;
  try {
    for (const action of actions) {
      await persistState({ playbackStepId: action.id || "" });
      notifyRecordingChanged();
      await playRecordedAction(action);
    }
  } catch (error) {
    caught = error;
    await persistState({ playbackError: errorMessage(error) });
  } finally {
    await persistState({ playbackRunning: false });
    notifyRecordingChanged();
  }

  if (caught) {
    throw caught;
  }
  return recordingState();
}

async function playRecordedAction(action) {
  const tabId = await playbackTabId(action);
  if (action.kind === "navigate" && action.url) {
    await chromeCall(chrome.tabs.update, chrome.tabs, tabId, {
      url: action.url,
      active: true
    });
    state.activeTabId = tabId;
    await waitForTabReady(tabId, 10000);
    return;
  }
  if (action.kind === "click" && action.selector) {
    await runFunctionInPage(tabId, clickSelector, [action.selector]);
    return;
  }
  if (action.kind === "fill" && action.selector) {
    await runFunctionInPage(tabId, typeText, [action.selector, action.text || "", true]);
    return;
  }
  if (action.kind === "press" && action.key) {
    if (action.selector) {
      await runFunctionInPage(tabId, focusSelector, [action.selector]);
    }
    const key = normalizeKey(action.key);
    await sendKeyEvent(tabId, key, "rawKeyDown");
    if (key.text) {
      await sendKeyEvent(tabId, key, "char");
    }
    await sendKeyEvent(tabId, key, "keyUp");
  }
}

async function playbackTabId(action) {
  if (Number.isInteger(action.tabId)) {
    try {
      await chromeCall(chrome.tabs.get, chrome.tabs, action.tabId);
      state.activeTabId = action.tabId;
      return action.tabId;
    } catch (_error) {
      // The original tab was closed; fall back to the active tab.
    }
  }
  return getTargetTabId({});
}

async function waitForTabReady(tabId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
      if (tab.status === "complete") {
        return;
      }
    } catch (_error) {
      return;
    }
    await sleep(100);
  }
}

function recordingState() {
  const actions = Array.isArray(state.recordedActions) ? state.recordedActions : [];
  const crxSources = Array.isArray(crxRecorderState.sources) ? crxRecorderState.sources : [];
  const preferredSource =
    crxSources.find((source) => source && source.id === "playwright-test") ||
    crxSources.find((source) => source && typeof source.text === "string") ||
    null;
  return {
    recording: state.recording,
    mode: state.recorderMode || "record",
    crxMode: crxRecorderState.mode || "none",
    tabId: state.recordingTabId,
    startedAt: state.recordingStartedAt,
    stoppedAt: state.recordingStoppedAt,
    count: actions.length,
    actions,
    sources: crxSources,
    lastInspect: state.lastInspect || null,
    playing: !!state.playbackRunning,
    playbackError: state.playbackError || "",
    playbackStepId: state.playbackStepId || "",
    script: preferredSource?.text || renderRecordedPlaywright(actions)
  };
}

function normalizeRecorderMode(mode) {
  return mode === "inspect" ? "inspect" : "record";
}

function normalizeCrxMode(mode) {
  return mode === "inspect" || mode === "inspecting" ? "inspecting" : "recording";
}

function recordedAction(kind, fields) {
  const actionKind = String(kind || "");
  if (!["navigate", "click", "fill", "press"].includes(actionKind)) {
    return null;
  }
  const action = {
    id: randomHex(6),
    kind: actionKind,
    at: new Date().toISOString(),
    url: String(fields.url || "").slice(0, 4096),
    title: String(fields.title || "").slice(0, 512),
    tabId: Number.isInteger(fields.tabId) ? fields.tabId : null,
    windowId: Number.isInteger(fields.windowId) ? fields.windowId : null
  };
  if (fields.selector) action.selector = String(fields.selector).slice(0, 1024);
  if (fields.text !== undefined) action.text = String(fields.text).slice(0, 4096);
  if (fields.key) action.key = String(fields.key).slice(0, 128);
  if (fields.label) action.label = String(fields.label).slice(0, 512);
  if (fields.tag) action.tag = String(fields.tag).slice(0, 64);
  return action;
}

function upsertRecordedAction(actions, action) {
  const previous = actions[actions.length - 1];
  if (
    previous &&
    action.kind === "fill" &&
    previous.kind === "fill" &&
    previous.selector === action.selector
  ) {
    actions[actions.length - 1] = { ...previous, ...action, id: previous.id };
    return;
  }
  if (
    previous &&
    action.kind === "navigate" &&
    previous.kind === "navigate" &&
    previous.url === action.url
  ) {
    return;
  }
  actions.push(action);
}

function renderRecordedPlaywright(actions) {
  const lines = [
    "import { test, expect } from '@playwright/test';",
    "",
    "test('rbc recording', async ({ page }) => {"
  ];
  let previousTabId = null;
  for (const action of actions) {
    if (action.tabId && action.tabId !== previousTabId) {
      lines.push(`  // Tab ${action.tabId}${action.title ? `: ${action.title}` : ""}`);
      previousTabId = action.tabId;
    }
    if (action.kind === "navigate" && action.url) {
      lines.push(`  await page.goto(${JSON.stringify(action.url)});`);
    } else if (action.kind === "click" && action.selector) {
      lines.push(`  await page.locator(${JSON.stringify(action.selector)}).click();`);
    } else if (action.kind === "fill" && action.selector) {
      lines.push(`  await page.locator(${JSON.stringify(action.selector)}).fill(${JSON.stringify(action.text || "")});`);
    } else if (action.kind === "press" && action.key) {
      if (action.selector) {
        lines.push(`  await page.locator(${JSON.stringify(action.selector)}).press(${JSON.stringify(action.key)});`);
      } else {
        lines.push(`  await page.locator("body").press(${JSON.stringify(action.key)});`);
      }
    }
  }
  lines.push("});");
  return lines.join("\n");
}

function normalizePlaywrightCrxCode(code) {
  const source = String(code || "").trim();
  if (!source) {
    return source;
  }
  if (/\btest\s*\(/.test(source)) {
    return source;
  }

  const moduleMatch = source.match(
    /^module\.exports\s*=\s*async\s*\(\s*\{\s*page\s*\}\s*\)\s*=>\s*\{([\s\S]*)\}\s*;?\s*$/
  );
  const body = moduleMatch ? moduleMatch[1].trim() : source;
  return [
    "import { test, expect } from '@playwright/test';",
    "",
    "test('rbc', async ({ page }) => {",
    indentPlaywrightBody(body),
    "});"
  ].join("\n");
}

function indentPlaywrightBody(body) {
  return String(body || "")
    .split(/\r?\n/)
    .map((line) => (line.trim() ? `  ${line}` : ""))
    .join("\n");
}

function isRecordableUrl(url) {
  return /^https?:\/\//i.test(String(url || ""));
}

async function getTargetTabId(params) {
  if (Number.isInteger(params.tabId)) {
    state.activeTabId = params.tabId;
    return params.tabId;
  }

  if (Number.isInteger(state.activeTabId)) {
    try {
      await chromeCall(chrome.tabs.get, chrome.tabs, state.activeTabId);
      return state.activeTabId;
    } catch (_error) {
      state.activeTabId = null;
    }
  }

  const activeTabs = await chromeCall(chrome.tabs.query, chrome.tabs, {
    active: true,
    lastFocusedWindow: true
  });

  if (activeTabs.length > 0 && Number.isInteger(activeTabs[0].id)) {
    state.activeTabId = activeTabs[0].id;
    return activeTabs[0].id;
  }

  const tabs = await chromeCall(chrome.tabs.query, chrome.tabs, { active: true });
  if (tabs.length > 0 && Number.isInteger(tabs[0].id)) {
    state.activeTabId = tabs[0].id;
    return tabs[0].id;
  }

  throw new Error("No active tab found");
}

async function currentTabSummary(tabId) {
  const tab = await chromeCall(chrome.tabs.get, chrome.tabs, tabId);
  return tabSummary(tab);
}

function windowSummary(window) {
  return {
    id: window.id,
    focused: window.focused,
    incognito: window.incognito,
    profile: window.incognito ? "incognito" : "regular",
    type: window.type || "",
    state: window.state || "",
    top: window.top,
    left: window.left,
    width: window.width,
    height: window.height,
    tabs: (window.tabs || []).map((tab) => tabSummary(tab, window))
  };
}

function tabSummary(tab, window) {
  return {
    id: tab.id,
    windowId: tab.windowId,
    active: tab.active,
    index: tab.index,
    incognito: tab.incognito || window?.incognito || false,
    profile: tab.incognito || window?.incognito ? "incognito" : "regular",
    pinned: tab.pinned || false,
    audible: tab.audible || false,
    discarded: tab.discarded || false,
    title: tab.title || "",
    url: tab.url || "",
    status: tab.status || ""
  };
}

async function evaluateInPage(tabId, expression) {
  await sendCdp(tabId, "Runtime.enable", {});
  const result = await sendCdp(tabId, "Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
    userGesture: true
  });

  if (result.exceptionDetails) {
    throw new Error(formatException(result.exceptionDetails));
  }

  return unwrapRemoteObject(result.result);
}

async function runFunctionInPage(tabId, fn, args) {
  const expression = `(${fn.toString()})(${args.map((arg) => JSON.stringify(arg)).join(",")})`;
  return evaluateInPage(tabId, expression);
}

async function ensureDebugger(tabId) {
  if (attachedTabs.has(tabId)) {
    return;
  }

  try {
    await chromeCall(chrome.debugger.attach, chrome.debugger, { tabId }, "1.3");
    attachedTabs.add(tabId);
  } catch (error) {
    if (String(errorMessage(error)).includes("Another debugger is already attached")) {
      attachedTabs.add(tabId);
      return;
    }
    throw error;
  }
}

async function detachAllDebuggers() {
  const tabIds = Array.from(attachedTabs);
  attachedTabs.clear();

  await Promise.all(
    tabIds.map(async (tabId) => {
      try {
        await chromeCall(chrome.debugger.detach, chrome.debugger, { tabId });
      } catch (_error) {
        // The tab may have closed or detached already.
      }
    })
  );
}

async function sendCdp(tabId, method, params) {
  await ensureDebugger(tabId);
  return chromeCall(chrome.debugger.sendCommand, chrome.debugger, { tabId }, method, params || {});
}

async function sendKeyEvent(tabId, key, type) {
  await sendCdp(tabId, "Input.dispatchKeyEvent", {
    type,
    key: key.key,
    code: key.code,
    windowsVirtualKeyCode: key.keyCode,
    nativeVirtualKeyCode: key.keyCode,
    text: type === "char" ? key.text : "",
    unmodifiedText: type === "char" ? key.text : "",
    modifiers: 0
  });
}

function createSnapshot(maxTextLength, maxItems) {
  function trim(value, length) {
    return String(value || "").replace(/\s+/g, " ").trim().slice(0, length);
  }

  function cssPath(element) {
    if (!element || element.nodeType !== Node.ELEMENT_NODE) {
      return "";
    }

    if (element.id) {
      return `#${CSS.escape(element.id)}`;
    }

    const parts = [];
    let current = element;

    while (current && current.nodeType === Node.ELEMENT_NODE && parts.length < 5) {
      let part = current.tagName.toLowerCase();
      if (current.classList.length > 0) {
        part += `.${CSS.escape(current.classList[0])}`;
      }

      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((child) => child.tagName === current.tagName);
        if (siblings.length > 1) {
          part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
        }
      }

      parts.unshift(part);
      current = parent;
    }

    return parts.join(" > ");
  }

  function describe(element) {
    return {
      selector: cssPath(element),
      tag: element.tagName.toLowerCase(),
      role: element.getAttribute("role") || "",
      label: trim(
        element.getAttribute("aria-label") ||
          element.getAttribute("alt") ||
          element.getAttribute("title") ||
          element.value ||
          element.innerText,
        300
      ),
      href: element.href || "",
      type: element.getAttribute("type") || ""
    };
  }

  const clickableSelector = "a,button,input,textarea,select,[role='button'],[contenteditable='true']";
  const elements = Array.from(document.querySelectorAll(clickableSelector))
    .slice(0, maxItems)
    .map(describe);

  return {
    url: location.href,
    title: document.title,
    text: trim(document.body ? document.body.innerText : "", maxTextLength),
    activeElement: cssPath(document.activeElement),
    elements
  };
}

function clickSelector(selector) {
  const element = document.querySelector(selector);
  if (!element) {
    throw new Error(`No element matches selector: ${selector}`);
  }

  element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
  const rect = element.getBoundingClientRect();
  const x = rect.left + rect.width / 2;
  const y = rect.top + rect.height / 2;

  element.dispatchEvent(new MouseEvent("mouseover", { bubbles: true, clientX: x, clientY: y }));
  element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: x, clientY: y }));
  element.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, clientX: x, clientY: y }));
  element.click();

  return {
    selector,
    text: String(element.innerText || element.value || "").trim().slice(0, 300),
    rect: {
      x: Math.round(rect.x),
      y: Math.round(rect.y),
      width: Math.round(rect.width),
      height: Math.round(rect.height)
    }
  };
}

function typeText(selector, text, replace) {
  const element = selector ? document.querySelector(selector) : document.activeElement;
  if (!element) {
    throw new Error(selector ? `No element matches selector: ${selector}` : "No active element");
  }

  element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
  element.focus();

  if (element.isContentEditable) {
    if (replace) {
      element.innerText = "";
    }
    document.execCommand("insertText", false, text);
  } else if ("value" in element) {
    if (replace) {
      element.value = text;
    } else {
      const start = Number.isInteger(element.selectionStart) ? element.selectionStart : element.value.length;
      const end = Number.isInteger(element.selectionEnd) ? element.selectionEnd : element.value.length;
      element.value = `${element.value.slice(0, start)}${text}${element.value.slice(end)}`;
      const cursor = start + text.length;
      if (element.setSelectionRange) {
        element.setSelectionRange(cursor, cursor);
      }
    }
    element.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
  } else {
    throw new Error("Target element is not editable");
  }

  return {
    selector: selector || "",
    tag: element.tagName.toLowerCase(),
    textLength: text.length
  };
}

function focusSelector(selector) {
  const element = document.querySelector(selector);
  if (!element) {
    throw new Error(`No element matches selector: ${selector}`);
  }

  element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
  element.focus();
  return {
    selector,
    tag: element.tagName.toLowerCase()
  };
}

function normalizeKey(input) {
  const aliases = {
    escape: ["Escape", "Escape", 27],
    esc: ["Escape", "Escape", 27],
    enter: ["Enter", "Enter", 13],
    tab: ["Tab", "Tab", 9],
    backspace: ["Backspace", "Backspace", 8],
    delete: ["Delete", "Delete", 46],
    arrowup: ["ArrowUp", "ArrowUp", 38],
    arrowdown: ["ArrowDown", "ArrowDown", 40],
    arrowleft: ["ArrowLeft", "ArrowLeft", 37],
    arrowright: ["ArrowRight", "ArrowRight", 39],
    home: ["Home", "Home", 36],
    end: ["End", "End", 35],
    pageup: ["PageUp", "PageUp", 33],
    pagedown: ["PageDown", "PageDown", 34],
    space: [" ", "Space", 32]
  };

  const lower = input.toLowerCase();
  if (aliases[lower]) {
    const [key, code, keyCode] = aliases[lower];
    return { key, code, keyCode, text: key.length === 1 ? key : "" };
  }

  if (input.length === 1) {
    const keyCode = input.toUpperCase().charCodeAt(0);
    return { key: input, code: `Key${input.toUpperCase()}`, keyCode, text: input };
  }

  return { key: input, code: input, keyCode: 0, text: "" };
}

function unwrapRemoteObject(remoteObject) {
  if (!remoteObject) {
    return null;
  }
  if (Object.prototype.hasOwnProperty.call(remoteObject, "value")) {
    return remoteObject.value;
  }
  if (remoteObject.unserializableValue) {
    return remoteObject.unserializableValue;
  }
  return remoteObject.description || null;
}

function formatException(exceptionDetails) {
  if (exceptionDetails.exception) {
    return unwrapRemoteObject(exceptionDetails.exception) || exceptionDetails.exception.description || "Evaluation failed";
  }
  return exceptionDetails.text || "Evaluation failed";
}

function sendSocket(message) {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    return false;
  }
  socket.send(JSON.stringify(message));
  return true;
}

function buildShareUrl(relayUrl, sessionId, agentToken) {
  const url = new URL(relayUrl);
  const prefix = url.pathname.replace(/\/+$/, "");
  url.pathname = `${prefix}/share/${encodeURIComponent(sessionId)}`;
  url.search = "";
  url.hash = `agent=${encodeURIComponent(agentToken)}`;
  return url.href;
}

function buildBrowserWsUrl(relayUrl, sessionId, browserToken, agentToken) {
  const url = new URL(relayUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  const prefix = url.pathname.replace(/\/+$/, "");
  url.pathname = `${prefix}/ws/browser/${encodeURIComponent(sessionId)}`;
  url.search = `token=${encodeURIComponent(browserToken)}&agent_token=${encodeURIComponent(agentToken)}`;
  url.hash = "";
  return url.href;
}

function normalizeRelayUrl(input) {
  const raw = String(input || "").trim();
  if (!raw) {
    throw new Error("Relay URL is required");
  }

  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(raw) ? raw : `https://${raw}`;
  const url = new URL(withScheme);

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Relay URL must use http or https");
  }

  url.search = "";
  url.hash = "";
  url.pathname = url.pathname.replace(/\/+$/, "");
  return url.href.replace(/\/$/, "");
}

function stringParam(value) {
  if (typeof value !== "string") {
    return "";
  }
  return value.trim().slice(0, 512);
}

function randomHex(lengthBytes) {
  const bytes = new Uint8Array(lengthBytes);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function clampNumber(value, min, max) {
  const number = Number(value);
  if (!Number.isFinite(number)) {
    return min;
  }
  return Math.min(Math.max(number, min), max);
}

function chromeCall(fn, context, ...args) {
  return new Promise((resolve, reject) => {
    try {
      fn.call(context, ...args, (result) => {
        const error = chrome.runtime.lastError;
        if (error) {
          reject(new Error(error.message));
        } else {
          resolve(result);
        }
      });
    } catch (error) {
      reject(error);
    }
  });
}

function reportError(error) {
  persistState({ lastError: errorMessage(error), status: "error" }).catch(() => {
    // Avoid recursive error reporting.
  });
}

function errorMessage(error) {
  if (!error) {
    return "Unknown error";
  }
  if (error.message) {
    return error.message;
  }
  return String(error);
}
