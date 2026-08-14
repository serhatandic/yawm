/**
 * The shape of the file tree beside a diff, with no React in it.
 *
 * Splitting it out is not tidiness: the trailing-slash bug below is exactly
 * the kind of thing that is invisible in a rendered tree and obvious in an
 * assertion, and it could not be asserted while it lived inside a component.
 */

export type FileStat =
  | {
      kind: "counts";
      insertions: number;
      deletions: number;
      /**
       * Green for added and red for removed only where that is what the
       * numbers mean. Under "At risk" they count lines deleting the worktree
       * would destroy, and green is this app's word for reclaimable.
       */
      tone: "change" | "risk";
      /** What these two numbers count. */
      title: string;
    }
  /** No number to give, and why. */
  | { kind: "unknown"; label: string; title: string };

export interface FileEntry {
  path: string;
  stat: FileStat;
}

export interface TreeNode {
  name: string;
  path: string;
  children: TreeNode[];
  file?: FileEntry;
}

/**
 * Git's own spelling for "this whole directory is one entry".
 *
 * A path arriving as `remote.git/` used to be split on `/` like any other,
 * which produced a final empty segment: a leaf with no name, no stat and no
 * file behind it, sitting under the directory as if the directory contained
 * something the tree could not name. The backend now strips the slash at the
 * source; this keeps the tree correct for any path that still carries one,
 * because a row that cannot be clicked is worse than a row that is missing.
 */
export function normaliseTreePath(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.length > 0 ? trimmed : path;
}

/**
 * Group files into a tree, folding single-child directories into one row.
 *
 * `src/components/ui` repeated twenty times is noise, so a chain of
 * directories with nothing else in them collapses to a single label — the same
 * thing editors do, and for the same reason.
 */
export function buildTree(files: FileEntry[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", children: [] };

  for (const file of files) {
    const parts = normaliseTreePath(file.path)
      .split("/")
      .filter((part) => part.length > 0);
    if (parts.length === 0) continue;
    let node = root;
    parts.forEach((part, index) => {
      const isLeaf = index === parts.length - 1;
      const path = parts.slice(0, index + 1).join("/");
      let next = node.children.find((c) => c.name === part && !c.file);
      if (!next || isLeaf) {
        next = { name: part, path, children: [], ...(isLeaf ? { file } : {}) };
        node.children.push(next);
      }
      node = next;
    });
  }

  const fold = (nodes: TreeNode[]): TreeNode[] =>
    nodes.map((node) => {
      let current = node;
      // A directory whose only child is another directory adds a row and no
      // information, so merge them into one.
      while (
        !current.file &&
        current.children.length === 1 &&
        !current.children[0]!.file
      ) {
        const only = current.children[0]!;
        current = {
          ...only,
          name: `${current.name}/${only.name}`,
        };
      }
      return { ...current, children: fold(current.children) };
    });

  return fold(root.children);
}

/** Files anywhere beneath a node, so a collapsed folder can say what it hides. */
export function countFiles(node: TreeNode): number {
  if (node.file) return 1;
  return node.children.reduce((n, child) => n + countFiles(child), 0);
}
