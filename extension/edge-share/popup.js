"use strict";

const DEFAULT_RELAY_URL = "http://127.0.0.1:8765";

const relayUrlInput = document.getElementById("relayUrl");
const shareUrlInput = document.getElementById("shareUrl");
const shareButton = document.getElementById("shareButton");
const stopButton = document.getElementById("stopButton");
const copyButton = document.getElementById("copyButton");
const statusDot = document.getElementById("statusDot");
const statusText = document.getElementById("statusText");
const errorText = document.getElementById("errorText");

let currentState = null;

document.addEventListener("DOMContentLoaded", () => {
  shareButton.addEventListener("click", onShare);
  stopButton.addEventListener("click", onStop);
  copyButton.addEventListener("click", onCopy);

  refreshState();
  setInterval(refreshState, 1000);
});

chrome.runtime.onMessage.addListener((message) => {
  if (message && message.type === "state_changed") {
    renderState(message.state);
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

async function refreshState() {
  try {
    const response = await sendMessage({ type: "get_state" });
    renderState(response);
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

  if (currentState.lastError) {
    showError(currentState.lastError);
  } else {
    clearError();
  }
}

function statusLabel(state) {
  if (!state) {
    return "Unknown";
  }
  if (state.connected) {
    return "Connected";
  }
  if (state.sharing) {
    return state.status || "Sharing";
  }
  return state.status || "Idle";
}

function setBusy(busy) {
  shareButton.disabled = busy;
  stopButton.disabled = busy || !(currentState && currentState.sharing);
  copyButton.disabled = busy || !(currentState && currentState.shareUrl);
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
