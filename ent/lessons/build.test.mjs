import { test } from 'node:test';
import assert from 'node:assert/strict';
import { escapeHtml, escapeAttr, renderInline, renderMarkdown, parseLesson, validateGlossary, buildGlossaryIndex, coverageReport } from './build.mjs';

test('escapeHtml escapes angle brackets and ampersands', () => {
  assert.equal(escapeHtml('a < b & c > d'), 'a &lt; b &amp; c &gt; d');
});

test('escapeAttr also escapes double quotes', () => {
  assert.equal(escapeAttr('say "hi" & <go>'), 'say &quot;hi&quot; &amp; &lt;go&gt;');
});

test('renderInline: inline code is escaped and wrapped in <code>', () => {
  assert.equal(renderInline('use `Vec<T>` here'), 'use <code>Vec&lt;T&gt;</code> here');
});

test('renderInline: bold and italic', () => {
  assert.equal(renderInline('**bold** and *soft*'), '<strong>bold</strong> and <em>soft</em>');
});

test('renderInline: content inside inline code is not treated as bold/italic', () => {
  assert.equal(renderInline('`a * b * c`'), '<code>a * b * c</code>');
});

test('renderInline: external link passes through', () => {
  assert.equal(
    renderInline('[View PR](https://example.com/pull/16)'),
    '<a href="https://example.com/pull/16">View PR</a>',
  );
});

test('renderInline: cross-lesson .md link is rewritten to .html', () => {
  assert.equal(
    renderInline('[PR #40](40-serve-https-local-ca.md)'),
    '<a href="40-serve-https-local-ca.html">PR #40</a>',
  );
});

test('renderInline: a link label containing inline code renders the code span', () => {
  assert.equal(
    renderInline('run [`adj install-ca`](40-serve-https-local-ca.md) first'),
    'run <a href="40-serve-https-local-ca.html"><code>adj install-ca</code></a> first',
  );
});

test('renderMarkdown: heading levels', () => {
  assert.equal(renderMarkdown('## The Rust idea'), '<h2>The Rust idea</h2>');
  assert.equal(renderMarkdown('### Where this is headed'), '<h3>Where this is headed</h3>');
});

test('renderMarkdown: paragraph joins wrapped lines and renders inline', () => {
  assert.equal(
    renderMarkdown('Clone an `Arc`\nto share it.'),
    '<p>Clone an <code>Arc</code> to share it.</p>',
  );
});

test('renderMarkdown: fenced code is literal, escaped, not inline-rendered', () => {
  const md = '```rust\nlet x: Vec<u8> = *ptr; // **not bold**\n```';
  assert.equal(
    renderMarkdown(md),
    '<pre><code class="language-rust">let x: Vec&lt;u8&gt; = *ptr; // **not bold**\n</code></pre>',
  );
});

test('renderMarkdown: unordered list with one nested level', () => {
  const md = '- top one\n- top two\n  - nested a';
  assert.equal(
    renderMarkdown(md),
    '<ul><li>top one</li><li>top two<ul><li>nested a</li></ul></li></ul>',
  );
});

test('renderMarkdown: ordered list', () => {
  assert.equal(renderMarkdown('1. first\n2. second'), '<ol><li>first</li><li>second</li></ol>');
});

test('renderMarkdown: a bullet wrapping across lines stays one <li>', () => {
  assert.equal(
    renderMarkdown('- first line\n  wrapped second'),
    '<ul><li>first line wrapped second</li></ul>',
  );
});

test('renderMarkdown: an ordered item wrapping across lines stays one <li>', () => {
  assert.equal(
    renderMarkdown('1. first line\n   wraps here\n2. second'),
    '<ol><li>first line wraps here</li><li>second</li></ol>',
  );
});

test('renderMarkdown: two nested siblings share one nested list', () => {
  assert.equal(
    renderMarkdown('- top\n  - n1\n  - n2'),
    '<ul><li>top<ul><li>n1</li><li>n2</li></ul></li></ul>',
  );
});

test('renderMarkdown: a marker-type change splits into two lists', () => {
  assert.equal(
    renderMarkdown('- a\n1. b'),
    '<ul><li>a</li></ul><ol><li>b</li></ol>',
  );
});

test('renderMarkdown: real wrapped bullets stay one list with no stray paragraphs', () => {
  const md = [
    '- **Root-relative, trailing slash.** The leading `/` resolves from the domain',
    '  root regardless of where the visitor entered. The trailing slash points at the',
    '  *directory* `/ent/`, whose index (`ent/index.html`) the host serves.',
    '- **`rel="canonical"`.** Meta-refresh shells look like duplicate content or a',
    '  sneaky redirect to a crawler.',
  ].join('\n');
  const html = renderMarkdown(md);
  assert.equal((html.match(/<ul>/g) || []).length, 1);
  assert.equal((html.match(/<li>/g) || []).length, 2);
  assert.equal((html.match(/<p>/g) || []).length, 0);
});

