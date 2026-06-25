"use strict";

(() => {
  if (window.browserConnection) {
    return;
  }

  const SOURCE_PAGE = "browser-connection:page";
  const SOURCE_CONTENT = "browser-connection:content";
  const pending = new Map();
  let nextId = 1;

  function request(input) {
    const payload = normalizeRequest(input);
    const id = String(nextId++);

    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      window.postMessage(
        {
          source: SOURCE_PAGE,
          id,
          method: payload.method,
          params: payload.params
        },
        window.location.origin
      );
    });
  }

  function normalizeRequest(input) {
    if (!input || typeof input !== "object") {
      throw new Error("browserConnection.request expects an object");
    }
    const method = String(input.method || "").trim();
    if (!method) {
      throw new Error("browserConnection request method is required");
    }
    const params = input.params && typeof input.params === "object" ? input.params : {};
    return { method, params };
  }

  window.addEventListener("message", (event) => {
    if (event.source !== window || event.origin !== window.location.origin) {
      return;
    }
    const message = event.data;
    if (!message || message.source !== SOURCE_CONTENT || !message.id) {
      return;
    }

    const entry = pending.get(message.id);
    if (!entry) {
      return;
    }
    pending.delete(message.id);

    if (message.ok) {
      entry.resolve(message.result);
    } else {
      entry.reject(new Error(message.error || "browserConnection request failed"));
    }
  });

  Object.defineProperty(window, "browserConnection", {
    value: Object.freeze({ request }),
    enumerable: false,
    configurable: false,
    writable: false
  });

  window.dispatchEvent(new Event("browserConnection#initialized"));
})();
