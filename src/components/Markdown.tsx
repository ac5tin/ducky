import { useState, type ReactNode } from "react";
import { Icon } from "./icons";
import { openUrl } from "@tauri-apps/plugin-opener";

export function Markdown({ text }: { text: string }) {
  // react-markdown is imported lazily via static import at module level in
  // MarkdownInner to keep this file lean.
  return <MarkdownInner text={text} />;
}

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";

function MarkdownInner({ text }: { text: string }) {
  return (
    <div className="md-body">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: false, ignoreMissing: true }]]}
        components={{
          a: (props) => (
            <a
              {...props}
              onClick={(e) => {
                e.preventDefault();
                const href = props.href ?? "";
                if (href.startsWith("http")) openUrl(href).catch(() => {});
              }}
            />
          ),
          pre: (props) => <CodeBlock {...(props as any)} />,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

function CodeBlock({ children, ...rest }: { children?: ReactNode } & Record<string, any>) {
  const [copied, setCopied] = useState(false);
  const code = extractText(children);
  return (
    <div className="group relative">
      <button
        className="absolute right-2 top-2 rounded-md bg-white/10 p-1.5 text-slate-300 opacity-0 transition hover:bg-white/20 group-hover:opacity-100"
        aria-label="Copy code"
        onClick={() => {
          navigator.clipboard.writeText(code).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        <Icon name={copied ? "check" : "copy"} className="h-4 w-4" />
      </button>
      <pre {...rest}>{children}</pre>
    </div>
  );
}

function extractText(node: any): string {
  if (node == null) return "";
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (typeof node === "object" && node.props) return extractText(node.props.children);
  return "";
}
