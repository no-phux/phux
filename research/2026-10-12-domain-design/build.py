#!/usr/bin/env python3
"""Render the domain-design Markdown files to self-contained review pages.

Usage: uv run --with markdown python3 research/2026-10-12-domain-design/build.py

Each <name>.md becomes <name>.html beside it: no external scripts, fonts, or
network requests. Per-section note boxes save to browser localStorage and
export as Markdown. DESIGN.md is excluded; DESIGN.html is the hand-drawn
visual summary.
"""

import hashlib
import html
import json
import pathlib
import re

import markdown

HERE = pathlib.Path(__file__).resolve().parent
DRAFT = "Draft 3 · 12 October 2026"

# Pages, in reading order. FULL-DESIGN keeps draft 1's storage key so notes
# taken on draft 1 resurface as "Notes from an earlier draft".
PAGES = [
    ("FULL-DESIGN", "DOMAIN MODEL", "phux-domain-review-2026-10-12-v1"),
    ("CONTRACTS", "CONTRACTS", "phux-domain-review-contracts"),
    ("PLAN", "BUILD PLAN", "phux-domain-review-plan"),
    ("MIGRATION", "MIGRATION", "phux-domain-review-migration"),
    ("BROWSER-KIND", "BROWSER KIND", "phux-domain-review-browser"),
]

CSS = r"""
:root{--bg:#101317;--panel:#191e24;--text:#e9edf2;--muted:#a9b3c0;--line:#333e4c;--accent:#88dac4;--code:#11171e}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.7 system-ui,-apple-system,sans-serif}
a{color:var(--accent)}button{font:inherit;color:var(--text);background:var(--panel);border:1px solid var(--line);border-radius:7px;padding:.5rem .8rem;cursor:pointer}button:hover{border-color:var(--accent)}a:focus-visible,button:focus-visible,textarea:focus-visible{outline:3px solid var(--accent);outline-offset:3px}
.top{padding:1.2rem 2rem;border-bottom:1px solid var(--line);display:flex;align-items:center;justify-content:space-between;gap:1rem}.brand{font-weight:750;letter-spacing:.06em}.badge{color:var(--accent);font-size:.85rem;margin-left:.7rem}.actions{display:flex;flex-wrap:wrap;gap:.6rem}
.shell{display:grid;grid-template-columns:260px minmax(0,1fr);max-width:1510px;margin:auto}nav{position:sticky;top:0;align-self:start;max-height:100vh;overflow:auto;padding:2rem 1.5rem;font-size:.85rem}nav ul{list-style:none;padding:0}nav li{margin:.6rem 0}nav a{text-decoration:none;color:var(--muted)}nav a:hover{color:var(--accent)}nav .toc>ul>li>a{display:none}nav .toc ul ul{padding:0}nav .toc ul ul ul{display:none}
main{max-width:1120px;padding:2rem 3rem 6rem;min-width:0}h1{font-size:clamp(2rem,4vw,3.4rem);line-height:1.12;letter-spacing:-.04em;margin:1rem 0 2rem;max-width:900px}h2{font-size:1.65rem;line-height:1.3;margin:4rem 0 1.3rem;padding-top:1.2rem;border-top:1px solid var(--line);scroll-margin-top:1rem}h3{font-size:1.15rem;margin:2rem 0 .7rem;color:var(--accent)}p{margin:1rem 0}li{margin:.4rem 0}strong{font-weight:700}code{font-size:.87em;background:var(--code);padding:.15rem .35rem;border-radius:4px}pre{background:var(--code);border:1px solid var(--line);border-radius:9px;overflow:auto;padding:1.3rem;font:13px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;tab-size:4}pre code{padding:0;background:none;font:inherit}
.table-wrap{overflow:auto;margin:1.5rem 0}table{border-collapse:collapse;width:100%;font-size:.91rem}th,td{border:1px solid var(--line);padding:.65rem .8rem;text-align:left;vertical-align:top}th{background:var(--panel);font-weight:650}tbody tr:nth-child(even){background:var(--panel)}blockquote{border-left:3px solid var(--accent);margin:1.5rem 0;padding:.2rem 1.2rem;color:var(--muted)}
.intro{color:var(--muted);font-size:.9rem}.review-note{margin:2rem 0;padding:1rem 1.2rem;background:var(--panel);border:1px solid var(--line);border-radius:9px}.review-note label{display:block;color:var(--accent);font-size:.85rem;font-weight:600;margin-bottom:.6rem}textarea{width:100%;min-height:110px;resize:vertical;padding:.8rem;background:var(--bg);color:var(--text);border:1px solid var(--line);border-radius:5px;font:inherit;line-height:1.5}.note-status{font-size:.8rem;color:var(--muted);margin-top:.5rem}.footer{color:var(--muted);font-size:.8rem;border-top:1px solid var(--line);padding-top:1rem}.footer code{overflow-wrap:anywhere}
@media(max-width:900px){.shell{grid-template-columns:1fr}nav{position:static;max-height:320px;border-bottom:1px solid var(--line)}main{padding:1.5rem 1rem 4rem}.top{padding:1rem;align-items:flex-start;flex-direction:column}}
@media print{body{background:white;color:black;font-size:10pt}nav,.top,.intro{display:none}.shell{display:block}main{max-width:none;padding:0}h2{break-before:page}pre{background:#f5f5f5;border-color:#bbb;font-size:8pt;white-space:pre-wrap}table{font-size:9pt}th,td{border-color:#bbb}th,tbody tr:nth-child(even){background:#f5f5f5}.review-note{background:#fafafa;border-color:#bbb}textarea{background:white;color:black;border:0;min-height:0}.review-note.empty{display:none}.note-status{display:none}a{color:inherit}.footer{color:#444}}
.orphans{margin:1.5rem 0;padding:1rem 1.2rem;border:1px dashed var(--accent);border-radius:9px}.orphans h2{border:0;margin:0 0 .6rem;padding:0;font-size:1.05rem}.orphans pre{white-space:pre-wrap}
"""

