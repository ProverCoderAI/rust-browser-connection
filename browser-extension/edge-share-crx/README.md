# Browser Connection Playwright CRX

This directory contains the installable Edge/Chrome extension built from
`ruifigueira/playwright-crx` recorder sources and integrated with the
`browser-connection` relay.

Upstream project: https://github.com/ruifigueira/playwright-crx

## What is included

- Playwright CRX recorder/player UI, side panel, shortcuts, context menu, and
  options page.
- Browser Connection provider bridge exposed to web pages as
  `window.browserConnection`.
- Browser relay WebSocket client used by the control panel and `rbc` tools.

## Install

1. Open `edge://extensions`.
2. Enable Developer mode.
3. Click `Load unpacked`.
4. Select this directory after unpacking the release archive.

## Notes

- The extension action button is reserved for Playwright CRX recorder attach,
  so Browser Connection sharing is triggered from the platform page.
- Apache-2.0 upstream attribution is preserved in
  `PLAYWRIGHT_CRX_LICENSE` and `PLAYWRIGHT_CRX_NOTICE`.
