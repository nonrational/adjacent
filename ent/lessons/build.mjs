// Dependency-free generator for the interactive Rust lessons site.
// Reads docs/lessons/*.md (source of truth) + docs/lessons/glossary.json,
// emits static HTML + shared assets into ent/lessons/. Run from repo root.

// Placeholder sentinel: a NUL char, built at runtime so no literal NUL (which
// would make git treat this file as binary) ever sits in the source. NUL never
// appears in lesson prose, so restoring placeholders can't collide with text.
const NUL = String.fromCharCode(0);
const PLACEHOLDER = new RegExp(NUL + '(\\d+)' + NUL, 'g');

const emphasize = (str) =>
  str.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>').replace(/\*([^*]+)\*/g, '<em>$1</em>');

export function escapeHtml(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function escapeAttr(s) {
  return escapeHtml(s).replace(/"/g, '&quot;');
}

// Render one run of inline markdown. Inline code is extracted first so its
// contents are never reinterpreted as emphasis; links are extracted before
// escaping so their hrefs stay clean. A glossary index (alias -> key), when
// supplied, turns a matching code span into a clickable term button (Task 4).
export function renderInline(text, { glossaryIndex } = {}) {
  const slots = [];
  const stash = (html) => NUL + (slots.push(html) - 1) + NUL;

  let s = String(text).replace(/`([^`]+)`/g, (_, code) => {
    const inner = `<code>${escapeHtml(code)}</code>`;
    const key = glossaryIndex && glossaryIndex.get(code);
    return stash(
      key
        ? `<button type="button" class="term" data-term="${escapeAttr(key)}">${inner}</button>`
        : inner,
    );
  });

  s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, label, href) => {
    const h = /^\d+-[a-z0-9-]+\.md$/.test(href) ? href.replace(/\.md$/, '.html') : href;
    // The label may already hold code placeholders from the pass above; escape
    // its plain text and apply emphasis, leaving placeholders for the final
    // restore. One shared slots array keeps nested code spans resolvable.
    const inner = emphasize(escapeHtml(label));
    return stash(`<a href="${escapeAttr(h)}">${inner}</a>`);
  });

  s = emphasize(escapeHtml(s));

  // Restore repeatedly: a stashed link can itself contain a code placeholder,
  // and a single global replace does not re-scan the text it just inserted.
  while (PLACEHOLDER.test(s)) {
    PLACEHOLDER.lastIndex = 0;
    s = s.replace(PLACEHOLDER, (_, i) => slots[Number(i)]);
  }
  return s;
}

// Block-level renderer for the bounded markdown the lessons use: ATX headings,
// paragraphs, fenced code (literal), and unordered/ordered lists with one
// nesting level. Blockquotes are handled by parseLesson (the header) and do
// not appear in bodies, so they are intentionally unsupported here.
export function renderMarkdown(body, { glossaryIndex } = {}) {
  const lines = String(body).replace(/\r\n/g, '\n').split('\n');
  const out = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (/^```/.test(line)) {
      const lang = line.slice(3).trim();
      const buf = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i])) buf.push(lines[i++]);
      i++; // consume closing fence
      const cls = lang ? ` class="language-${escapeAttr(lang)}"` : '';
      out.push(`<pre><code${cls}>${escapeHtml(buf.join('\n') + '\n')}</code></pre>`);
      continue;
    }

    const heading = line.match(/^(#{1,3})\s+(.*)$/);
    if (heading) {
      const level = heading[1].length;
      out.push(`<h${level}>${renderInline(heading[2].trim(), { glossaryIndex })}</h${level}>`);
      i++;
      continue;
    }

    if (/^\s*([-*]|\d+\.)\s+/.test(line)) {
      const { html, next } = parseList(lines, i, glossaryIndex);
      out.push(html);
      i = next;
      continue;
    }

    if (line.trim() === '') {
      i++;
      continue;
    }

    const para = [];
    while (
      i < lines.length &&
      lines[i].trim() !== '' &&
      !/^(#{1,3}\s|```|\s*([-*]|\d+\.)\s)/.test(lines[i])
    ) {
      para.push(lines[i]);
      i++;
    }
    out.push(`<p>${renderInline(para.join(' ').replace(/\s+/g, ' ').trim(), { glossaryIndex })}</p>`);
  }

  return out.join('');
}

// Parse one list (plus one level of nested sub-lists) starting at line `i`.
// Item text accumulates lazy-continuation lines (wrapped prose) the way the
// paragraph branch does, so a bullet spanning several source lines stays a
// single <li>. A marker indented deeper than this list's base opens a nested
// list under the current item; a shallower marker, a blank line, a block
// start, or a marker-type change ends the list. Returns the HTML and the
// index just past the list.
function parseList(lines, i, glossaryIndex) {
  const first = lines[i].match(/^(\s*)([-*]|\d+\.)\s+/);
  const baseIndent = first[1].length;
  const ordered = /\d+\./.test(first[2]);
  const tag = ordered ? 'ol' : 'ul';
  const items = [];

  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === '') break;              // blank line ends the list
    if (/^(#{1,3}\s|```)/.test(line)) break;    // a block start ends the list
    const m = line.match(/^(\s*)([-*]|\d+\.)\s+(.*)$/);
    if (m) {
      const indent = m[1].length;
      if (indent > baseIndent) {                // deeper: nested list under last item
        const sub = parseList(lines, i, glossaryIndex);
        if (items.length) items[items.length - 1].children += sub.html;
        i = sub.next;
        continue;
      }
      if (indent < baseIndent) break;           // dedent: this list is done
      if (/\d+\./.test(m[2]) !== ordered) break; // marker type change ends the list
      items.push({ text: m[3], children: '' });
      i++;
      continue;
    }
    if (items.length) {                          // lazy continuation of current item
      items[items.length - 1].text += ` ${line.trim()}`;
      i++;
      continue;
    }
    break;
  }

  const body = items
    .map(
      (it) =>
        `<li>${renderInline(it.text.replace(/\s+/g, ' ').trim(), { glossaryIndex })}${it.children}</li>`,
    )
    .join('');
  return { html: `<${tag}>${body}</${tag}>`, next: i };
}

// Parse a lesson file of the fixed template shape into structured parts.
export function parseLesson(filename, md) {
  const fm = filename.match(/^(\d+)-([a-z0-9-]+)\.md$/);
  if (!fm) throw new Error(`not a lesson filename: ${filename}`);
  const pr = Number(fm[1]);
  const slug = fm[2];

  let rest = String(md).replace(/^﻿/, '').replace(/^<!--[\s\S]*?-->\s*/, '');

  const titleMatch = rest.match(/^#\s+(.*)\n?/);
  const title = titleMatch ? titleMatch[1].trim() : '';
  if (titleMatch) rest = rest.slice(titleMatch[0].length);

  const lines = rest.split('\n');
  let i = 0;
  while (i < lines.length && lines[i].trim() === '') i++;
  const quote = [];
  while (i < lines.length && lines[i].startsWith('>')) {
    quote.push(lines[i].replace(/^>\s?/, ''));
    i++;
  }
  const bodyMarkdown = lines.slice(i).join('\n').trim();

  const field = (label) => (quote.find((l) => l.startsWith(`**${label}:**`)) || '')
    .replace(new RegExp(`^\\*\\*${label}:\\*\\*\\s*`), '')
    .trim();

  const lesson = field('Rust lesson');
  const tags = field('Tags').split('·').map((t) => t.replace(/`/g, '').trim()).filter(Boolean);
  const mergedRaw = field('Merged');
  const mm = mergedRaw.match(/^([0-9-]+)\s*·\s*(.+?)\s*·\s*\[[^\]]*\]\(([^)]+)\)/);
  const merged = mm
    ? { date: mm[1].trim(), delta: mm[2].trim(), url: mm[3].trim() }
    : { date: '', delta: '', url: '' };

  return { pr, slug, title, lesson, tags, merged, bodyMarkdown };
}

// Every prereq must resolve to a real entry; every alias/term must be unique
// across entries (an ambiguous alias could not annotate deterministically).
export function validateGlossary(glossary) {
  const errors = [];
  const warnings = [];
  const keys = new Set(Object.keys(glossary));
  const seen = new Map();
  for (const [key, e] of Object.entries(glossary)) {
    for (const p of e.prereqs || []) {
      if (!keys.has(p)) errors.push(`entry '${key}': prereq '${p}' has no glossary entry`);
    }
    for (const name of [e.term, ...(e.aliases || [])]) {
      if (!name) continue;
      if (seen.has(name) && seen.get(name) !== key) {
        errors.push(`alias '${name}' maps to both '${seen.get(name)}' and '${key}'`);
      } else {
        seen.set(name, key);
      }
    }
  }
  return { errors, warnings };
}

export function buildGlossaryIndex(glossary) {
  const idx = new Map();
  for (const [key, e] of Object.entries(glossary)) {
    for (const name of [e.term, ...(e.aliases || [])]) {
      if (name) idx.set(name, key);
    }
  }
  return idx;
}

export function coverageReport(lessons, glossaryIndex) {
  const counts = new Map();
  for (const l of lessons) {
    const noFences = String(l.bodyMarkdown).replace(/```[\s\S]*?```/g, '');
    for (const m of noFences.matchAll(/`([^`]+)`/g)) {
      const t = m[1];
      if (!glossaryIndex.has(t)) counts.set(t, (counts.get(t) || 0) + 1);
    }
  }
  return [...counts.entries()]
    .map(([term, count]) => ({ term, count }))
    .sort((a, b) => b.count - a.count || a.term.localeCompare(b.term));
}

// ---- Theme + config (the repo-specific seam; swap this to retarget) ----
export const THEME = {
  name: 'adjacent',
  palette: {
    ink: '#0a0a0a', paper: '#ededea', soft: '#b5b5af', dim: '#7a7a74',
    rule: '#1f1f1d', accent: '#d4a574', accentDim: '#8a6a44',
  },
  fontLinks: [
    '<link rel="preconnect" href="https://fonts.googleapis.com">',
    '<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>',
    '<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;700&display=swap" rel="stylesheet">',
  ].join('\n  '),
  fontStack: "'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace",
  wordmark: { a: 'adj.ac', slash: '/', b: 'ent' },
  siteTitle: 'adj.ac/ent — Rust, one PR at a time',
  repoUrl: 'https://github.com/nonrational/adjacent',
  paths: { lessonsDir: 'docs/lessons', readme: 'docs/lessons/README.md', outDir: 'ent/lessons' },
};

const HTML_HEADER = '<!-- generated by build.mjs; edit docs/lessons/*.md -->';
const ASSET_HEADER = '/* generated by build.mjs; edit docs/lessons/*.md */';

export function renderTagChips(tags, glossaryIndex) {
  return tags
    .map((t) => {
      const key = glossaryIndex.get(t);
      const code = `<code>${escapeHtml(t)}</code>`;
      return key
        ? `<button type="button" class="term tag" data-term="${escapeAttr(key)}">${code}</button>`
        : `<span class="tag">${code}</span>`;
    })
    .join('');
}

export function renderLessonPage(parsed, { glossaryIndex, theme = THEME }) {
  const { a, slash, b } = theme.wordmark;
  const body = renderMarkdown(parsed.bodyMarkdown, { glossaryIndex });
  const lesson = renderInline(parsed.lesson, { glossaryIndex });
  const chips = renderTagChips(parsed.tags, glossaryIndex);
  const prLink = parsed.merged.url
    ? `<a href="${escapeAttr(parsed.merged.url)}">View PR</a>`
    : '';
  return `${HTML_HEADER}
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${escapeHtml(parsed.title)}</title>
  ${theme.fontLinks}
  <link rel="stylesheet" href="lessons.css">
</head>
<body>
  <div class="frame">
    <header>
      <a class="wordmark" href="index.html"><span class="a">${a}</span><span class="slash">${slash}</span><span class="b">${b}</span></a>
      <a class="back" href="index.html">all lessons</a>
    </header>
    <article>
      <h1>${escapeHtml(parsed.title)}</h1>
      <div class="lesson-header">
        <p class="lesson">${lesson}</p>
        <p class="tags">${chips}</p>
        <p class="meta">${escapeHtml(parsed.merged.date)} · ${escapeHtml(parsed.merged.delta)} · ${prLink}</p>
      </div>
      ${body}
    </article>
  </div>
  <script src="glossary.js"></script>
  <script src="drawer.js" defer></script>
</body>
</html>
`;
}

export function renderStylesheet(theme = THEME) {
  const p = theme.palette;
  return `${ASSET_HEADER}
:root {
  --ink: ${p.ink}; --paper: ${p.paper}; --soft: ${p.soft}; --dim: ${p.dim};
  --rule: ${p.rule}; --accent: ${p.accent}; --accent-dim: ${p.accentDim};
}
* { box-sizing: border-box; margin: 0; padding: 0; }
html, body { background: var(--ink); color: var(--paper); font-family: ${theme.fontStack}; font-size: 15px; line-height: 1.65; -webkit-font-smoothing: antialiased; }
body { min-height: 100vh; padding: 3rem clamp(1.25rem, 6vw, 5rem); }
.frame { max-width: 760px; margin: 0 auto; }
header { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 3rem; }
.wordmark { font-weight: 700; font-size: 1.1rem; letter-spacing: -0.03em; text-decoration: none; }
.wordmark .a { color: var(--paper); } .wordmark .b { color: var(--accent); } .wordmark .slash { color: var(--rule); font-weight: 300; margin: 0 0.1em; }
.back { font-size: 12px; color: var(--dim); text-transform: uppercase; letter-spacing: 0.1em; text-decoration: none; border-bottom: 1px dotted var(--dim); }
.back:hover { color: var(--accent); border-color: var(--accent); }
article h1 { font-size: clamp(1.6rem, 4vw, 2.3rem); font-weight: 700; letter-spacing: -0.02em; line-height: 1.15; margin-bottom: 1.5rem; }
.lesson-header { border-left: 2px solid var(--accent-dim); padding-left: 1.1rem; margin-bottom: 2.5rem; }
.lesson-header .lesson { font-size: 1.15rem; font-weight: 300; margin-bottom: 0.75rem; }
.lesson-header .tags { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 0.6rem; }
.lesson-header .meta { font-size: 12px; color: var(--dim); letter-spacing: 0.04em; }
article h2 { font-size: 1.25rem; margin: 2.5rem 0 1rem; padding-top: 1.25rem; border-top: 1px solid var(--rule); }
article h3 { font-size: 1.05rem; margin: 1.75rem 0 0.75rem; }
article p { margin-bottom: 1rem; color: var(--paper); }
article ul, article ol { margin: 0 0 1rem 1.4rem; }
article li { margin-bottom: 0.35rem; }
article a { color: var(--paper); text-decoration: none; border-bottom: 1px solid var(--accent-dim); }
article a:hover { color: var(--accent); border-color: var(--accent); }
code { font-family: inherit; color: var(--accent); font-size: 0.95em; }
pre { background: rgba(255,255,255,0.02); border: 1px solid var(--rule); border-left: 2px solid var(--accent-dim); padding: 1rem 1.15rem; margin-bottom: 1.25rem; overflow-x: auto; }
pre code { color: var(--paper); font-size: 13px; line-height: 1.5; }
.term { font: inherit; background: none; border: none; padding: 0; cursor: pointer; border-bottom: 1px dotted var(--accent); }
.term:hover, .term:focus-visible { background: rgba(212,165,116,0.1); }
.tag code { font-size: 0.85em; }
span.tag { border-bottom: 1px dotted var(--rule); }
table.index { width: 100%; border-collapse: collapse; margin-bottom: 2.5rem; font-size: 0.9rem; }
table.index td { border-bottom: 1px solid var(--rule); padding: 0.6rem 0.75rem 0.6rem 0; vertical-align: top; }
table.index td.pr { white-space: nowrap; }
table.index td.pr a { color: var(--accent); border: none; }
table.index td.takeaway { color: var(--soft); }
table.index tr.sparse td { color: var(--dim); }
table.index .mark { color: var(--accent-dim); }
/* Drawer (built by drawer.js) */
.drawer-scrim { position: fixed; inset: 0; background: rgba(0,0,0,0.4); opacity: 0; transition: opacity 0.15s; z-index: 10; }
.drawer-scrim.open { opacity: 1; }
.drawer { position: fixed; top: 0; right: 0; height: 100%; width: min(360px, 90vw); background: var(--ink); border-left: 1px solid var(--rule); transform: translateX(100%); transition: transform 0.18s ease-out; z-index: 11; padding: 1.75rem 1.5rem; overflow-y: auto; }
.drawer.open { transform: none; }
.drawer .close { position: absolute; top: 1rem; right: 1.1rem; background: none; border: none; color: var(--dim); font: inherit; font-size: 1.2rem; cursor: pointer; }
.drawer .close:hover { color: var(--accent); }
.drawer .back-link { background: none; border: none; color: var(--dim); font: inherit; font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase; cursor: pointer; margin-bottom: 1rem; display: none; }
.drawer.has-history .back-link { display: inline-block; }
.drawer .d-term { color: var(--accent); font-weight: 700; font-size: 1.1rem; margin-bottom: 0.9rem; word-break: break-word; }
.drawer .d-label { font-size: 11px; letter-spacing: 0.16em; text-transform: uppercase; color: var(--dim); margin: 1.2rem 0 0.4rem; }
.drawer .d-short, .drawer .d-why { color: var(--paper); font-size: 0.95rem; }
.drawer .d-prereqs { display: flex; flex-wrap: wrap; gap: 0.4rem; }
.drawer .d-prereqs .term { border: 1px solid var(--accent-dim); border-radius: 2px; padding: 0.2rem 0.55rem; }
.drawer .d-link a { color: var(--accent); font-size: 0.9rem; }
@media (max-width: 540px) { body { padding: 2rem 1.1rem; } }
`;
}

export function renderGlossaryScript(glossary) {
  return `${ASSET_HEADER}\nwindow.__GLOSSARY__ = ${JSON.stringify(glossary, null, 0)};\n`;
}

// Browser-side drawer. Built as a string so the generator can emit it verbatim.
// Progressive enhancement: with JS off, .term buttons are inert and the lesson
// stays fully readable.
const DRAWER_JS = String.raw`(() => {
  const G = window.__GLOSSARY__ || {};
  const scrim = document.createElement('div');
  scrim.className = 'drawer-scrim';
  const drawer = document.createElement('aside');
  drawer.className = 'drawer';
  drawer.setAttribute('role', 'dialog');
  drawer.setAttribute('aria-label', 'Term explanation');
  drawer.innerHTML =
    '<button class="close" aria-label="Close">×</button>' +
    '<button class="back-link">← back</button>' +
    '<div class="d-body"></div>';
  document.body.append(scrim, drawer);
  const body = drawer.querySelector('.d-body');
  // Escapes text and attribute values alike (includes ") so a quote in a glossary key or URL can't break out of an attribute.
  const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  let history = [];

  function render(key) {
    const e = G[key];
    if (!e) return;
    const prereqs = (e.prereqs || [])
      .filter((k) => G[k])
      .map((k) => '<button type="button" class="term" data-term="' + esc(k) + '"><code>' + esc(G[k].term) + '</code></button>')
      .join('');
    const link = e.link ? '<p class="d-label">Dig deeper</p><p class="d-link"><a href="' + esc(e.link.url) + '" target="_blank" rel="noopener">' + esc(e.link.label) + '</a></p>' : '';
    body.innerHTML =
      '<p class="d-term">' + esc(e.term) + '</p>' +
      '<p class="d-short">' + esc(e.short || '') + '</p>' +
      (e.why ? '<p class="d-label">Why it matters</p><p class="d-why">' + esc(e.why) + '</p>' : '') +
      (prereqs ? '<p class="d-label">Learn first</p><div class="d-prereqs">' + prereqs + '</div>' : '') +
      link;
    // Back is shown only once we've descended past the root term.
    drawer.classList.toggle('has-history', history.length > 1);
  }

  function open(key) {
    history = [key];
    render(key);
    scrim.classList.add('open');
    drawer.classList.add('open');
  }
  function close() {
    scrim.classList.remove('open');
    drawer.classList.remove('open');
    history = [];
    drawer.classList.remove('has-history');
  }

  document.addEventListener('click', (ev) => {
    const term = ev.target.closest('.term');
    if (term && term.dataset.term) {
      ev.preventDefault();
      if (ev.target.closest('.drawer')) {
        history.push(term.dataset.term); // descend into a prereq
        render(term.dataset.term);
      } else {
        open(term.dataset.term);         // open from the lesson prose
      }
      return;
    }
    if (ev.target.closest('.drawer .close')) { close(); }
  });
  drawer.querySelector('.back-link').addEventListener('click', () => {
    history.pop();
    const prev = history[history.length - 1];
    if (prev) { render(prev); } else { close(); }
  });
  scrim.addEventListener('click', close);
  document.addEventListener('keydown', (ev) => { if (ev.key === 'Escape') close(); });
})();
`;

export function renderDrawerScript() {
  return `${ASSET_HEADER}\n${DRAWER_JS}`;
}

export function parseReadmeIndex(readmeMd) {
  const rows = [];
  const rowRe = /^\|\s*\[#(\d+)\]\(([^)]+)\)\s*(◦)?\s*\|\s*(.+?)\s*\|\s*(.+?)\s*\|$/gm;
  for (const m of readmeMd.matchAll(rowRe)) {
    rows.push({
      pr: Number(m[1]),
      href: m[2].replace(/\.md$/, '.html'),
      sparse: Boolean(m[3]),
      lesson: m[4],
      takeaway: m[5],
    });
  }
  const paths = [];
  const pathRe = /^-\s+\*\*(.+?)\*\*\s+—\s+(.+)$/gm;
  for (const m of readmeMd.matchAll(pathRe)) {
    paths.push({ theme: m[1], html: renderInline(m[2]) });
  }
  return { rows, paths };
}

export function renderIndexPage(readmeMd, { theme = THEME } = {}) {
  const { rows, paths } = parseReadmeIndex(readmeMd);
  const { a, slash, b } = theme.wordmark;
  const rowHtml = rows
    .map((r) => `<tr${r.sparse ? ' class="sparse"' : ''}><td class="pr"><a href="${escapeAttr(r.href)}">#${r.pr}</a>${r.sparse ? ' <span class="mark">◦</span>' : ''}</td><td>${renderInline(r.lesson)}</td><td class="takeaway">${renderInline(r.takeaway)}</td></tr>`)
    .join('\n      ');
  const pathHtml = paths
    .map((p) => `<li><strong>${escapeHtml(p.theme)}</strong> — ${p.html}</li>`)
    .join('\n      ');
  return `${HTML_HEADER}
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>${escapeHtml(theme.siteTitle)}</title>
  ${theme.fontLinks}
  <link rel="stylesheet" href="lessons.css">
</head>
<body>
  <div class="frame">
    <header>
      <a class="wordmark" href="${escapeAttr(theme.repoUrl)}"><span class="a">${a}</span><span class="slash">${slash}</span><span class="b">${b}</span></a>
      <span class="back">Rust, one PR at a time</span>
    </header>
    <article>
      <h1>Rust, one PR at a time</h1>
      <table class="index">
      ${rowHtml}
      </table>
      <h2>Reading paths by theme</h2>
      <ul>
      ${pathHtml}
      </ul>
    </article>
  </div>
</body>
</html>
`;
}
