"use strict";

const SOURCE_PAGE = "browser-connection:page";
const SOURCE_CONTENT = "browser-connection:content";
const SOURCE_RECORDER = "browser-connection:recorder";
const RECORDER_OVERLAY_ID = "browser-connection-recorder-overlay";
const RECORDER_OVERLAY_STORAGE_KEY = "browserConnectionRecorderOverlay";

let currentRecorderState = {
  recording: false,
  mode: "record"
};
let lastInspectSentAt = 0;
let lastInspectSelector = "";

injectProvider();
installRecorder();
installRecorderOverlay();

window.addEventListener("message", (event) => {
  if (event.source !== window || event.origin !== window.location.origin) {
    return;
  }
  const message = event.data;
  if (!message || message.source !== SOURCE_PAGE || !message.id) {
    if (message && message.source === SOURCE_RECORDER && message.kind === "navigate") {
      recordAction({ kind: "navigate" });
    }
    return;
  }

  chrome.runtime.sendMessage(
    {
      type: "platform_request",
      id: message.id,
      method: message.method,
      params: message.params || {},
      origin: window.location.origin,
      href: window.location.href,
      title: document.title || ""
    },
    (response) => {
      const runtimeError = chrome.runtime.lastError;
      if (runtimeError) {
        postResponse(message.id, false, null, runtimeError.message);
        return;
      }
      if (!response || response.ok !== true) {
        postResponse(
          message.id,
          false,
          null,
          (response && response.error) || "Extension request failed"
        );
        return;
      }
      postResponse(message.id, true, response.result, "");
    }
  );
});

function injectProvider() {
  const script = document.createElement("script");
  script.src = chrome.runtime.getURL("provider.js");
  script.async = false;
  script.onload = () => script.remove();
  (document.documentElement || document.head || document.body).appendChild(script);
}

function postResponse(id, ok, result, error) {
  window.postMessage(
    {
      source: SOURCE_CONTENT,
      id,
      ok,
      result,
      error
    },
    window.location.origin
  );
}

