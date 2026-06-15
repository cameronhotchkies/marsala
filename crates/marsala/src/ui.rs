use std::{convert::Infallible, path::Path, path::PathBuf, time::Duration};

use anyhow::Result;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use crate::{
    event_log::{read_tail_lines, EventLogFollowEvent, EventLogFollower, EventRecord},
    runtime_settings::RuntimeSettingsSnapshot,
};

const DEFAULT_RECENT_LINES: usize = 200;
const MAX_RECENT_LINES: usize = 2_000;

pub(crate) const INTERCEPTION_UI_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Marsala Interception</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #101413;
      --panel: #171d1b;
      --panel-2: #1d2522;
      --line: #32403b;
      --text: #e4ece7;
      --muted: #9aa8a1;
      --accent: #73c7a3;
      --warn: #d6b36a;
      --bad: #df8e80;
      --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
      --sans: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background: var(--bg);
      color: var(--text);
      font-family: var(--sans);
      font-size: 14px;
    }
    button, input, select {
      font: inherit;
      color: var(--text);
      background: var(--panel-2);
      border: 1px solid var(--line);
      border-radius: 6px;
    }
    button {
      min-height: 34px;
      padding: 0 12px;
      cursor: pointer;
    }
    button:hover, input:focus, select:focus {
      border-color: var(--accent);
      outline: none;
    }
    input, select {
      height: 34px;
      padding: 0 10px;
      min-width: 0;
    }
    label {
      color: var(--muted);
      display: grid;
      gap: 5px;
      font-size: 12px;
    }
    .app {
      height: 100vh;
      display: grid;
      grid-template-rows: auto auto 1fr;
    }
    header {
      padding: 14px 18px 10px;
      border-bottom: 1px solid var(--line);
      display: flex;
      align-items: center;
      gap: 14px;
    }
    h1 {
      margin: 0;
      font-size: 18px;
      font-weight: 650;
      letter-spacing: 0;
    }
    .status {
      color: var(--muted);
      font-family: var(--mono);
      font-size: 12px;
    }
    .toolbar {
      padding: 10px 18px;
      border-bottom: 1px solid var(--line);
      display: grid;
      grid-template-columns: 150px minmax(120px, 1fr) minmax(120px, 1fr) auto auto auto auto;
      gap: 10px;
      align-items: end;
    }
    .toggle {
      height: 34px;
      display: flex;
      align-items: center;
      gap: 8px;
      color: var(--muted);
      font-size: 13px;
      white-space: nowrap;
    }
    .toggle input {
      width: 16px;
      height: 16px;
      padding: 0;
    }
    main {
      min-height: 0;
      display: grid;
      grid-template-columns: minmax(520px, 58%) minmax(320px, 42%);
    }
    .timeline, .details {
      min-width: 0;
      min-height: 0;
      overflow: auto;
    }
    .timeline {
      border-right: 1px solid var(--line);
    }
    table {
      width: 100%;
      border-collapse: collapse;
      table-layout: fixed;
    }
    th {
      position: sticky;
      top: 0;
      z-index: 1;
      background: var(--panel);
      color: var(--muted);
      font-size: 12px;
      font-weight: 600;
      text-align: left;
      border-bottom: 1px solid var(--line);
      padding: 8px 10px;
    }
    td {
      border-bottom: 1px solid rgba(50,64,59,.65);
      padding: 8px 10px;
      vertical-align: top;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    tr {
      cursor: pointer;
    }
    tr:hover, tr.selected {
      background: #1b2a25;
    }
    .time, .code, pre {
      font-family: var(--mono);
    }
    .time {
      color: var(--muted);
      font-size: 12px;
      width: 92px;
    }
    .type { width: 160px; }
    .target { width: 42%; }
    .status-col { width: 120px; }
    .bytes { width: 130px; }
    .pill {
      display: inline-block;
      max-width: 100%;
      padding: 2px 7px;
      border: 1px solid var(--line);
      border-radius: 999px;
      color: var(--accent);
      background: rgba(115,199,163,.08);
      overflow: hidden;
      text-overflow: ellipsis;
      vertical-align: middle;
    }
    .muted { color: var(--muted); }
    .warn { color: var(--warn); }
    .bad { color: var(--bad); }
    .details {
      padding: 16px;
      background: var(--panel);
    }
    .details h2 {
      margin: 0 0 6px;
      font-size: 16px;
      letter-spacing: 0;
    }
    .meta {
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      margin: 10px 0 16px;
    }
    .section {
      margin-top: 14px;
      border-top: 1px solid var(--line);
      padding-top: 12px;
    }
    .section h3 {
      margin: 0 0 8px;
      color: var(--muted);
      font-size: 12px;
      text-transform: uppercase;
      letter-spacing: .08em;
    }
    pre {
      margin: 0;
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      color: #dce7e1;
      background: #0d1110;
      border: 1px solid var(--line);
      border-radius: 6px;
      padding: 12px;
      line-height: 1.45;
      max-height: 44vh;
      overflow: auto;
    }
    .empty {
      color: var(--muted);
      padding: 18px;
    }
    @media (max-width: 900px) {
      .app { height: auto; min-height: 100vh; }
      .toolbar { grid-template-columns: 1fr 1fr; }
      main { grid-template-columns: 1fr; }
      .timeline { border-right: 0; border-bottom: 1px solid var(--line); max-height: 55vh; }
      .details { min-height: 45vh; }
      .bytes { display: none; }
    }
  </style>
</head>
<body>
  <div class="app">
    <header>
      <h1>Marsala Interception</h1>
      <div id="status" class="status" role="status" aria-live="polite" aria-atomic="true">connecting</div>
    </header>
    <section class="toolbar" aria-label="Filters">
      <label>Category
        <select id="category">
          <option value="">All</option>
          <option value="payload">Payloads</option>
          <option value="mitm">MITM metadata</option>
          <option value="proxy">Proxy</option>
          <option value="gateway">Gateway</option>
          <option value="service">Service</option>
        </select>
      </label>
      <label>Host
        <input id="host" type="search" placeholder="chatgpt.com">
      </label>
      <label>Path contains
        <input id="path" type="search" placeholder="/backend-api/">
      </label>
      <label class="toggle"><input id="payloadOnly" type="checkbox">Payload only</label>
      <label class="toggle"><input id="goblinMode" type="checkbox" aria-label="Goblin mode" disabled><span aria-hidden="true">👺</span><span>Goblin mode</span></label>
      <button id="pause">Pause</button>
      <button id="clear">Clear</button>
    </section>
    <main>
      <section class="timeline" aria-label="Event timeline">
        <table>
          <thead>
            <tr>
              <th class="time">Time</th>
              <th class="type">Type</th>
              <th class="target">Request</th>
              <th class="status-col">Status</th>
              <th class="bytes">Bytes</th>
            </tr>
          </thead>
          <tbody id="rows"></tbody>
        </table>
        <div id="empty" class="empty">No matching events yet.</div>
      </section>
      <aside class="details" aria-label="Event details">
        <h2 id="detailTitle">No event selected</h2>
        <div id="detailSummary" class="muted">Select an event from the timeline.</div>
        <div id="detailMeta" class="meta"></div>
        <div id="previewSection" class="section" hidden>
          <h3>Preview</h3>
          <pre id="preview"></pre>
        </div>
        <div id="goblinSection" class="section" hidden>
          <h3>Goblin mode</h3>
          <pre id="goblinDetails"></pre>
        </div>
        <div class="section">
          <h3>Raw event</h3>
          <pre id="raw">{}</pre>
        </div>
      </aside>
    </main>
  </div>
  <script>
    const state = {
      events: [], selected: null, source: null, paused: false, nextId: 1,
      settings: null, settingsSource: null, settingsSaving: false
    };
    const rows = document.getElementById('rows');
    const empty = document.getElementById('empty');
    const statusEl = document.getElementById('status');
    const controls = {
      category: document.getElementById('category'),
      host: document.getElementById('host'),
      path: document.getElementById('path'),
      payloadOnly: document.getElementById('payloadOnly'),
      goblinMode: document.getElementById('goblinMode'),
      pause: document.getElementById('pause'),
      clear: document.getElementById('clear')
    };

    function params() {
      const p = new URLSearchParams();
      if (controls.category.value) p.set('category', controls.category.value);
      if (controls.host.value.trim()) p.set('host', controls.host.value.trim());
      if (controls.path.value.trim()) p.set('path_contains', controls.path.value.trim());
      if (controls.payloadOnly.checked) p.set('payloads_only', 'true');
      p.set('lines', '250');
      return p;
    }

    function setStatus(text) {
      statusEl.textContent = text + ' · visible ' + state.events.length;
    }

    function setExactStatus(text) {
      statusEl.textContent = text;
    }

    function applySettings(settings) {
      if (!settings || typeof settings.revision !== 'number' || typeof settings.goblin_mode !== 'boolean') return;
      state.settings = settings;
      controls.goblinMode.checked = settings.goblin_mode;
      controls.goblinMode.disabled = state.settingsSaving;
    }

    async function loadSettings() {
      controls.goblinMode.disabled = true;
      const response = await fetch('/ui/settings', { cache: 'no-store' });
      if (!response.ok) throw new Error('Goblin mode settings are unavailable');
      applySettings(await response.json());
    }

    function connectSettings() {
      if (state.settingsSource) state.settingsSource.close();
      const source = new EventSource('/ui/settings/events');
      state.settingsSource = source;
      source.addEventListener('settings', message => applySettings(JSON.parse(message.data)));
      source.addEventListener('error', () => {
        if (!state.settings) controls.goblinMode.disabled = true;
      });
    }

    async function saveGoblinMode(enabled) {
      if (!state.settings || state.settingsSaving) return;
      const previous = state.settings;
      state.settingsSaving = true;
      controls.goblinMode.disabled = true;
      controls.goblinMode.checked = enabled;
      try {
        const response = await fetch('/ui/settings', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ expected_revision: previous.revision, goblin_mode: enabled })
        });
        const body = await response.json().catch(() => ({}));
        if (!response.ok) {
          if (body.settings) applySettings(body.settings);
          throw new Error(body.error && body.error.message ? body.error.message : 'Goblin mode could not be saved');
        }
        applySettings(body);
        if (enabled) setExactStatus('Dance baby, dance! Goblins are back on the menu!');
        else setExactStatus('Goblin mode disabled.');
      } catch (error) {
        if (!state.settings || state.settings.revision === previous.revision) applySettings(previous);
        setExactStatus('Goblin mode update failed: ' + error.message);
      } finally {
        state.settingsSaving = false;
        controls.goblinMode.disabled = !state.settings;
      }
    }

    function connect() {
      if (state.source) state.source.close();
      if (state.paused) {
        setStatus('paused');
        return;
      }
      const p = params();
      p.delete('lines');
      const source = new EventSource('/ui/events?' + p.toString());
      state.source = source;
      setStatus('connecting');
      source.addEventListener('open', () => setStatus('live'));
      source.addEventListener('event', (message) => appendEvent(JSON.parse(message.data)));
      source.addEventListener('error', () => setStatus('reconnecting'));
    }

    async function loadRecent() {
      const response = await fetch('/ui/events/recent?' + params().toString(), { cache: 'no-store' });
      if (!response.ok) throw new Error(await response.text());
      state.events = (await response.json()).map(assignId);
      render();
      setStatus(state.paused ? 'paused' : 'live');
    }

    function assignId(event) {
      event._id = state.nextId++;
      return event;
    }

    function appendEvent(event) {
      state.events.push(assignId(event));
      if (state.events.length > 800) state.events.shift();
      render();
      setStatus('live');
    }

    function render() {
      rows.innerHTML = '';
      empty.hidden = state.events.length !== 0;
      for (const event of state.events) {
        const tr = document.createElement('tr');
        tr.dataset.id = event._id;
        if (state.selected && state.selected._id === event._id) tr.classList.add('selected');
        tr.innerHTML = `
          <td class="time">${escapeHtml(shortTime(event.timestamp))}</td>
          <td class="type"><span class="pill">${escapeHtml(event.event_type)}</span></td>
          <td class="target">${escapeHtml(targetText(event))}</td>
          <td class="status-col ${statusClass(event)}">${escapeHtml(statusText(event))}</td>
          <td class="bytes muted">${escapeHtml(event.byte_summary || '')}</td>`;
        tr.addEventListener('click', () => selectEvent(event));
        rows.appendChild(tr);
      }
      if (state.selected && !state.events.find(e => e._id === state.selected._id)) {
        state.selected = null;
      }
      renderDetails(state.selected);
    }

    function selectEvent(event) {
      state.selected = event;
      render();
    }

    function renderDetails(event) {
      document.getElementById('detailTitle').textContent = event ? event.event_type : 'No event selected';
      document.getElementById('detailSummary').textContent = event ? event.summary : 'Select an event from the timeline.';
      const meta = document.getElementById('detailMeta');
      meta.innerHTML = '';
      if (event) {
        for (const value of [event.category, event.target_host, event.method, event.direction, statusText(event), goblinMarker(event), event.byte_summary, event.auth_summary, event.preview_status, event.truncated ? 'truncated' : '']) {
          if (!value) continue;
          const span = document.createElement('span');
          span.className = 'pill';
          span.textContent = value;
          meta.appendChild(span);
        }
      }
      const previewSection = document.getElementById('previewSection');
      const preview = document.getElementById('preview');
      if (event && event.preview) {
        previewSection.hidden = false;
        preview.textContent = event.preview;
      } else if (event && previewUnavailableText(event)) {
        previewSection.hidden = false;
        preview.textContent = previewUnavailableText(event);
      } else {
        previewSection.hidden = true;
        preview.textContent = '';
      }
      const goblinSection = document.getElementById('goblinSection');
      const goblinDetails = document.getElementById('goblinDetails');
      if (event && hasGoblinAudit(event)) {
        const values = [
          ['enabled at request time', event.goblin_mode_enabled],
          ['applied', event.goblin_mode_applied],
          ['outcome', event.goblin_mode_outcome],
          ['reason', event.goblin_mode_reason],
          ['rule version', event.goblin_rule_version],
          ['local request ID', event.request_id],
          ['provider response ID', event.provider_response_id]
        ].filter(([, value]) => value !== null && value !== undefined && value !== '');
        goblinSection.hidden = false;
        goblinDetails.textContent = values.map(([label, value]) => label + ': ' + value).join('\n');
      } else {
        goblinSection.hidden = true;
        goblinDetails.textContent = '';
      }
      document.getElementById('raw').textContent = event ? JSON.stringify(event.raw, null, 2) : '{}';
    }

    function targetText(event) {
      return [event.method, event.path || event.target_host || '', event.direction ? '(' + event.direction + ')' : '']
        .filter(Boolean)
        .join(' ');
    }

    function statusText(event) {
      const parts = [];
      if (event.status) parts.push(String(event.status));
      if (event.upstream_status) parts.push('up ' + event.upstream_status);
      if (event.truncated) parts.push('truncated');
      const goblin = goblinMarker(event);
      if (goblin) parts.push(goblin);
      return parts.join(' · ');
    }

    function hasGoblinAudit(event) {
      return event.goblin_mode_enabled !== null && event.goblin_mode_enabled !== undefined
        || event.goblin_mode_applied !== null && event.goblin_mode_applied !== undefined
        || Boolean(event.goblin_mode_outcome);
    }

    function goblinMarker(event) {
      if (event.goblin_mode_applied === true) return 'Goblin applied';
      if (event.goblin_mode_enabled === true) return 'Goblin on';
      return '';
    }

    function statusClass(event) {
      const status = Number(event.upstream_status || event.status);
      if (status >= 500) return 'bad';
      if (status >= 400 || event.truncated) return 'warn';
      return '';
    }

    function previewUnavailableText(event) {
      const data = event.raw && event.raw.data ? event.raw.data : {};
      if (event.preview_status === 'binary_skipped') return 'binary payload skipped';
      if (event.preview_status === 'compressed_preview_unavailable') return data.preview_error || 'compressed preview unavailable';
      if (event.preview_status === 'compressed_fragmented_preview_unavailable') return 'compressed fragmented preview unavailable';
      if (event.preview_status === 'decoded_non_utf8') return 'decoded payload is not UTF-8';
      if (data.utf8 === false) return data.compressed ? 'compressed preview unavailable' : 'text preview unavailable: not UTF-8';
      return '';
    }

    function shortTime(value) {
      const date = new Date(value);
      return Number.isNaN(date.getTime()) ? value : date.toLocaleTimeString();
    }

    function escapeHtml(value) {
      return String(value ?? '').replace(/[&<>"']/g, ch => ({
        '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
      }[ch]));
    }

    let filterTimer = null;
    for (const control of [controls.category, controls.host, controls.path, controls.payloadOnly]) {
      control.addEventListener('input', () => {
        clearTimeout(filterTimer);
        filterTimer = setTimeout(async () => {
          await loadRecent();
          connect();
        }, 150);
      });
    }
    controls.pause.addEventListener('click', () => {
      state.paused = !state.paused;
      controls.pause.textContent = state.paused ? 'Resume' : 'Pause';
      connect();
    });
    controls.clear.addEventListener('click', () => {
      state.events = [];
      state.selected = null;
      render();
      setStatus(state.paused ? 'paused' : 'live');
    });
    controls.goblinMode.addEventListener('change', () => saveGoblinMode(controls.goblinMode.checked));

    loadRecent().then(connect).catch(error => setStatus('error: ' + error.message));
    loadSettings().then(connectSettings).catch(error => setStatus('error: ' + error.message));
  </script>