const SAMPLE = `<!-- Lesson for PR #16. Teaches one concept. -->

# PR #16 — Tracer: supervised app with logs

> **Rust lesson:** Hand the task an \`Arc<Mutex<T>>\` clone.
> **Tags:** \`Arc<Mutex<T>>\` · \`tokio::spawn\`
> **Merged:** 2026-06-08 · +2369/−0 · [View PR](https://github.com/nonrational/adjacent/pull/16)

## The situation

First paragraph.
`;

test('parseLesson extracts pr, slug, and title', () => {
  const p = parseLesson('16-supervised-app-with-logs.md', SAMPLE);
  assert.equal(p.pr, 16);
  assert.equal(p.slug, 'supervised-app-with-logs');
  assert.equal(p.title, 'PR #16 — Tracer: supervised app with logs');
});

test('parseLesson extracts the lesson sentence and tags', () => {
  const p = parseLesson('16-supervised-app-with-logs.md', SAMPLE);
  assert.equal(p.lesson, 'Hand the task an `Arc<Mutex<T>>` clone.');
  assert.deepEqual(p.tags, ['Arc<Mutex<T>>', 'tokio::spawn']);
});

test('parseLesson extracts merged date, delta, and url', () => {
  const p = parseLesson('16-supervised-app-with-logs.md', SAMPLE);
  assert.equal(p.merged.date, '2026-06-08');
  assert.equal(p.merged.delta, '+2369/−0');
  assert.equal(p.merged.url, 'https://github.com/nonrational/adjacent/pull/16');
});

test('parseLesson body starts after the blockquote header', () => {
  const p = parseLesson('16-supervised-app-with-logs.md', SAMPLE);
  assert.ok(p.bodyMarkdown.startsWith('## The situation'));
  assert.ok(!p.bodyMarkdown.includes('Rust lesson:'));
});

const GLOSSARY = {
  arc: { term: 'Arc<T>', aliases: ['Arc', 'Arc<Mutex<T>>'], short: 's', why: 'w', prereqs: ['ownership'], link: { label: 'x', url: 'y' } },
  ownership: { term: 'ownership', aliases: [], short: 's', why: 'w', prereqs: [], link: { label: 'x', url: 'y' } },
};

test('validateGlossary: clean glossary has no errors', () => {
  assert.deepEqual(validateGlossary(GLOSSARY).errors, []);
});

test('validateGlossary: dangling prereq is an error', () => {
  const bad = { arc: { term: 'Arc<T>', aliases: [], prereqs: ['nope'] } };
  assert.equal(validateGlossary(bad).errors.length, 1);
  assert.match(validateGlossary(bad).errors[0], /nope/);
});

test('validateGlossary: duplicate alias across entries is an error', () => {
  const bad = {
    a: { term: 'A', aliases: ['shared'], prereqs: [] },
    b: { term: 'B', aliases: ['shared'], prereqs: [] },
  };
  assert.match(validateGlossary(bad).errors[0], /shared/);
});

test('buildGlossaryIndex maps every term and alias to its key', () => {
  const idx = buildGlossaryIndex(GLOSSARY);
  assert.equal(idx.get('Arc'), 'arc');
  assert.equal(idx.get('Arc<Mutex<T>>'), 'arc');
  assert.equal(idx.get('Arc<T>'), 'arc');
  assert.equal(idx.get('ownership'), 'ownership');
});

test('renderInline wraps a glossary-matched code span as a term button', () => {
  const idx = buildGlossaryIndex(GLOSSARY);
  assert.equal(
    renderInline('use `Arc` now', { glossaryIndex: idx }),
    'use <button type="button" class="term" data-term="arc"><code>Arc</code></button> now',
  );
});

test('coverageReport lists backticked terms missing from the glossary, not those present', () => {
  const idx = buildGlossaryIndex(GLOSSARY);
  const lessons = [{ bodyMarkdown: 'see `Arc` and `Weak`\n```rust\n`Mutex` in code\n```' }];
  const rep = coverageReport(lessons, idx);
  assert.deepEqual(rep, [{ term: 'Weak', count: 1 }]);
});

import {
  THEME, renderTagChips, renderLessonPage, renderStylesheet, renderGlossaryScript,
} from './build.mjs';