function installRecorder() {
  const pendingInputs = new Map();

  document.addEventListener("mousemove", (event) => {
    if (isRecorderOverlayEvent(event) || !isInspectMode()) return;
    inspectEventTarget(event, false);
  }, true);

  document.addEventListener("click", (event) => {
    if (isRecorderOverlayEvent(event)) return;
    if (isInspectMode()) {
      inspectEventTarget(event, true);
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    const target = closestRecordableElement(event.target);
    if (!target) return;
    recordAction({
      kind: "click",
      selector: bestSelector(target),
      label: elementLabel(target),
      tag: target.tagName.toLowerCase()
    });
  }, true);

  document.addEventListener("input", (event) => {
    if (isRecorderOverlayEvent(event) || isInspectMode()) return;
    const target = closestRecordableElement(event.target);
    if (!target || !isEditable(target)) return;
    const selector = bestSelector(target);
    clearTimeout(pendingInputs.get(selector));
    pendingInputs.set(selector, setTimeout(() => {
      pendingInputs.delete(selector);
      recordFill(target);
    }, 350));
  }, true);

  document.addEventListener("change", (event) => {
    if (isRecorderOverlayEvent(event) || isInspectMode()) return;
    const target = closestRecordableElement(event.target);
    if (!target || !isEditable(target)) return;
    recordFill(target);
  }, true);

  document.addEventListener("keydown", (event) => {
    if (isRecorderOverlayEvent(event)) return;
    if (isInspectMode()) {
      if (event.key === "Escape") {
        hideInspectorHighlight();
        sendRuntimeMessage({ type: "set_recording_mode", mode: "record" }).catch(() => {});
      }
      return;
    }
    if (!shouldRecordKey(event)) return;
    const target = closestRecordableElement(event.target);
    recordAction({
      kind: "press",
      selector: target ? bestSelector(target) : "",
      key: keyName(event),
      label: target ? elementLabel(target) : "",
      tag: target ? target.tagName.toLowerCase() : ""
    });
  }, true);

  window.addEventListener("pageshow", () => {
    recordAction({ kind: "navigate" });
  });
}

function isInspectMode() {
  return !!currentRecorderState.recording && currentRecorderState.mode === "inspect";
}

function inspectEventTarget(event, commit) {
  const target = closestRecordableElement(event.target) || elementFromEvent(event);
  if (!target) return;
  showInspectorHighlight(target);
  sendInspectTarget(target, commit);
}

function elementFromEvent(event) {
  if (event.target && event.target.nodeType === Node.ELEMENT_NODE) {
    return event.target;
  }
  return null;
}

function sendInspectTarget(element, force) {
  const selector = bestSelector(element);
  const now = Date.now();
  if (!force && selector === lastInspectSelector && now - lastInspectSentAt < 400) {
    return;
  }
  if (!force && now - lastInspectSentAt < 160) {
    return;
  }

  lastInspectSentAt = now;
  lastInspectSelector = selector;
  chrome.runtime.sendMessage({
    type: "inspect_target",
    target: {
      selector,
      label: elementLabel(element),
      tag: element.tagName.toLowerCase(),
      url: location.href,
      title: document.title || ""
    }
  }, () => {
    void chrome.runtime.lastError;
  });
}

function showInspectorHighlight(element) {
  const rect = element.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return;

  let highlight = document.getElementById(`${RECORDER_OVERLAY_ID}-highlight`);
  if (!highlight) {
    highlight = document.createElement("div");
    highlight.id = `${RECORDER_OVERLAY_ID}-highlight`;
    highlight.style.position = "fixed";
    highlight.style.zIndex = "2147483646";
    highlight.style.pointerEvents = "none";
    highlight.style.border = "2px solid #35b184";
    highlight.style.borderRadius = "6px";
    highlight.style.boxShadow = "0 0 0 4px rgba(53, 177, 132, 0.22)";
    highlight.style.transition = "left 80ms ease, top 80ms ease, width 80ms ease, height 80ms ease";
    document.documentElement.appendChild(highlight);
  }

  highlight.style.left = `${Math.max(0, rect.left)}px`;
  highlight.style.top = `${Math.max(0, rect.top)}px`;
  highlight.style.width = `${rect.width}px`;
  highlight.style.height = `${rect.height}px`;
  highlight.style.display = "block";
}

function hideInspectorHighlight() {
  const highlight = document.getElementById(`${RECORDER_OVERLAY_ID}-highlight`);
  if (highlight) {
    highlight.style.display = "none";
  }
}

function installRecorderOverlay() {
  if (!canInstallRecorderOverlay()) {
    return;
  }

  const overlay = {
    host: null,
    shadow: null,
    state: {
      recording: false,
      count: 0,
      script: ""
    },
    prefs: {
      left: null,
      top: null,
      minimized: false
    },
    dragging: null
  };

  const init = () => {
    if (document.getElementById(RECORDER_OVERLAY_ID)) {
      return;
    }
    readOverlayPrefs()
      .then((prefs) => {
        overlay.prefs = { ...overlay.prefs, ...prefs };
        createRecorderOverlay(overlay);
        refreshRecorderOverlay(overlay);
        setInterval(() => refreshRecorderOverlay(overlay), 3000);
      })
      .catch(() => {
        createRecorderOverlay(overlay);
        refreshRecorderOverlay(overlay);
      });
  };

  if (document.documentElement) {
    init();
  } else {
    document.addEventListener("DOMContentLoaded", init, { once: true });
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (!message || message.type !== "recording_state_changed") {
      return;
    }
    renderRecorderOverlay(overlay, message.recording || {});
  });
}

function canInstallRecorderOverlay() {
  if (!/^https?:\/\//i.test(location.href)) {
    return false;
  }
  try {
    return window.top === window;
  } catch (_error) {
    return false;
  }
}

