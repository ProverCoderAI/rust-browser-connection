"use strict";

const DEFAULT_RELAY_URL = "http://127.0.0.1:8765";

const relayUrlInput = document.getElementById("relayUrl");
const shareUrlInput = document.getElementById("shareUrl");
const shareButton = document.getElementById("shareButton");
const stopButton = document.getElementById("stopButton");
const copyButton = document.getElementById("copyButton");
const recordButton = document.getElementById("recordButton");
const inspectButton = document.getElementById("inspectButton");
const playRecordButton = document.getElementById("playRecordButton");
const stopRecordButton = document.getElementById("stopRecordButton");
const clearRecordButton = document.getElementById("clearRecordButton");
const copyScriptButton = document.getElementById("copyScriptButton");
const recordingStatus = document.getElementById("recordingStatus");
const recordedScript = document.getElementById("recordedScript");
const statusDot = document.getElementById("statusDot");
const statusText = document.getElementById("statusText");
const errorText = document.getElementById("errorText");

let currentState = null;
let currentRecording = null;

document.addEventListener("DOMContentLoaded", () => {
  shareButton.addEventListener("click", onShare);
  stopButton.addEventListener("click", onStop);
  copyButton.addEventListener("click", onCopy);
  recordButton.addEventListener("click", onRecord);
  inspectButton.addEventListener("click", onInspect);
  playRecordButton.addEventListener("click", onPlayRecord);
  stopRecordButton.addEventListener("click", onStopRecord);
  clearRecordButton.addEventListener("click", onClearRecord);
  copyScriptButton.addEventListener("click", onCopyScript);

  refreshState();
  setInterval(refreshState, 1000);
});

chrome.runtime.onMessage.addListener((message) => {
  if (message && message.type === "state_changed") {
    renderState(message.state);
  }
  if (message && message.type === "recording_state_changed") {
    renderRecording(message.recording);
  }
});

async function onShare() {
  setBusy(true);
  clearError();

  try {
    const response = await sendMessage({
      type: "start_share",
      relayUrl: relayUrlInput.value || DEFAULT_RELAY_URL
    });
    renderState(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onStop() {
  setBusy(true);
  clearError();

  try {
    const response = await sendMessage({ type: "stop_share" });
    renderState(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onCopy() {
  clearError();

  try {
    const link = shareUrlInput.value.trim();
    if (!link) {
      throw new Error("No share link to copy");
    }

    await navigator.clipboard.writeText(link);
    copyButton.textContent = "Copied";
    setTimeout(() => {
      copyButton.textContent = "Copy";
    }, 1200);
  } catch (error) {
    showError(error);
  }
}

async function onRecord() {
  setBusy(true);
  clearError();

  try {
    const response = currentRecording && currentRecording.recording
      ? await sendMessage({ type: "set_recording_mode", mode: "record" })
      : await sendMessage({ type: "start_recording", params: { mode: "record" } });
    renderRecording(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onInspect() {
  setBusy(true);
  clearError();

  try {
    const response = currentRecording && currentRecording.recording
      ? await sendMessage({ type: "set_recording_mode", mode: "inspect" })
      : await sendMessage({ type: "start_recording", params: { mode: "inspect" } });
    renderRecording(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onPlayRecord() {
  setBusy(true);
  clearError();

  try {
    const response = await sendMessage({ type: "play_recording" });
    renderRecording(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onStopRecord() {
  setBusy(true);
  clearError();

  try {
    const response = await sendMessage({ type: "stop_recording" });
    renderRecording(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onClearRecord() {
  setBusy(true);
  clearError();

  try {
    const response = await sendMessage({ type: "clear_recording" });
    renderRecording(response);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(false);
  }
}

async function onCopyScript() {
  clearError();

  try {
    const script = recordedScript.value.trim();
    if (!script) {
      throw new Error("No recorded script to copy");
    }
    await navigator.clipboard.writeText(script);
    copyScriptButton.textContent = "Copied";
    setTimeout(() => {
      copyScriptButton.textContent = "Copy script";
    }, 1200);
  } catch (error) {
    showError(error);
  }
}

async function refreshState() {
  try {
    const [stateResponse, recordingResponse] = await Promise.all([
      sendMessage({ type: "get_state" }),
      sendMessage({ type: "get_recording" })
    ]);
    renderState(stateResponse);
    renderRecording(recordingResponse);
  } catch (error) {
    showError(error);
  }
}

function renderState(state) {
  currentState = state || {};

  relayUrlInput.value = currentState.relayUrl || relayUrlInput.value || DEFAULT_RELAY_URL;
  shareUrlInput.value = currentState.shareUrl || "";

  statusDot.className = "dot";
  if (currentState.connected) {
    statusDot.classList.add("connected");
  } else if (currentState.lastError) {
    statusDot.classList.add("error");
  }

  statusText.textContent = statusLabel(currentState);
  stopButton.disabled = !currentState.sharing;
  copyButton.disabled = !currentState.shareUrl;
  recordButton.disabled = false;
  stopRecordButton.disabled = !currentState.recording;

  if (currentState.lastError) {
    showError(currentState.lastError);
  } else {
    clearError();
  }
}

function renderRecording(recording) {
  currentRecording = recording || {};
  const count = currentRecording.count || 0;
  const mode = currentRecording.mode === "inspect" ? "inspect" : "record";
  recordingStatus.textContent = currentRecording.playing
    ? `playing ${count}`
    : currentRecording.recording ? `${mode} ${count}` : `idle ${count}`;
  recordingStatus.classList.toggle("active", !!currentRecording.recording);
  recordedScript.value = currentRecording.script || "";
  recordButton.classList.toggle("primary", mode === "record");
  inspectButton.classList.toggle("primary", mode === "inspect");
  playRecordButton.disabled = !!currentRecording.playing || count === 0;
  stopRecordButton.disabled = !currentRecording.recording;
  clearRecordButton.disabled = count === 0 && !currentRecording.recording;
  copyScriptButton.disabled = !recordedScript.value.trim();
}

function statusLabel(state) {
  if (!state) {
    return "Unknown";
  }
  if (state.connected) {
    if (state.platformOrigin) {
      return `Connected to ${originLabel(state.platformOrigin)}`;
    }
    return "Connected";
  }
  if (state.sharing) {
    return state.status || "Sharing";
  }
  return state.status || "Idle";
}

function originLabel(origin) {
  try {
    return new URL(origin).host;
  } catch (_error) {
    return origin;
  }
}

function setBusy(busy) {
  shareButton.disabled = busy;
  stopButton.disabled = busy || !(currentState && currentState.sharing);
  copyButton.disabled = busy || !(currentState && currentState.shareUrl);
  recordButton.disabled = busy;
  inspectButton.disabled = busy;
  playRecordButton.disabled = busy || !(currentRecording && currentRecording.count > 0) || !!(currentRecording && currentRecording.playing);
  stopRecordButton.disabled = busy || !(currentRecording && currentRecording.recording);
  clearRecordButton.disabled = busy || !(currentRecording && (currentRecording.count || currentRecording.recording));
  copyScriptButton.disabled = busy || !recordedScript.value.trim();
}

function sendMessage(message) {
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

function showError(error) {
  errorText.textContent = typeof error === "string" ? error : error.message || String(error);
  errorText.classList.add("visible");
}

function clearError() {
  errorText.textContent = "";
  errorText.classList.remove("visible");
}
