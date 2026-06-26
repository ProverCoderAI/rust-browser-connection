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

## Connect from browser-connection control panel

Open the `browser-connection` `controlPanelUrl` in the Edge profile where this
extension is installed. The page injects `window.browserConnection`, triggers a
connect request, and this extension opens a confirmation window. After approval,
the extension connects to the control panel origin as the relay and returns a
share link to the page, which registers it in the current browser pool.

This mode does not require copying the share link or starting
`browser-connection-relay` separately for the current workspace. If the Edge
browser is on another machine, expose the control panel only through an
authenticated platform proxy or private tunnel that forwards HTTP(S) and
WebSocket upgrade to the same origin. The panel token is CSRF protection for the
UI, not public authentication.

## Manual share link

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
- `list_tabs`: returns windows with nested tabs plus a flat `tabs` list. Window
  and tab metadata includes `regular`/`incognito` when Edge exposes it.
- `activate_tab`: activates `tabId` or the current target tab.
- `start_recording`, `stop_recording`, `clear_recording`, and
  `recording_state`: control the user action recorder.
- `set_recording_mode`: accepts `mode: "record"` or `mode: "inspect"`. Inspect
  mode highlights elements and reports selectors without activating the page.
- `play_recording`: replays the currently recorded steps through the same
  shared browser debugger transport.

`rbc <browser> pw` can run a Playwright-compatible subset against share links by
translating supported `page` methods into these relay commands. Unsupported
Playwright APIs return a clear error instead of silently pretending to work.

## Notes and limits

- This extension is the browser side for `browser-connection-relay` and the
  `browser-connection` shared-extension driver.
- `chrome.debugger` exposes useful CDP commands but is not identical to a full
  remote debugging port.
- Pages such as `edge://`, extension pages, store pages, and policy-restricted
  tabs may reject debugger, scripting, or screenshot actions.
- Edge/Chrome extensions cannot read the human profile name. A loaded extension
  instance represents one browser profile; incognito windows are visible only
  when the user enables the extension in InPrivate/Incognito mode.
- The share link should be treated as a bearer credential. Anyone with the link
  can control the shared session until the user clicks `Stop` or the relay
  expires the session.
- The extension uses broad host permissions so it can connect to arbitrary relay
  hosts during development. A production build should narrow this to known relay
  origins.