function createRecorderOverlay(overlay) {
  const host = document.createElement("div");
  host.id = RECORDER_OVERLAY_ID;
  host.style.all = "initial";
  host.style.position = "fixed";
  host.style.zIndex = "2147483647";
  host.style.display = "none";
  applyOverlayPosition(host, overlay.prefs);

  const shadow = host.attachShadow({ mode: "closed" });
  shadow.innerHTML = `
    <style>
      :host {
        color-scheme: dark;
      }

      * {
        box-sizing: border-box;
      }

      .panel,
      .pill {
        width: 344px;
        color: #e6edf3;
        border: 1px solid #3a4656;
        border-radius: 10px;
        background: #10151d;
        box-shadow: 0 16px 50px rgba(0, 0, 0, 0.35);
        font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        font-size: 13px;
        line-height: 1.35;
      }

      .panel {
        overflow: hidden;
      }

      .bar {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 10px;
        padding: 9px 10px;
        border-bottom: 1px solid #2b3543;
        cursor: grab;
        user-select: none;
      }

      .bar:active {
        cursor: grabbing;
      }

      .status {
        display: flex;
        min-width: 0;
        align-items: center;
        gap: 8px;
        font-weight: 700;
      }

      .dot {
        width: 9px;
        height: 9px;
        flex: 0 0 auto;
        border-radius: 999px;
        background: #f85149;
        box-shadow: 0 0 0 4px rgba(248, 81, 73, 0.18);
      }

      .count {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .icon-button,
      button {
        border: 1px solid #3a4656;
        border-radius: 7px;
        color: #e6edf3;
        background: #1a2230;
        font: inherit;
        font-weight: 700;
        cursor: pointer;
      }

      .icon-button {
        width: 28px;
        height: 28px;
        padding: 0;
      }

      .icon-button:hover,
      button:hover {
        background: #243044;
      }

      .body {
        display: grid;
        gap: 10px;
        padding: 10px;
      }

      .hint {
        margin: 0;
        color: #b8c7dc;
      }

      .mode-switch {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 6px;
        padding: 3px;
        border: 1px solid #2b3543;
        border-radius: 8px;
        background: #0b1018;
      }

      .mode-button {
        min-height: 30px;
        border-color: transparent;
        background: transparent;
      }

      .mode-button.active {
        border-color: #35b184;
        color: #d9fff0;
        background: rgba(53, 177, 132, 0.2);
      }

      .inspect-target,
      .steps {
        display: grid;
        gap: 6px;
        max-height: 132px;
        overflow: auto;
        padding: 8px;
        border: 1px solid #2b3543;
        border-radius: 8px;
        background: #0b1018;
      }

      .inspect-target {
        display: none;
      }

      .inspect-target.visible {
        display: grid;
      }

      .label {
        color: #8ea2bd;
        font-size: 11px;
        font-weight: 800;
        text-transform: uppercase;
      }

      .selector,
      .empty,
      .step-selector {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      .selector {
        color: #f8fafc;
        font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
        font-size: 12px;
      }

      .empty {
        color: #8ea2bd;
      }

      .step {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        gap: 8px;
        align-items: center;
        min-width: 0;
      }

      .step-kind {
        min-width: 54px;
        color: #d9fff0;
        font-size: 11px;
        font-weight: 800;
        text-transform: uppercase;
      }

      .step-selector {
        color: #c8d7eb;
        font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
        font-size: 11px;
      }

      .step.playing .step-selector {
        color: #ffffff;
      }

      .actions {
        display: grid;
        grid-template-columns: 1fr 1fr 1fr 1fr;
        gap: 8px;
      }

      button {
        min-height: 32px;
        padding: 6px 8px;
      }

      button.primary {
        border-color: #ef4444;
        background: #dc2626;
      }

      button.primary:hover {
        background: #b91c1c;
      }

      button.copied {
        border-color: #35b184;
        color: #d9fff0;
      }

      button:disabled {
        cursor: not-allowed;
        opacity: 0.55;
      }

      .pill {
        width: auto;
        min-width: 126px;
        display: none;
        align-items: center;
        gap: 8px;
        padding: 8px 10px;
        cursor: pointer;
        user-select: none;
      }

      .pill strong {
        white-space: nowrap;
      }

      :host(.is-minimized) .panel {
        display: none;
      }

      :host(.is-minimized) .pill {
        display: flex;
      }
    </style>
    <section class="panel" aria-label="Browser action recorder">
      <div class="bar" id="dragHandle">
        <div class="status">
          <span class="dot"></span>
          <span class="count" id="statusText">Recording 0 actions</span>
        </div>
        <button class="icon-button" id="minimizeButton" type="button" title="Minimize">-</button>
      </div>
      <div class="body">
        <div class="mode-switch" aria-label="Recorder mode">
          <button class="mode-button active" id="recordModeButton" type="button">Record</button>
          <button class="mode-button" id="inspectModeButton" type="button">Inspect</button>
        </div>
        <p class="hint" id="hintText">User actions are being recorded for the agent.</p>
        <div class="inspect-target" id="inspectTarget">
          <div class="label">Selector</div>
          <div class="selector" id="inspectSelector">Hover an element</div>
        </div>
        <div class="steps" id="stepsList">
          <div class="empty">No recorded steps yet.</div>
        </div>
        <div class="actions">
          <button id="playButton" type="button">Play</button>
          <button class="primary" id="stopButton" type="button">Stop</button>
          <button id="clearButton" type="button">Clear</button>
          <button id="copyButton" type="button">Copy</button>
        </div>
      </div>
    </section>
    <button class="pill" id="restoreButton" type="button" title="Restore recorder">
      <span class="dot"></span>
      <strong id="pillText">REC 0</strong>
    </button>
  `;

  overlay.host = host;
  overlay.shadow = shadow;
  document.documentElement.appendChild(host);

  shadow.getElementById("recordModeButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    hideInspectorHighlight();
    sendRuntimeMessage({ type: "set_recording_mode", mode: "record" })
      .then((recording) => renderRecorderOverlay(overlay, recording))
      .catch(() => refreshRecorderOverlay(overlay));
  });

  shadow.getElementById("inspectModeButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    sendRuntimeMessage({ type: "set_recording_mode", mode: "inspect" })
      .then((recording) => renderRecorderOverlay(overlay, recording))
      .catch(() => refreshRecorderOverlay(overlay));
  });

  shadow.getElementById("playButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    hideInspectorHighlight();
    sendRuntimeMessage({ type: "play_recording" })
      .then((recording) => renderRecorderOverlay(overlay, recording))
      .catch(() => refreshRecorderOverlay(overlay));
  });

  shadow.getElementById("stopButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    hideInspectorHighlight();
    sendRuntimeMessage({ type: "stop_recording" })
      .then((recording) => renderRecorderOverlay(overlay, recording))
      .catch(() => refreshRecorderOverlay(overlay));
  });

  shadow.getElementById("clearButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    sendRuntimeMessage({ type: "clear_recording" })
      .then((recording) => renderRecorderOverlay(overlay, recording))
      .catch(() => refreshRecorderOverlay(overlay));
  });

  shadow.getElementById("copyButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    copyRecordedScript(overlay);
  });

  shadow.getElementById("minimizeButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    overlay.prefs.minimized = true;
    saveOverlayPrefs(overlay.prefs);
    renderRecorderOverlay(overlay, overlay.state);
  });

  shadow.getElementById("restoreButton").addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    overlay.prefs.minimized = false;
    saveOverlayPrefs(overlay.prefs);
    renderRecorderOverlay(overlay, overlay.state);
  });

  shadow.getElementById("dragHandle").addEventListener("pointerdown", (event) => {
    beginOverlayDrag(event, overlay);
  });
  shadow.getElementById("restoreButton").addEventListener("pointerdown", (event) => {
    beginOverlayDrag(event, overlay);
  });
}

