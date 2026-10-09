/** Arguments for `phux new` that create a session on one socket in one directory. */
export function createSessionArgs(socket: string, name: string, directory: string): string[] {
  return ["--socket", socket, "new", "--json", "-s", name, "--cwd", directory];
}

/** Arguments for `phux rename` on one socket. */
export function renameSessionArgs(socket: string, current: string, next: string): string[] {
  return ["--socket", socket, "rename", current, next];
}

/** Arguments for `phux kill` of one session on one socket. */
export function closeSessionArgs(socket: string, name: string): string[] {
  return ["--socket", socket, "kill", name];
}

/**
 * The session a new window attaches. An explicit name wins. An empty pick
 * keeps the window that opened it. This does not invent a host or a socket.
 */
export function windowSession(explicit: string | undefined, current: string): string {
  const name = explicit?.trim() ?? "";
  return name === "" ? current : name;
}

/**
 * Refuse a create that would fall back to the desktop process directory.
 * An empty directory is not a request to inherit the launcher cwd.
 */
export function createSessionError(name: string, directory: string): string | undefined {
  if (name.trim() === "") return "A session needs a name.";
  if (directory.trim() === "")
    return "A new session needs the focused directory or a chosen folder.";
  return undefined;
}

/** Refuse a rename that names neither side. An unchanged name is not an error. */
export function renameSessionError(current: string, next: string): string | undefined {
  if (current.trim() === "" || next.trim() === "")
    return "Rename needs the current session and a new name.";
  return undefined;
}