const PARSED = {
  pr: 16, slug: 'supervised-app-with-logs',
  title: 'PR #16 — Tracer: supervised app with logs',
  lesson: 'Hand the task an `Arc<Mutex<T>>` clone.',
  tags: ['Arc<Mutex<T>>', 'tokio::spawn'],
  merged: { date: '2026-06-08', delta: '+2369/−0', url: 'https://github.com/nonrational/adjacent/pull/16' },
  bodyMarkdown: '## The situation\n\nBoot it with an `Arc`.',
};
const IDX = buildGlossaryIndex({
  arc: { term: 'Arc<T>', aliases: ['Arc', 'Arc<Mutex<T>>'], prereqs: [] },
});

test('renderTagChips: a glossary tag becomes a term button, others plain', () => {
  const html = renderTagChips(['Arc<Mutex<T>>', 'tokio::spawn'], IDX);
  assert.match(html, /data-term="arc"[^>]*>[^<]*<code>Arc&lt;Mutex&lt;T&gt;&gt;<\/code>/);
  assert.match(html, /tokio::spawn/);
});

test('renderLessonPage: full document with provenance header and asset links', () => {
  const html = renderLessonPage(PARSED, { glossaryIndex: IDX });
  assert.ok(html.startsWith('<!-- generated by build.mjs'));
  assert.match(html, /<!DOCTYPE html>/);
  assert.match(html, /<link rel="stylesheet" href="lessons\.css">/);
  assert.match(html, /<script src="glossary\.js"><\/script>/);
  assert.match(html, /<script src="drawer\.js" defer><\/script>/);
  assert.match(html, /PR #16 — Tracer: supervised app with logs/);
  // A body code span that matches the glossary is annotated.
  assert.match(html, /data-term="arc"><code>Arc<\/code>/);
});

test('renderStylesheet: emits the theme accent as a CSS custom property', () => {
  assert.match(renderStylesheet(), /--accent:\s*#d4a574/);
});

test('renderGlossaryScript: assigns the glossary to a global', () => {
  const js = renderGlossaryScript({ arc: { term: 'Arc<T>' } });
  assert.match(js, /window\.__GLOSSARY__ = /);
  assert.match(js, /"Arc<T>"/);
});

import { renderDrawerScript } from './build.mjs';

test('renderDrawerScript emits provenance header and wires term clicks', () => {
  const js = renderDrawerScript();
  assert.ok(js.startsWith('/* generated by build.mjs'));
  assert.match(js, /addEventListener\('click'/);
  assert.match(js, /__GLOSSARY__/);
  assert.match(js, /Escape/);
});

import { parseReadmeIndex, renderIndexPage } from './build.mjs';

const README = `# Rust, one PR at a time

Intro prose.

## The lessons

| PR | Lesson | The one-line takeaway |
|----|--------|-----------------------|
| [#16](16-supervised-app-with-logs.md) | \`Arc<Mutex<T>>\` + \`tokio::spawn\` | Clone an \`Arc\` to share. |
| [#34](34-fix-landing-page-redirect.md) ◦ | Client-side redirects | How a meta refresh works. |

◦ = little or no Rust.

## Reading paths by theme

- **Error handling** — [#29](29-deadlines-and-failure-enums.md) → [#48](48-warn-dont-swallow-errors.md)
`;

test('parseReadmeIndex extracts rows including the sparse marker', () => {
  const { rows } = parseReadmeIndex(README);
  assert.equal(rows.length, 2);
  assert.equal(rows[0].pr, 16);
  assert.equal(rows[0].href, '16-supervised-app-with-logs.html');
  assert.equal(rows[0].sparse, false);
  assert.equal(rows[1].sparse, true);
});

test('parseReadmeIndex extracts reading paths', () => {
  const { paths } = parseReadmeIndex(README);
  assert.equal(paths.length, 1);
  assert.match(paths[0].theme, /Error handling/);
  assert.match(paths[0].html, /29-deadlines-and-failure-enums\.html/);
});

test('renderIndexPage produces a document linking to lesson pages', () => {
  const html = renderIndexPage(README);
  assert.ok(html.startsWith('<!-- generated by build.mjs'));
  assert.match(html, /href="16-supervised-app-with-logs\.html"/);
  assert.match(html, /<link rel="stylesheet" href="lessons\.css">/);
});

test('parseReadmeIndex unescapes table-cell pipe escapes in takeaways', () => {
  const md = [
    '## The lessons',
    '',
    '| PR | Lesson | Takeaway |',
    '|--|--|--|',
    '| [#48](48-warn-dont-swallow-errors.md) | Errors | use `.unwrap_or_else(\|err\|)` first |',
  ].join('\n');
  const { rows } = parseReadmeIndex(md);
  assert.equal(rows[0].takeaway, 'use `.unwrap_or_else(|err|)` first');
});