function refreshRecorderOverlay(overlay) {
  if (!overlay.host) {
    return;
  }
  sendRuntimeMessage({ type: "get_recording" })
    .then((recording) => renderRecorderOverlay(overlay, recording))
    .catch(() => {
      if (overlay.host) {
        overlay.host.style.display = "none";
      }
    });
}

function renderRecorderOverlay(overlay, recording) {
  if (!overlay.host || !overlay.shadow) {
    return;
  }

  overlay.state = {
    ...overlay.state,
    ...(recording || {})
  };
  currentRecorderState = {
    recording: !!overlay.state.recording,
    mode: overlay.state.mode || "record"
  };

  const recordingActive = !!overlay.state.recording;
  overlay.host.style.display = recordingActive ? "block" : "none";
  overlay.host.classList.toggle("is-minimized", !!overlay.prefs.minimized);

  const count = Number(overlay.state.count || 0);
  const label = `${count} ${count === 1 ? "action" : "actions"}`;
  const mode = overlay.state.mode === "inspect" ? "inspect" : "record";
  const playing = !!overlay.state.playing;
  overlay.shadow.getElementById("statusText").textContent = playing
    ? `Playing ${label}`
    : mode === "inspect"
      ? `Inspecting ${label}`
      : `Recording ${label}`;
  overlay.shadow.getElementById("pillText").textContent = `REC ${count}`;
  overlay.shadow.getElementById("hintText").textContent = mode === "inspect"
    ? "Hover elements to preview selectors. Click captures the selector without activating the page."
    : "User actions are being recorded for the agent.";

  overlay.shadow.getElementById("recordModeButton").classList.toggle("active", mode === "record");
  overlay.shadow.getElementById("inspectModeButton").classList.toggle("active", mode === "inspect");
  overlay.shadow.getElementById("playButton").disabled = playing || count === 0;
  overlay.shadow.getElementById("clearButton").disabled = playing || count === 0;
  overlay.shadow.getElementById("copyButton").disabled = !String(overlay.state.script || "").trim();

  const inspectTarget = overlay.shadow.getElementById("inspectTarget");
  const inspect = overlay.state.lastInspect || null;
  inspectTarget.classList.toggle("visible", mode === "inspect");
  overlay.shadow.getElementById("inspectSelector").textContent = inspect?.selector || "Hover an element";
  renderOverlaySteps(overlay);
  if (mode !== "inspect") {
    hideInspectorHighlight();
  }
}

