import ReactMarkdown from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import type { BodyFormat } from "../detailTypes";
import { MarkdownLink } from "./MarkdownLink";
import styles from "./MarkdownBody.module.css";

// Mod/modpack descriptions are third-party HTML/markdown from CurseForge and
// Modrinth — untrusted input. rehype-sanitize strips scripts, event handler
// attributes (onerror, onload, ...), and javascript: URLs before render.

interface OverviewBodyProps {
  content: string;
  bodyFormat: BodyFormat;
}

const HTML_TAG =
  /<(?:center|details|summary|img|div|span|br|p|a|h[1-6]|ul|ol|li|table|iframe|video)\b/i;

function normalizeContent(content: string): string {
  return content
    .replace(
      /https:\/\/imgur\.com\/([A-Za-z0-9]+)(\.[a-z]+)?/gi,
      "https://i.imgur.com/$1$2",
    )
    .replace(
      /http:\/\/imgur\.com\/([A-Za-z0-9]+)(\.[a-z]+)?/gi,
      "https://i.imgur.com/$1$2",
    );
}

function shouldAllowHtml(content: string, bodyFormat: BodyFormat): boolean {
  return bodyFormat === "html" || HTML_TAG.test(content);
}

export function OverviewBody({ content, bodyFormat }: OverviewBodyProps) {
  const normalized = normalizeContent(content);
  const allowHtml = shouldAllowHtml(normalized, bodyFormat);

  return (
    <div className={`${styles.prose} ${allowHtml ? styles.richHtml : ""}`}>
      <MarkdownBodyInner content={normalized} allowHtml={allowHtml} />
    </div>
  );
}

function MarkdownBodyInner({
  content,
  allowHtml,
}: {
  content: string;
  allowHtml: boolean;
}) {
  const rehypePlugins = allowHtml ? [rehypeRaw, rehypeSanitize] : [];

  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={rehypePlugins} components={{ a: MarkdownLink }}>
      {content}
    </ReactMarkdown>
  );
}
