import { useCallback, useMemo, useState } from "react";
import { cn, FOCUS_RING } from "@/lib/utils";
import { ChevronRight, File as FileIcon } from "lucide-react";
import {
  buildTree,
  countFiles,
  type FileEntry,
  type FileStat,
  type TreeNode,
} from "@/components/file-tree";

/**
 * The file tree beside a diff.
 *
 * A branch touching twenty files is unreadable as one long scroll, so this is
 * the navigation for it.
 *
 * Each row carries whatever the group it belongs to is counting, already
 * decided by the caller, together with the sentence that says so. The tree
 * used to render the file's whole diff stat regardless of the heading above
 * it, so under "Unmerged lines" a file's entire history read as its unmerged
 * count.
 *
 * The shape-building lives in `file-tree.ts`, where it can be asserted on.
 */

export type { FileEntry, FileStat, TreeNode };
export { buildTree };

export function FileTree({
  files,
  activePath,
  onSelect,
  heading,
}: {
  files: FileEntry[];
  activePath: string | null;
  onSelect: (path: string) => void;
  /** Names this group when the diff is split into more than one. */
  heading?: string;
}) {
  const tree = useMemo(() => buildTree(files), [files]);

  // Collapsed rather than expanded, so the set is empty for the common case and
  // a new diff starts fully open.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const toggle = useCallback((path: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  }, []);

  // Scrolling belongs to the container, so several trees share one scrollbar
  // instead of each getting its own.
  //
  // The heading sticks at the same offset and stands at the same height as the
  // group heading in the diff pane beside it, so the two stay level with each
  // other however either is scrolled.
  return (
    <nav aria-label={heading ? `${heading} files` : "Changed files"}>
      <p className="sticky top-0 z-10 flex h-7 items-center border-b border-border bg-card px-3 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
        <span className="truncate">
          {heading ? `${heading} · ` : ""}
          {files.length} {files.length === 1 ? "file" : "files"}
        </span>
      </p>
      <ul className="pb-2">
        {tree.map((node) => (
          <Node
            key={node.path}
            node={node}
            depth={0}
            activePath={activePath}
            onSelect={onSelect}
            collapsed={collapsed}
            onToggle={toggle}
          />
        ))}
      </ul>
    </nav>
  );
}

function Node({
  node,
  depth,
  activePath,
  onSelect,
  collapsed,
  onToggle,
}: {
  node: TreeNode;
  depth: number;
  activePath: string | null;
  onSelect: (path: string) => void;
  collapsed: Set<string>;
  onToggle: (path: string) => void;
}) {
  const indent = { paddingLeft: 8 + depth * 12 };

  if (node.file) {
    const active = activePath === node.file.path;
    return (
      <li>
        <button
          onClick={() => onSelect(node.file!.path)}
          title={node.file.path}
          style={indent}
          className={cn(
            "flex w-full items-center gap-1.5 py-1 pr-2 text-left text-[11px]",
            FOCUS_RING,
            active
              ? "bg-muted text-foreground"
              : "text-muted-foreground hover:bg-muted/60",
          )}
        >
          <FileIcon className="size-3 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate">{node.name}</span>
          <Stat stat={node.file.stat} />
        </button>
      </li>
    );
  }

  const isCollapsed = collapsed.has(node.path);
  // Folded chains ("app/[locale]") hide their intermediate folders, so count
  // the files underneath rather than the direct children.
  const fileCount = countFiles(node);

  return (
    <li>
      {/*
        A real button: the chevron was previously drawn on a plain div, which
        promised a control that did not exist.
      */}
      <button
        onClick={() => onToggle(node.path)}
        aria-expanded={!isCollapsed}
        style={indent}
        className={cn(
          "flex w-full items-center gap-1 py-1 pr-2 text-left text-[11px] text-muted-foreground hover:bg-muted/60 hover:text-foreground",
          FOCUS_RING,
        )}
      >
        <ChevronRight
          className={cn(
            "size-3 shrink-0 opacity-50 transition-transform",
            !isCollapsed && "rotate-90",
          )}
        />
        <span className="min-w-0 flex-1 truncate font-medium">{node.name}</span>
        {isCollapsed ? (
          <span className="shrink-0 tabular-nums text-muted-foreground">
            {fileCount}
          </span>
        ) : null}
      </button>
      {isCollapsed ? null : (
        <ul>
          {node.children.map((child) => (
            <Node
              key={child.path}
              node={child}
              depth={depth + 1}
              activePath={activePath}
              onSelect={onSelect}
              collapsed={collapsed}
              onToggle={onToggle}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

export function Stat({ stat }: { stat: FileStat }) {
  if (stat.kind === "unknown") {
    return (
      <span className="shrink-0 text-muted-foreground" title={stat.title}>
        {stat.label}
      </span>
    );
  }
  if (stat.tone === "risk") {
    return (
      <span className="shrink-0 tabular-nums text-review" title={stat.title}>
        {stat.insertions > 0 ? `+${stat.insertions}` : ""}
        {stat.insertions > 0 && stat.deletions > 0 ? " " : ""}
        {stat.deletions > 0 ? `−${stat.deletions}` : ""}
      </span>
    );
  }
  return (
    <span className="shrink-0 tabular-nums" title={stat.title}>
      <span className="text-disposable">+{stat.insertions}</span>{" "}
      <span className="text-broken">−{stat.deletions}</span>
    </span>
  );
}