function renderOverlaySteps(overlay) {
  const steps = Array.isArray(overlay.state.actions) ? overlay.state.actions : [];
  const list = overlay.shadow.getElementById("stepsList");
  const visible = steps.slice(-6).reverse();
  if (visible.length === 0) {
    list.replaceChildren(createShadowElement(overlay.shadow, "div", "empty", "No recorded steps yet."));
    return;
  }

  list.replaceChildren(...visible.map((action) => {
    const row = createShadowElement(overlay.shadow, "div", `step${action.id && action.id === overlay.state.playbackStepId ? " playing" : ""}`, "");
    row.appendChild(createShadowElement(overlay.shadow, "span", "step-kind", action.kind || "step"));
    row.appendChild(createShadowElement(overlay.shadow, "span", "step-selector", recordedActionLabel(action)));
    return row;
  }));
}

function createShadowElement(shadow, tag, className, text) {
  const element = document.createElement(tag);
  if (className) {
    element.className = className;
  }
  element.textContent = text;
  return element;
}

function recordedActionLabel(action) {
  if (action.selector) return action.selector;
  if (action.url) return action.url;
  if (action.key) return action.key;
  return action.title || "-";
}

function beginOverlayDrag(event, overlay) {
  if (!overlay.host || event.button !== 0) {
    return;
  }

  event.preventDefault();
  event.stopPropagation();

  const rect = overlay.host.getBoundingClientRect();
  overlay.dragging = {
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    left: rect.left,
    top: rect.top
  };

  const move = (moveEvent) => moveOverlay(moveEvent, overlay);
  const up = (upEvent) => {
    if (!overlay.dragging || upEvent.pointerId !== overlay.dragging.pointerId) {
      return;
    }
    document.removeEventListener("pointermove", move, true);
    document.removeEventListener("pointerup", up, true);
    overlay.dragging = null;
    saveOverlayPrefs(overlay.prefs);
  };

  document.addEventListener("pointermove", move, true);
  document.addEventListener("pointerup", up, true);
}

