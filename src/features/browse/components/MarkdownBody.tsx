import rehypeRaw from "rehype-raw";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import styles from "./MarkdownBody.module.css";

interface MarkdownBodyProps {
  content: string;
  allowHtml?: boolean;
}

export function MarkdownBody({
  content,
  allowHtml = false,
}: MarkdownBodyProps) {
  const rehypePlugins = allowHtml ? [rehypeRaw] : [];

  return (
    <div className={styles.prose}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={rehypePlugins}>
        {content}
      </ReactMarkdown>
    </div>
  );
}
