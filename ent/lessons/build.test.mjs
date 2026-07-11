import { test } from 'node:test';
import assert from 'node:assert/strict';
import { escapeHtml, escapeAttr, renderInline } from './build.mjs';

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