SCRIPT = r"""
const storageKey = __STORAGE_KEY__;
const sourceLabel = __SOURCE_LABEL__;
let notes = {};
let storageAvailable = true;
try { notes = JSON.parse(localStorage.getItem(storageKey) || '{}'); }
catch { storageAvailable = false; }
if (!notes || typeof notes !== 'object' || Array.isArray(notes)) notes = {};

function saveNotes(status) {
  try { localStorage.setItem(storageKey, JSON.stringify(notes)); status.textContent = 'Saved locally in this browser. Export for a portable copy.'; }
  catch { status.textContent = 'Local saving unavailable. Export your notes before closing this page.'; }
}
function notePanel(heading) {
  const panel = document.createElement('section');
  panel.className = 'review-note empty';
  const label = document.createElement('label');
  label.textContent = 'Your notes · ' + heading.textContent;
  const field = document.createElement('textarea');
  field.id = 'note-' + heading.id;
  label.htmlFor = field.id;
  field.placeholder = 'What should change? Reference a decision or paragraph here…';
  field.value = typeof notes[heading.id] === 'string' ? notes[heading.id] : '';
  panel.classList.toggle('empty', !field.value.trim());
  const status = document.createElement('div');
  status.className = 'note-status';
  status.setAttribute('role', 'status');
  status.textContent = storageAvailable ? 'Notes save locally. Export to share them.' : 'Local saving unavailable. Export before closing.';
  field.addEventListener('input', () => {
    notes[heading.id] = field.value;
    panel.classList.toggle('empty', !field.value.trim());
    saveNotes(status);
  });
  panel.append(label, field, status);
  return panel;
}
const headings = [...document.querySelectorAll('main h2')];
const headingIds = new Set(headings.map(h => h.id));
for (let index = 0; index < headings.length; index += 1) {
  const panel = notePanel(headings[index]);
  const next = headings[index + 1];
  const before = next || document.querySelector('.footer');
  before.before(panel);
}
// Notes written against an earlier draft whose section no longer exists.
const orphans = Object.entries(notes).filter(([id, text]) => !headingIds.has(id) && typeof text === 'string' && text.trim());
if (orphans.length) {
  const box = document.createElement('section');
  box.className = 'orphans';
  const title = document.createElement('h2');
  title.textContent = 'Notes from an earlier draft';
  box.append(title);
  for (const [id, text] of orphans) {
    const label = document.createElement('p');
    label.textContent = '§ ' + id;
    const body = document.createElement('pre');
    body.textContent = text;
    box.append(label, body);
  }
  document.querySelector('main h1').after(box);
}
for (const table of document.querySelectorAll('main table')) {
  const wrap = document.createElement('div');
  wrap.className = 'table-wrap';
  table.before(wrap);
  wrap.append(table);
}
function exportNotes() {
  const parts = ['# ' + document.title + ' — review notes', '', 'Source: ' + sourceLabel, ''];
  for (const heading of headings) {
    const field = document.getElementById('note-' + heading.id);
    if (field.value.trim()) parts.push('## ' + heading.textContent, '', field.value.trim(), '');
  }
  for (const [id, text] of orphans) parts.push('## (earlier draft) ' + id, '', text.trim(), '');
  const blob = new Blob([parts.join('\n')], {type:'text/markdown;charset=utf-8'});
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = 'phux-domain-review-notes.md';
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
document.getElementById('export').addEventListener('click', exportNotes);
document.getElementById('print').addEventListener('click', () => window.print());
"""


