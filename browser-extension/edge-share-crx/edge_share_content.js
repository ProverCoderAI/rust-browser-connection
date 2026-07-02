"use strict";

const SOURCE_PAGE = "browser-connection:page";
const SOURCE_CONTENT = "browser-connection:content";

injectProvider();

window.addEventListener("message", (event) => {
  if (event.source !== window || event.origin !== window.location.origin) {
    return;
  }
  const message = event.data;
  if (!message || message.source !== SOURCE_PAGE || !message.id) {
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
