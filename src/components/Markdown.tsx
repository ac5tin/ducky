import { memo, useState, type ComponentProps } from "react";
import { Icon } from "./icons";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown, { type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";

type MarkdownProps = ComponentProps<typeof ReactMarkdown>;

// Module-level: fresh plugin/component values on every render make React treat
// `a`/`pre` as new component types, remounting every code block on each token.
const REMARK_PLUGINS: MarkdownProps["remarkPlugins"] = [remarkGfm];
const REHYPE_PLUGINS: MarkdownProps["rehypePlugins"] = [
  [rehypeHighlight, { detect: false, ignoreMissing: true }],
];
const COMPONENTS: MarkdownProps["components"] = {
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
  pre: (props) => <CodeBlock {...props} />,
};

// Memoized on `text` so a streaming message only re-parses itself, never the
// finished messages above it.
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="md-body">
      <ReactMarkdown
        remarkPlugins={REMARK_PLUGINS}
        rehypePlugins={REHYPE_PLUGINS}
        components={COMPONENTS}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});

function CodeBlock({ children, ...rest }: ComponentProps<"pre"> & ExtraProps) {
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