function moveOverlay(event, overlay) {
  if (!overlay.host || !overlay.dragging || event.pointerId !== overlay.dragging.pointerId) {
    return;
  }

  event.preventDefault();
  event.stopPropagation();

  const width = overlay.host.offsetWidth || 300;
  const height = overlay.host.offsetHeight || 120;
  const left = clampNumber(
    overlay.dragging.left + event.clientX - overlay.dragging.startX,
    8,
    Math.max(8, window.innerWidth - width - 8)
  );
  const top = clampNumber(
    overlay.dragging.top + event.clientY - overlay.dragging.startY,
    8,
    Math.max(8, window.innerHeight - height - 8)
  );

  overlay.prefs.left = Math.round(left);
  overlay.prefs.top = Math.round(top);
  applyOverlayPosition(overlay.host, overlay.prefs);
}

function applyOverlayPosition(host, prefs) {
  if (Number.isFinite(prefs.left) && Number.isFinite(prefs.top)) {
    host.style.left = `${prefs.left}px`;
    host.style.top = `${prefs.top}px`;
    host.style.right = "auto";
    host.style.bottom = "auto";
    return;
  }
  host.style.left = "auto";
  host.style.top = "auto";
  host.style.right = "16px";
  host.style.bottom = "16px";
}

function readOverlayPrefs() {
  return new Promise((resolve) => {
    chrome.storage.local.get(RECORDER_OVERLAY_STORAGE_KEY, (items) => {
      const prefs = items && items[RECORDER_OVERLAY_STORAGE_KEY];
      resolve(prefs && typeof prefs === "object" ? prefs : {});
    });
  });
}

function saveOverlayPrefs(prefs) {
  chrome.storage.local.set({
    [RECORDER_OVERLAY_STORAGE_KEY]: {
      left: Number.isFinite(prefs.left) ? prefs.left : null,
      top: Number.isFinite(prefs.top) ? prefs.top : null,
      minimized: !!prefs.minimized
    }
  });
}

function copyRecordedScript(overlay) {
  const script = String(overlay.state.script || "");
  if (!script) {
    return;
  }

  const button = overlay.shadow.getElementById("copyButton");
  copyText(script).then(() => {
    button.classList.add("copied");
    button.textContent = "Copied";
    setTimeout(() => {
      button.classList.remove("copied");
      button.textContent = "Copy";
    }, 1200);
  });
}

function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) {
    return navigator.clipboard.writeText(text);
  }

  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "true");
  textarea.style.position = "fixed";
  textarea.style.left = "-9999px";
  document.documentElement.appendChild(textarea);
  textarea.select();
  document.execCommand("copy");
  textarea.remove();
  return Promise.resolve();
}

function sendRuntimeMessage(message) {
  return new Promise((resolve, reject) => {
    chrome.runtime.sendMessage(message, (response) => {
      const runtimeError = chrome.runtime.lastError;
      if (runtimeError) {
        reject(new Error(runtimeError.message));
        return;
      }
      if (!response || response.ok !== true) {
        reject(new Error((response && response.error) || "Extension message failed"));
        return;
      }
      resolve(response.result);
    });
  });
}

function isRecorderOverlayEvent(event) {
  if (!event || typeof event.composedPath !== "function") {
    return false;
  }
  return event.composedPath().some((node) => {
    return node && node.nodeType === Node.ELEMENT_NODE && node.id === RECORDER_OVERLAY_ID;
  });
}

