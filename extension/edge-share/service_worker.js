"use strict";

const PROTOCOL_VERSION = 1;
const DEFAULT_RELAY_URL = "http://127.0.0.1:8765";
const STORAGE_KEY = "edgeShareState";
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;

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
  updatedAt: ""
};

let socket = null;
let reconnectTimer = null;
let reconnectDelayMs = RECONNECT_MIN_MS;
let intentionallyClosed = false;
const attachedTabs = new Set();

chrome.runtime.onInstalled.addListener(() => {
  restoreState().then(connectIfNeeded).catch(reportError);
});

chrome.runtime.onStartup.addListener(() => {
  restoreState().then(connectIfNeeded).catch(reportError);
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  handlePopupMessage(message)
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

async function handlePopupMessage(message) {
  if (!message || typeof message.type !== "string") {
    throw new Error("Invalid message");
  }

  if (message.type === "get_state") {
    await restoreState();
    return publicState();
  }

  if (message.type === "start_share") {
    return startShare(message.relayUrl || DEFAULT_RELAY_URL);
  }

  if (message.type === "stop_share") {
    return stopShare();
  }

  throw new Error(`Unknown popup message: ${message.type}`);
}

async function startShare(relayUrlInput) {
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
    activeTabId: null
  });

  connectRelay();
  return publicState();
}

async function stopShare() {
  intentionallyClosed = true;
  clearReconnectTimer();

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
    lastError: ""
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
  const tabs = await chromeCall(chrome.tabs.query, chrome.tabs, {});
  return { tabs: tabs.map(tabSummary) };
}

async function commandActivateTab(params) {
  const tabId = await getTargetTabId(params);
  const tab = await chromeCall(chrome.tabs.update, chrome.tabs, tabId, { active: true });
  state.activeTabId = tab.id;
  await persistState({ activeTabId: tab.id });
  return { tab: tabSummary(tab) };
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

function tabSummary(tab) {
  return {
    id: tab.id,
    windowId: tab.windowId,
    active: tab.active,
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