</body>
</html>
"#;

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UiEventFilter {
    pub category: Option<String>,
    pub host: Option<String>,
    pub path_contains: Option<String>,
    pub payloads_only: Option<bool>,
    pub lines: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UiEvent {
    pub timestamp: String,
    pub event_type: String,
    pub category: String,
    pub method: Option<String>,
    pub path: Option<String>,
    pub target_host: Option<String>,
    pub status: Option<String>,
    pub upstream_status: Option<String>,
    pub direction: Option<String>,
    pub byte_summary: Option<String>,
    pub auth_summary: Option<String>,
    pub truncated: bool,
    pub preview: Option<String>,
    pub preview_status: Option<String>,
    pub goblin_mode_enabled: Option<bool>,
    pub goblin_mode_applied: Option<bool>,
    pub goblin_mode_outcome: Option<String>,
    pub goblin_mode_reason: Option<String>,
    pub goblin_rule_version: Option<String>,
    pub request_id: Option<String>,
    pub provider_response_id: Option<String>,
    pub summary: String,
    pub raw: Value,
}

pub(crate) fn recent_events(path: &Path, filter: &UiEventFilter) -> Result<Vec<UiEvent>> {
    let lines = filter
        .lines
        .unwrap_or(DEFAULT_RECENT_LINES)
        .clamp(1, MAX_RECENT_LINES);
    let events = read_tail_lines(path, lines)?
        .into_iter()
        .filter_map(|line| ui_event_from_line(&line).ok().flatten())
        .filter(|event| filter.matches(event))
        .collect();
    Ok(events)
}

pub(crate) fn event_stream(
    path: PathBuf,
    filter: UiEventFilter,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let stream = stream::unfold(
        (EventLogFollower::from_end(path), filter),
        |(mut follower, filter)| async move {
            loop {
                match follower.poll() {
                    Ok(events) => {
                        for event in events {
                            let EventLogFollowEvent::Line(line) = event else {
                                continue;
                            };
                            let Ok(Some(ui_event)) = ui_event_from_line(&line) else {
                                continue;
                            };
                            if !filter.matches(&ui_event) {
                                continue;
                            }
                            let data = match serde_json::to_string(&ui_event) {
                                Ok(data) => data,
                                Err(error) => {
                                    serde_json::json!({ "error": error.to_string() }).to_string()
                                }
                            };
                            return Some((
                                Ok(Event::default().event("event").data(data)),
                                (follower, filter),
                            ));
                        }
                    }
                    Err(error) => {
                        let data = serde_json::json!({ "error": error.to_string() }).to_string();
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        return Some((
                            Ok(Event::default().event("error").data(data)),
                            (follower, filter),
                        ));
                    }
                }

                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(crate) fn settings_stream(
    receiver: watch::Receiver<RuntimeSettingsSnapshot>,
) -> Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>> {
    let stream = stream::unfold((receiver, true), |(mut receiver, initial)| async move {
        if !initial && receiver.changed().await.is_err() {
            return None;
        }
        let snapshot = *receiver.borrow_and_update();
        let data = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        Some((
            Ok(Event::default().event("settings").data(data)),
            (receiver, false),
        ))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(crate) fn ui_event_from_line(line: &str) -> Result<Option<UiEvent>> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let record: EventRecord = serde_json::from_str(line)?;
    Ok(Some(ui_event_from_record(record)))
}

pub(crate) fn ui_event_from_record(record: EventRecord) -> UiEvent {
    let data = &record.data;
    let method = string_field(data, "method");
    let path = string_field(data, "path").or_else(|| string_field(data, "target"));
    let target_host =
        string_field(data, "target_host").or_else(|| host_from_target(path.as_deref()));
    let status = display_field(data, "status");
    let upstream_status = display_field(data, "upstream_status");
    let direction = string_field(data, "direction");
    let byte_summary = byte_summary(data);
    let auth_summary = auth_summary(data);
    let truncated = bool_field(data, "truncated").unwrap_or(false);
    let preview = string_field(data, "preview");
    let preview_status = string_field(data, "preview_status");
    let goblin_mode_enabled = bool_field(data, "goblin_mode_enabled");
    let explicit_goblin_mode_applied = bool_field(data, "goblin_mode_applied");
    let has_goblin_audit = goblin_mode_enabled.is_some()
        || explicit_goblin_mode_applied.is_some()
        || data.get("goblin_mode_outcome").is_some();
    let goblin_mode_outcome = string_field(data, "goblin_mode_outcome").or_else(|| {
        has_goblin_audit
            .then(|| string_field(data, "outcome").or_else(|| string_field(data, "status")))
            .flatten()
    });
    let goblin_mode_applied = explicit_goblin_mode_applied.or_else(|| {
        goblin_mode_outcome
            .as_deref()
            .map(|outcome| outcome == "applied")
    });
    let goblin_mode_reason = string_field(data, "goblin_mode_reason").or_else(|| {
        goblin_mode_outcome
            .as_ref()
            .and_then(|_| string_field(data, "reason"))
    });
    let goblin_rule_version =
        display_field(data, "goblin_rule_version").or_else(|| display_field(data, "rule_version"));
    let request_id = string_field(data, "request_id");
    let provider_response_id = string_field(data, "provider_response_id");
    let category = category_for(&record.event_type).to_string();
    let summary = summary_text(
        &record.event_type,
        method.as_deref(),
        path.as_deref(),
        target_host.as_deref(),
        status.as_deref(),
        upstream_status.as_deref(),
        direction.as_deref(),
        byte_summary.as_deref(),
        truncated,
    );
    let raw = serde_json::json!({
        "timestamp": record.timestamp,
        "event_type": record.event_type,
        "data": record.data,
    });

    UiEvent {
        timestamp: record.timestamp.to_rfc3339(),
        event_type: raw["event_type"].as_str().unwrap_or_default().to_string(),
        category,
        method,
        path,
        target_host,
        status,
        upstream_status,
        direction,
        byte_summary,
        auth_summary,
        truncated,
        preview,
        preview_status,
        goblin_mode_enabled,
        goblin_mode_applied,
        goblin_mode_outcome,
        goblin_mode_reason,
        goblin_rule_version,
        request_id,
        provider_response_id,
        summary,
        raw,
    }
}

impl UiEventFilter {
    fn matches(&self, event: &UiEvent) -> bool {
        if self.payloads_only.unwrap_or(false) && event.category != "payload" {
            return false;
        }

        if let Some(category) = non_empty(self.category.as_deref()) {
            if category != "all" && event.category != category {
                return false;
            }
        }

        if let Some(host) = non_empty(self.host.as_deref()) {
            let Some(target_host) = &event.target_host else {
                return false;
            };
            if !target_host
                .to_ascii_lowercase()
                .contains(&host.to_ascii_lowercase())
            {
                return false;
            }
        }

        if let Some(path_contains) = non_empty(self.path_contains.as_deref()) {
            let Some(path) = &event.path else {
                return false;
            };
            if !path
                .to_ascii_lowercase()
                .contains(&path_contains.to_ascii_lowercase())
            {
                return false;
            }
        }

        true
    }
}

fn category_for(event_type: &str) -> &'static str {
    match event_type {
        "mitm_payload" | "mitm_websocket_frame" => "payload",
        event_type if event_type.starts_with("mitm_") => "mitm",
        event_type if event_type.starts_with("proxy_") => "proxy",
        "http_request"
        | "chat_completions_request"
        | "chat_completions_response"
        | "responses_request"
        | "responses_response" => "gateway",
        _ => "service",
    }
}

fn summary_text(
    event_type: &str,
    method: Option<&str>,
    path: Option<&str>,
    target_host: Option<&str>,
    status: Option<&str>,
    upstream_status: Option<&str>,
    direction: Option<&str>,
    byte_summary: Option<&str>,
    truncated: bool,
) -> String {
    let mut parts = vec![event_type.to_string()];
    if let Some(direction) = direction {
        parts.push(direction.to_string());
    }
    if let Some(method) = method {
        parts.push(method.to_string());
    }
    if let Some(target_host) = target_host {
        parts.push(target_host.to_string());
    }
    if let Some(path) = path {
        parts.push(path.to_string());
    }
    if let Some(status) = status {
        parts.push(format!("status={status}"));
    }
    if let Some(upstream_status) = upstream_status {
        parts.push(format!("upstream={upstream_status}"));
    }
    if let Some(byte_summary) = byte_summary {
        parts.push(byte_summary.to_string());
    }
    if truncated {
        parts.push("truncated".to_string());
    }
    parts.join(" ")
}

fn byte_summary(data: &Value) -> Option<String> {
    let fields = [
        ("body", "body_bytes"),
        ("payload", "payload_bytes"),
        ("preview", "preview_bytes"),
        ("req_body", "request_body_bytes"),
        ("resp_body", "response_body_bytes"),
        ("headers", "header_bytes"),
        ("resp_headers", "response_header_bytes"),
    ];
    let parts: Vec<_> = fields
        .into_iter()
        .filter_map(|(label, key)| u64_field(data, key).map(|value| format!("{label}={value}B")))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn auth_summary(data: &Value) -> Option<String> {
    let auth_shape = data.get("auth_shape")?.as_object()?;
    let mut parts = Vec::new();
    for key in [
        "authorization",
        "proxy_authorization",
        "cookie",
        "x_api_key",
    ] {
        if auth_shape
            .get(&format!("{key}_present"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(scheme) = auth_shape
                .get(&format!("{key}_scheme"))
                .and_then(Value::as_str)
            {
                parts.push(format!("{key}:{scheme}"));
            } else {
                parts.push(key.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn host_from_target(target: Option<&str>) -> Option<String> {
    let target = target?;
    if target.starts_with('/') {
        return None;
    }
    Some(
        target
            .split('/')
            .next()
            .unwrap_or(target)
            .trim_end_matches(":443")
            .to_string(),
    )
}

fn string_field(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn display_field(data: &Value, key: &str) -> Option<String> {
    let value = data.get(key)?;
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(number) = value.as_u64() {
        return Some(number.to_string());
    }
    Some(value.to_string())
}

fn u64_field(data: &Value, key: &str) -> Option<u64> {
    data.get(key).and_then(Value::as_u64)
}

fn bool_field(data: &Value, key: &str) -> Option<bool> {
    data.get(key).and_then(Value::as_bool)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use serde_json::json;

    use super::*;

    fn record(event_type: &str, data: Value) -> EventRecord {
        EventRecord {
            timestamp: DateTime::parse_from_rfc3339("2026-06-10T12:00:00Z")
                .expect("timestamp")
                .to_utc(),
            event_type: event_type.to_string(),
            data,
        }
    }

    #[test]
    fn goblin_controls_use_exact_status_and_accessibility_contracts() {
        assert!(INTERCEPTION_UI_HTML.contains(
            r#"id="status" class="status" role="status" aria-live="polite" aria-atomic="true""#
        ));
        assert!(INTERCEPTION_UI_HTML.contains(r#"aria-label="Goblin mode""#));
        assert!(INTERCEPTION_UI_HTML
            .contains("setExactStatus('Dance baby, dance! Goblins are back on the menu!')"));
        assert!(INTERCEPTION_UI_HTML.contains("setExactStatus('Goblin mode disabled.')"));
        assert!(INTERCEPTION_UI_HTML
            .contains("setExactStatus('Goblin mode update failed: ' + error.message)"));
        assert!(INTERCEPTION_UI_HTML.contains("if (goblin) parts.push(goblin)"));
    }

    #[test]
    fn transforms_payload_event_for_ui() {
        let event = ui_event_from_record(record(
            "mitm_payload",
            json!({
                "target_host": "chatgpt.com",
                "method": "POST",
                "path": "/backend-api/conversation",
                "direction": "request",
                "body_bytes": 128,
                "preview_bytes": 64,
                "truncated": true,
                "preview": "{\"message\":\"hello\"}"
            }),
        ));

        assert_eq!(event.category, "payload");
        assert_eq!(event.target_host.as_deref(), Some("chatgpt.com"));
        assert_eq!(event.direction.as_deref(), Some("request"));
        assert_eq!(event.byte_summary.as_deref(), Some("body=128B preview=64B"));
        assert!(event.truncated);
        assert!(event.summary.contains("truncated"));
    }

    #[test]
    fn transforms_websocket_preview_status_for_ui() {
        let event = ui_event_from_record(record(
            "mitm_websocket_frame",
            json!({
                "target_host": "chatgpt.com",
                "method": "GET",
                "path": "/backend-api/codex/responses",
                "direction": "request",
                "opcode": "text",
                "payload_bytes": 64,
                "preview_bytes": 32,
                "utf8": true,
                "compressed": true,
                "decoded": true,
                "preview_status": "decoded",
                "preview": "{\"input\":\"hello\"}"
            }),
        ));

        assert_eq!(event.category, "payload");
        assert_eq!(event.preview_status.as_deref(), Some("decoded"));
        assert_eq!(event.preview.as_deref(), Some("{\"input\":\"hello\"}"));

        let unavailable = ui_event_from_record(record(
            "mitm_websocket_frame",
            json!({
                "target_host": "chatgpt.com",
                "method": "GET",
                "path": "/backend-api/codex/responses",
                "direction": "request",
                "opcode": "text",
                "payload_bytes": 64,
                "utf8": false,
                "compressed": true,
                "decoded": false,
                "preview_status": "compressed_preview_unavailable",
                "preview_error": "deflate_decode_failed: corrupt deflate stream"
            }),
        ));

        assert_eq!(
            unavailable.preview_status.as_deref(),
            Some("compressed_preview_unavailable")
        );
        assert!(unavailable.preview.is_none());
        assert_eq!(
            unavailable.raw["data"]["preview_error"],
            "deflate_decode_failed: corrupt deflate stream"
        );
    }

    #[test]
    fn exposes_goblin_audit_fields_without_using_current_settings() {
        let event = ui_event_from_record(record(
            "codex_response_terminal",
            json!({
                "goblin_mode_enabled": true,
                "goblin_mode_applied": true,
                "goblin_mode_outcome": "applied",
                "goblin_mode_reason": "exact_match",
                "goblin_rule_version": 1,
                "request_id": "marsala-42",
                "provider_response_id": "resp_123"
            }),
        ));

        assert_eq!(event.goblin_mode_enabled, Some(true));
        assert_eq!(event.goblin_mode_applied, Some(true));
        assert_eq!(event.goblin_mode_outcome.as_deref(), Some("applied"));
        assert_eq!(event.goblin_mode_reason.as_deref(), Some("exact_match"));
        assert_eq!(event.goblin_rule_version.as_deref(), Some("1"));
        assert_eq!(event.request_id.as_deref(), Some("marsala-42"));
        assert_eq!(event.provider_response_id.as_deref(), Some("resp_123"));
    }

    #[test]
    fn maps_current_websocket_transform_audit_shape() {
        let event = ui_event_from_record(record(
            "mitm_websocket_transform",
            json!({
                "goblin_mode_enabled": true,
                "status": "applied",
                "reason": "target_removed",
                "request_id": "marsala-7"
            }),
        ));

        assert_eq!(event.goblin_mode_applied, Some(true));
        assert_eq!(event.goblin_mode_outcome.as_deref(), Some("applied"));
        assert_eq!(event.goblin_mode_reason.as_deref(), Some("target_removed"));
    }

    #[test]
    fn filter_matches_payload_host_and_path() {
        let event = ui_event_from_record(record(
            "mitm_websocket_frame",
            json!({
                "target_host": "ab.chatgpt.com",
                "path": "/socket?token=[redacted]",
                "direction": "response",
                "payload_bytes": 42
            }),
        ));
        let filter = UiEventFilter {
            category: Some("payload".to_string()),
            host: Some("CHATGPT".to_string()),
            path_contains: Some("socket".to_string()),
            payloads_only: Some(true),
            lines: None,
        };

        assert!(filter.matches(&event));

        let filter = UiEventFilter {
            host: Some("api.openai.com".to_string()),
            ..filter
        };
        assert!(!filter.matches(&event));
    }
}