function clampNumber(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

function recordFill(element) {
  if (isSensitiveEditable(element)) return;
  const value = elementValue(element);
  recordAction({
    kind: "fill",
    selector: bestSelector(element),
    text: value,
    label: elementLabel(element),
    tag: element.tagName.toLowerCase()
  });
}

function recordAction(action) {
  if (!/^https?:\/\//i.test(location.href)) return;
  chrome.runtime.sendMessage({
    type: "recorder_action",
    action: {
      ...action,
      url: location.href,
      title: document.title || ""
    }
  }, () => {
    void chrome.runtime.lastError;
  });
}

function closestRecordableElement(node) {
  if (!node || node.nodeType !== Node.ELEMENT_NODE) {
    return null;
  }
  return node.closest("a,button,input,textarea,select,[role='button'],[contenteditable='true'],[data-testid],[data-test],[data-qa]");
}

function isEditable(element) {
  const tag = element.tagName.toLowerCase();
  return element.isContentEditable || tag === "input" || tag === "textarea" || tag === "select";
}

function isSensitiveEditable(element) {
  const haystack = [
    element.type,
    element.name,
    element.id,
    element.getAttribute("autocomplete"),
    element.getAttribute("aria-label"),
    element.getAttribute("placeholder")
  ].join(" ").toLowerCase();
  return /\b(password|passwd|token|secret|otp|2fa|mfa|code)\b/.test(haystack);
}

function elementValue(element) {
  if (element.isContentEditable) return element.innerText || "";
  if ("value" in element) return element.value || "";
  return "";
}

function shouldRecordKey(event) {
  if (event.isComposing) return false;
  if (event.ctrlKey || event.metaKey || event.altKey) return true;
  return [
    "Enter",
    "Escape",
    "Tab",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Backspace",
    "Delete"
  ].includes(event.key);
}

function keyName(event) {
  const parts = [];
  if (event.ctrlKey) parts.push("Control");
  if (event.metaKey) parts.push("Meta");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey && event.key !== "Shift") parts.push("Shift");
  parts.push(event.key);
  return parts.join("+");
}

function bestSelector(element) {
  const direct = stableSelector(element);
  if (direct) return direct;
  const labelled = labelledSelector(element);
  if (labelled) return labelled;
  return cssPath(element);
}

function stableSelector(element) {
  for (const name of ["data-testid", "data-test", "data-qa", "data-cy"]) {
    const value = element.getAttribute(name);
    if (value) return `[${name}="${cssString(value)}"]`;
  }
  if (element.id) return `#${cssIdent(element.id)}`;
  if (element.getAttribute("name")) {
    return `${element.tagName.toLowerCase()}[name="${cssString(element.getAttribute("name"))}"]`;
  }
  return "";
}

function labelledSelector(element) {
  const label = element.getAttribute("aria-label") || element.getAttribute("placeholder") || element.getAttribute("title");
  if (!label) return "";
  return `${element.tagName.toLowerCase()}[${element.getAttribute("aria-label") ? "aria-label" : element.getAttribute("placeholder") ? "placeholder" : "title"}="${cssString(label)}"]`;
}

function cssPath(element) {
  const parts = [];
  let current = element;
  while (current && current.nodeType === Node.ELEMENT_NODE && parts.length < 5) {
    let part = current.tagName.toLowerCase();
    if (current.classList.length > 0) {
      part += `.${cssIdent(current.classList[0])}`;
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

function elementLabel(element) {
  return String(
    element.getAttribute("aria-label") ||
    element.getAttribute("alt") ||
    element.getAttribute("title") ||
    element.getAttribute("placeholder") ||
    element.innerText ||
    element.value ||
    ""
  ).replace(/\s+/g, " ").trim().slice(0, 300);
}

function cssIdent(value) {
  if (window.CSS && typeof CSS.escape === "function") {
    return CSS.escape(String(value));
  }
  return String(value).replace(/[^a-zA-Z0-9_-]/g, "\\$&");
}

function cssString(value) {
  return String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}
