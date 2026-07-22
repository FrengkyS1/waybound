import type { AnchorHTMLAttributes } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Every `<a>` inside third-party mod/changelog markdown renders through
 * this instead of a plain anchor — a real click would otherwise navigate
 * this window's own main frame to the external URL, replacing the whole
 * app with no way back short of relaunching it. Opens in the system's
 * default browser instead, same as every other external link in the app. */
export function MarkdownLink({ href, children, ...rest }: AnchorHTMLAttributes<HTMLAnchorElement>) {
  return (
    <a
      {...rest}
      href={href}
      onClick={(e) => {
        e.preventDefault();
        if (href) void openUrl(href);
      }}
    >
      {children}
    </a>
  );
}
