# Browser Connection Edge Share

Static Microsoft Edge extension skeleton for sharing a running Edge browser with
`browser-connection` through a relay link. This directory intentionally has no
build step and no dependencies.

## Install for development

1. Open `edge://extensions`.
2. Enable `Developer mode`.
3. Choose `Load unpacked`.
4. Select this `extension/edge-share` directory.
5. Open the extension popup, enter the relay URL, and click `Share`.

The popup generates a one-time browser session and a share link such as:

```text
https://relay.example.com/share/<session_id>#agent=<agent_token>
```

The extension connects as the browser side to:

```text
wss://relay.example.com/ws/browser/<session_id>?token=<browser_token>&agent_token=<agent_token>
```

For local development, `http://127.0.0.1:8765` becomes
`ws://127.0.0.1:8765/ws/browser/...`.

## Relay protocol

On WebSocket open the extension sends:

```json
{
  "type": "hello",
  "role": "browser",
  "protocolVersion": 1,
  "sessionId": "<session_id>",
  "userAgent": "<navigator.userAgent>"
}
```

The relay forwards command messages from the agent:

```json
{
  "type": "command",
  "id": "request-1",
  "command": "navigate",
  "params": {
    "url": "https://example.com"
  }
}
```

The extension replies:

```json
{
  "type": "response",
  "id": "request-1",
  "ok": true,
  "result": {}
}
```

Failed commands return `ok: false` and an `error` string.

## Commands

All commands accept optional `params.tabId`. If omitted, the extension uses the
last selected tab or the active tab in the last focused window.

- `navigate`: requires `url`; uses `chrome.tabs.update`.
- `evaluate`: requires `expression`; runs JavaScript with `Runtime.evaluate`.
- `snapshot`: returns title, URL, visible text, active element, and common
  interactive elements.
- `click`: requires `selector`; scrolls, dispatches mouse events, and calls
  `click()`.
- `type`: requires `text`; uses `selector` when provided, otherwise the active
  element. Set `replace: true` to replace existing input text.
- `press_key`: requires `key`; dispatches common keyboard events through CDP.
- `screenshot`: returns a base64 PNG by default, or JPEG with
  `format: "jpeg"`.
- `list_tabs`: returns basic tab metadata.
- `activate_tab`: activates `tabId` or the current target tab.

## Notes and limits

- This extension is the browser side for `browser-connection-relay` and the
  `browser-connection` shared-extension driver.
- `chrome.debugger` exposes useful CDP commands but is not identical to a full
  remote debugging port.
- Pages such as `edge://`, extension pages, store pages, and policy-restricted
  tabs may reject debugger, scripting, or screenshot actions.
- The share link should be treated as a bearer credential. Anyone with the link
  can control the shared session until the user clicks `Stop` or the relay
  expires the session.
- The extension uses broad host permissions so it can connect to arbitrary relay
  hosts during development. A production build should narrow this to known relay
  origins.