def strip_frontmatter(text):
    if text.startswith("---\n"):
        end = text.index("\n---\n", 4)
        return text[end + 5 :]
    return text


def render(stem, label, storage_key):
    source = (HERE / f"{stem}.md").read_text()
    digest = hashlib.sha256(source.encode()).hexdigest()
    md = markdown.Markdown(
        extensions=["tables", "fenced_code", "toc", "sane_lists"],
        extension_configs={"toc": {"toc_depth": "1-3"}},
    )
    body = md.convert(strip_frontmatter(source))
    # Cross-document links point at the rendered pages.
    body = re.sub(
        r'href="(%s)\.md(#[^"]*)?"' % "|".join(s for s, _, _ in PAGES),
        lambda m: f'href="{m.group(1)}.html{m.group(2) or ""}"',
        body,
    )
    title = html.escape(md.toc_tokens[0]["name"] if md.toc_tokens else stem)
    nav = " · ".join(
        f'<a href="{s}.html">{l.title()}</a>' if s != stem else f"<strong>{l.title()}</strong>"
        for s, l, _ in PAGES
    )
    script = SCRIPT.replace("__STORAGE_KEY__", json.dumps(storage_key)).replace(
        "__SOURCE_LABEL__", json.dumps(f"{stem}.md, {DRAFT}, sha256 {digest}")
    )
    page = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="dark">
<title>{title}</title>
<style>
{CSS}
</style>
</head>
<body>
<header class="top"><div><span class="brand">PHUX / {label}</span><span class="badge">PROPOSAL · {DRAFT.upper()}</span></div><div class="actions"><a href="DESIGN.html">Overview</a><button id="export" type="button">Export notes as Markdown</button><button id="print" type="button">Print / PDF</button><a href="{stem}.md">Markdown source</a></div></header>
<div class="shell"><nav aria-label="Sections"><p>{nav}</p><div class="toc">
{md.toc}
</div></nav>
<main>
{body}
<p class="footer">Generated from {stem}.md by build.py. No external scripts, fonts, analytics, or network requests. Source SHA-256: <code>{digest}</code>.</p>
</main></div>
<script>
{script}
</script>
</body></html>
"""
    (HERE / f"{stem}.html").write_text(page)
    print(f"wrote {stem}.html")


if __name__ == "__main__":
    for page in PAGES:
        render(*page)
