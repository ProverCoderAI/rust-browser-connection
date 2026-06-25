---
name: browser-cli
description: Use rbc CLI commands to inspect and automate browser sessions without MCP. Trigger this skill when Codex needs browser automation, page inspection, clicking, typing, tab/window control, screenshots, or multi-step JavaScript browser scripts while minimizing MCP token overhead.
---

# Browser CLI

Use `rbc` instead of MCP browser tools when a normal shell command can drive the active browser. Prefer CLI commands for repeatable page workflows, scripts, screenshots, and audit traces.

## Workflow

1. Resolve the target:
   - Use `rbc <project> ...` when a `browser-connection` control panel is running for the workspace.
   - Use `rbc <project> --share-url "$EDGE_SHARE_URL" ...` for a browser shared by the Edge extension.
   - Use `rbc <project> --cdp-url http://127.0.0.1:<port> ...` for a direct CDP browser.
2. Inspect first:
   - Run `rbc <project> snapshot` before choosing selectors.
   - For shared-extension targets, run `rbc <project> tabs` to see windows/tabs and `rbc <project> activate-tab <id>` before acting on a non-active tab.
3. Prefer scripts for multi-step actions:
   - Put complex DOM logic in a temporary `.js` file.
   - Run it with `rbc <project> eval --file /path/to/script.js`.
   - Return a compact JSON object from the script.
4. Log actions when transparency matters:
   - Add `--trace-dir <dir>` to append `rbc.jsonl`.
   - Use `--json` when another script will parse the output.

## Commands

```bash
rbc <project> snapshot
rbc <project> navigate https://example.com
rbc <project> click 'button[type="submit"]'
rbc <project> type 'input[name="q"]' 'search text'
rbc <project> key Enter
rbc <project> eval --expression 'document.title'
rbc <project> eval --file /tmp/browser-task.js
rbc <project> screenshot --full-page --output /tmp/page.png
rbc <project> tabs
rbc <project> activate-tab 123
```

## Script Pattern

Use one browser round trip for several DOM reads/actions:

```js
(() => {
  const button = [...document.querySelectorAll('button')]
    .find((el) => /save|submit/i.test(el.innerText || ''));
  if (!button) return { ok: false, error: 'button not found' };
  button.click();
  return { ok: true, title: document.title, url: location.href };
})()
```

Run it:

```bash
rbc "$DOCKER_GIT_PROJECT_ID" --json --trace-dir .browser-trace eval --file /tmp/browser-task.js
```

## Rules

- Do not expose full shared browser URLs in logs or final answers; they contain bearer tokens.
- Do not use `tabs` or `activate-tab` against direct CDP targets; those are shared-extension only.
- If a command fails because no control panel is running, retry with explicit `--share-url` or `--cdp-url`.
- Keep command output compact; use screenshots only when visual state matters.
