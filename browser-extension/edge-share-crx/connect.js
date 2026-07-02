"use strict";

const params = new URLSearchParams(window.location.search);
const requestId = params.get("requestId") || "";

const originEl = document.getElementById("origin");
const workspaceEl = document.getElementById("workspace");
const relayEl = document.getElementById("relay");
const statusEl = document.getElementById("status");
const approveButton = document.getElementById("approveButton");
const rejectButton = document.getElementById("rejectButton");

let request = null;

document.addEventListener("DOMContentLoaded", () => {
  approveButton.addEventListener("click", approve);
  rejectButton.addEventListener("click", reject);
  loadRequest().catch(showError);
});

async function loadRequest() {
  if (!requestId) {
    throw new Error("Missing request id");
  }

  request = await sendMessage({
    type: "get_platform_connect_request",
    requestId
  });

  originEl.textContent = request.origin || "-";
  workspaceEl.textContent = request.workspaceId || request.poolId || "current-runtime";
  relayEl.textContent = request.relayUrl || "-";
  statusEl.textContent = "Allow this page to control this browser session through the relay.";
}

async function approve() {
  setBusy(true);
  statusEl.classList.remove("error");
  statusEl.textContent = "Connecting";

  try {
    const result = await sendMessage({
      type: "approve_platform_connect",
      requestId
    });
    statusEl.textContent = result.connected
      ? "Connected"
      : "Share session created. Waiting for relay connection.";
    setTimeout(() => window.close(), 900);
  } catch (error) {
    showError(error);
    setBusy(false);
  }
}

async function reject() {
  setBusy(true);

  try {
    await sendMessage({
      type: "reject_platform_connect",
      requestId
    });
  } finally {
    window.close();
  }
}

function setBusy(busy) {
  approveButton.disabled = busy;
  rejectButton.disabled = busy;
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
  statusEl.textContent = error.message || String(error);
  statusEl.classList.add("error");
}
