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
  host?: string;
  project?: string;
}

export interface ProjectGroup {
  project?: string;
  sessions: SessionLine[];
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

/** Group by project tag. Tags sort. Untagged is last. Input order is kept. */
export function groupByProject(sessions: readonly SessionLine[]): ProjectGroup[] {
  const tags: Array<string | undefined> = [];
  for (const session of sessions) {
    const tag = blank(session.project);
    if (!tags.includes(tag)) tags.push(tag);
  }
  tags.sort((left, right) => {
    if (left === right) return 0;
    if (left === undefined) return 1;
    if (right === undefined) return -1;
    return left < right ? -1 : 1;
  });
  return tags.flatMap((tag) => {
    const members = sessions.filter((session) => blank(session.project) === tag);
    if (members.length === 0) return [];
    const group: ProjectGroup = { sessions: members };
    if (tag !== undefined) group.project = tag;
    return [group];
  });
}
