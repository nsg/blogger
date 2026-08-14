export type EditableImage = {
  alt: string;
  path: string;
  invert: boolean;
};

const MARKDOWN_IMAGE_RE = /^!\[([^\]]*)\]\(([^)]+)\)\s*$/;
const INVERT_IMAGE_RE = /^\{\{\s*image\(\s*src\s*=\s*("(?:\\.|[^"\\])*")\s*(?:,\s*alt\s*=\s*("(?:\\.|[^"\\])*")\s*)?,\s*invert\s*=\s*true\s*\)\s*\}\}\s*$/;

function parseQuoted(value: string | undefined): string | null {
  if (value === undefined) return "";
  try {
    const parsed: unknown = JSON.parse(value);
    return typeof parsed === "string" ? parsed : null;
  } catch {
    return null;
  }
}

export function parseImageLine(line: string): EditableImage | null {
  const markdown = line.match(MARKDOWN_IMAGE_RE);
  if (markdown) {
    return { alt: markdown[1], path: markdown[2], invert: false };
  }

  const shortcode = line.match(INVERT_IMAGE_RE);
  if (!shortcode) return null;
  const path = parseQuoted(shortcode[1]);
  const alt = parseQuoted(shortcode[2]);
  return path === null || alt === null ? null : { alt, path, invert: true };
}

export function formatImageLine(image: EditableImage): string {
  if (!image.invert) return `![${image.alt}](${image.path})`;

  const args = [`src=${JSON.stringify(image.path)}`];
  if (image.alt) args.push(`alt=${JSON.stringify(image.alt)}`);
  args.push("invert=true");
  return `{{ image(${args.join(", ")}) }}`;
}
