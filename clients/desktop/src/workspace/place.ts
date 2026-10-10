/**
 * Where a new session, tab, or split starts, and how a picker groups sessions.
 * The same rules live in `phux_client_core::organization`.
 */

export interface Place {
  host?: string;
  directory?: string;
}

export interface SessionLine {
  name: string;
  host?: string | undefined;
  project?: string | undefined;
}

export interface ProjectGroup<T extends SessionLine = SessionLine> {
  project?: string | undefined;
  host?: string | undefined;
  sessions: T[];
}

function blank(value: string | undefined): string | undefined {
  return value && value.length > 0 ? value : undefined;
}

/** Focused host and directory, unless an explicit field overrides it. */
export function placeFor(focused: Place | undefined, explicit?: Place): Place {
  const host = blank(explicit?.host) ?? blank(focused?.host);
  const directory = blank(explicit?.directory) ?? blank(focused?.directory);
  const place: Place = {};
  if (host !== undefined) place.host = host;
  if (directory !== undefined) place.directory = directory;
  return place;
}

export function terminalHost(terminalId: string | undefined): string | undefined {
  if (!terminalId?.startsWith("satellite:")) return undefined;
  const rest = terminalId.slice("satellite:".length);
  const terminalSeparator = rest.lastIndexOf(":");
  const host = terminalSeparator < 0 ? rest : rest.slice(0, terminalSeparator);
  return blank(host);
}

/** Group by project tag, then remote host. Tags sort. Untagged/local is last. Input order is kept. */
export function groupByProject<T extends SessionLine>(sessions: readonly T[]): ProjectGroup<T>[] {
  const keys: Array<{ project?: string | undefined; host?: string | undefined }> = [];
  for (const session of sessions) {
    const key = { project: blank(session.project), host: blank(session.host) };
    if (!keys.some((item) => item.project === key.project && item.host === key.host))
      keys.push(key);
  }
  keys.sort(
    (left, right) => compareKey(left.project, right.project) || compareKey(left.host, right.host),
  );
  return keys.flatMap((key) => {
    const members = sessions.filter(
      (session) => blank(session.project) === key.project && blank(session.host) === key.host,
    );
    if (members.length === 0) return [];
    const group: ProjectGroup<T> = { sessions: members };
    if (key.project !== undefined) group.project = key.project;
    if (key.host !== undefined) group.host = key.host;
    return [group];
  });
}

function compareKey(left: string | undefined, right: string | undefined): number {
  if (left === right) return 0;
  if (left === undefined) return 1;
  if (right === undefined) return -1;
  return left < right ? -1 : 1;
}
