import assert from "node:assert/strict";
import test from "node:test";

import { formatImageLine, parseImageLine } from "../dist/image-syntax.js";

test("converts a Markdown image to the invert shortcode", () => {
  const parsed = parseImageLine("![A description](/images/2026/example.png)");

  assert.deepEqual(parsed, {
    alt: "A description",
    path: "/images/2026/example.png",
    invert: false,
  });
  assert.equal(
    formatImageLine({ ...parsed, invert: true }),
    '{{ image(src="/images/2026/example.png", alt="A description", invert=true) }}',
  );
});

test("converts the invert shortcode back to Markdown", () => {
  const parsed = parseImageLine(
    '{{ image(src="/images/2026/example.png", alt="A description", invert=true) }}',
  );

  assert.deepEqual(parsed, {
    alt: "A description",
    path: "/images/2026/example.png",
    invert: true,
  });
  assert.equal(
    formatImageLine({ ...parsed, invert: false }),
    "![A description](/images/2026/example.png)",
  );
});

test("recognizes the site's existing shortcode without alt text", () => {
  assert.deepEqual(
    parseImageLine('{{ image(src="/images/2024/pixel-upload.png", invert=true) }}'),
    { alt: "", path: "/images/2024/pixel-upload.png", invert: true },
  );
});

test("round-trips quotes in alt text", () => {
  const shortcode = formatImageLine({
    alt: 'The "after" state',
    path: "/images/2026/example.png",
    invert: true,
  });

  assert.deepEqual(parseImageLine(shortcode), {
    alt: 'The "after" state',
    path: "/images/2026/example.png",
    invert: true,
  });
});

test("ignores inline images and unrelated shortcodes", () => {
  assert.equal(parseImageLine("Text ![inline](/images/example.png)"), null);
  assert.equal(parseImageLine('{{ video(src="/images/example.png") }}'), null);
});
