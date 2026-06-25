---
name: browser-cli
description: Use rbc CLI commands to inspect and automate browser sessions without MCP, including Playwright-style scripts for CDP and shared-extension browsers. Trigger this skill when Codex needs browser automation, page inspection, clicking, typing, tab/window control, screenshots, Playwright page APIs, or multi-step JavaScript browser scripts while minimizing MCP token overhead.
---

# Browser CLI

Use `rbc` instead of MCP browser tools when a normal shell command can drive the active browser. Prefer CLI commands for repeatable page workflows, scripts, screenshots, and audit traces.

## Workflow

1. Resolve the target:
   - Use `rbc edge ...` for a browser shared by the Edge extension.
   - Use `rbc chromium ...` for the managed container browser.
   - Use `rbc active ...` when the noVNC/control panel selected browser should be used.
   - Use `--control-url`, `--share-url`, or `--cdp-url` only when overriding the configured pool.
2. Inspect first:
   - Run `rbc <browser> snapshot` before choosing selectors.
   - For shared-extension targets, run `rbc <browser> tabs` to see windows/tabs and `rbc <browser> activate-tab <id>` before acting on a non-active tab.
3. Prefer scripts for multi-step actions:
   - Put complex DOM logic in a temporary `.js` file.
   - Run it with `rbc <browser> eval --file /path/to/script.js`.
   - Return a compact JSON object from the script.
4. Log actions when transparency matters:
   - Add `--trace-dir <dir>` to append `rbc.jsonl`.
   - Use `--json` when another script will parse the output.

## Commands

```bash
rbc edge snapshot
rbc edge navigate https://example.com
rbc edge click 'button[type="submit"]'
rbc edge type 'input[name="q"]' 'search text'
rbc edge key Enter
rbc edge eval 'document.title'
rbc edge eval --file /tmp/browser-task.js
rbc edge pw --code 'return await page.title()'
rbc edge pw /tmp/playwright-task.js
rbc edge screenshot --full-page --output /tmp/page.png
rbc edge tabs
rbc edge activate-tab 123
rbc chromium snapshot
rbc active pw --code 'return await page.title()'
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
rbc edge --json --trace-dir .browser-trace eval --file /tmp/browser-task.js
```

## Playwright Pattern

Use `pw` when Playwright-style page APIs are more ergonomic. CDP-backed targets run real
Playwright; shared-extension targets run the supported compatibility subset.

```js
await page.goto('https://example.com');
await page.getByRole('link', { name: /more/i }).click();
return { title: await page.title(), url: page.url() };
```

Run it:

```bash
rbc edge --json --trace-dir .browser-trace pw /tmp/playwright-task.js
```

`pw` exposes `playwright`, `browser`, `context`, `page`, and `pages` in scope. On shared-extension targets, use the supported subset: `page.goto`, `title`, `url`, `evaluate`, `click`, `fill`, `type`, `press`, `screenshot`, `content`, `locator`, `getByText`, `getByRole`, `waitForSelector`, `waitForTimeout`, and `waitForLoadState`. Unsupported Playwright APIs fail with a clear error.

## Rules

- Do not expose full shared browser URLs in logs or final answers; they contain bearer tokens.
- Prefer browser names such as `edge`, `chromium`, or `active`; the platform should provide `BROWSER_CONNECTION_CONTROL_URL` or browser env aliases.
- Do not use `tabs` or `activate-tab` against direct CDP targets; those are shared-extension only.
- Expect `pw` on shared-extension targets to be a compatibility subset, not full Playwright.
- If browser-pool discovery fails, retry with explicit `--control-url`, `--share-url`, or `--cdp-url`.
- Keep command output compact; use screenshots only when visual state matters.
