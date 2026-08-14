/** Path helpers for values returned by the native directory picker. */

function lastSeparator(path: string): number {
  return Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
}

/** The directory containing `path`, preserving the path's native separator. */
export function parentPath(path: string): string {
  const cut = lastSeparator(path);
  if (cut < 0) return "";
  if (cut === 0) return path.slice(0, 1);
  if (cut === 2 && /^[A-Za-z]:[\\/]/.test(path)) return path.slice(0, 3);
  return path.slice(0, cut);
}

/** The final component of a POSIX or Windows path. */
export function pathName(path: string): string {
  const cut = lastSeparator(path);
  return cut >= 0 ? path.slice(cut + 1) : path;
}

/** Join a chosen directory and leaf using the directory's native separator. */
export function joinPath(directory: string, leaf: string): string {
  if (!leaf) return directory;
  const separator = directory.includes("\\") && !directory.includes("/") ? "\\" : "/";
  return `${directory.replace(/[\\/]+$/, "")}${separator}${leaf.replace(/^[\\/]+/, "")}`;
}
