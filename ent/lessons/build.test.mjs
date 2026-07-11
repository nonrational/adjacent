import { test } from 'node:test';
import assert from 'node:assert/strict';
import { escapeHtml, escapeAttr, renderInline, renderMarkdown } from './build.mjs';

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
