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
