"use client";
import {
  SearchDialog,
  SearchDialogClose,
  SearchDialogContent,
  SearchDialogHeader,
  SearchDialogIcon,
  SearchDialogInput,
  SearchDialogList,
  SearchDialogOverlay,
  type SharedProps,
} from "fumadocs-ui/components/dialog/search";
import { useDocsSearch } from "fumadocs-core/search/client";
import { staticClient } from "fumadocs-core/search/client/orama-static";
import type { SortedResult } from "fumadocs-core/search";
import { useMemo, useState } from "react";

function normalizedTitle(value: string) {
  return value.replace(/<\/?mark>/g, "").toLocaleLowerCase().trim();
}

function titleMatch(title: string, query: string) {
  if (title === query) return 2;
  if (title.startsWith(`${query} `)) return 1;
  return 0;
}

/** Rank page groups, never individual hits: headings stay with their page. */
function rankPages(results: SortedResult[], query: string) {
  const needle = normalizedTitle(query);
  const groups: { score: number; items: SortedResult[] }[] = [];
  for (const result of results) {
    if (result.type === "page" || groups.length === 0) {
      groups.push({ score: titleMatch(normalizedTitle(result.content), needle), items: [] });
    }
    groups[groups.length - 1].items.push(result);
  }
  return groups.sort((a, b) => b.score - a.score).flatMap((group) => group.items);
}

export default function SearchDialogComponent(props: SharedProps) {
  const [attempt, setAttempt] = useState(0);
  const client = useMemo(() => {
    // The upstream static client caches rejected loads as well as successes.
    // A user-requested retry needs a fresh cache key, not just a repeated query.
    const source = staticClient({ from: attempt === 0 ? "/api/search.json" : `/api/search.json?retry=${attempt}` });
    return {
      deps: [attempt],
      async search(query: string) {
        return rankPages(await source.search(query), query);
      },
    };
  }, [attempt]);
  const { search, setSearch, query } = useDocsSearch({ client });

  return (
    <SearchDialog search={search} onSearchChange={setSearch} isLoading={query.isLoading} {...props}>
      <SearchDialogOverlay />
      <SearchDialogContent>
        <SearchDialogHeader>
          <SearchDialogIcon aria-hidden="true" />
          <SearchDialogInput />
          <SearchDialogClose />
        </SearchDialogHeader>
        {query.error && !query.isLoading ? (
          <div className="docs-search-error">
            <p role="alert">Search could not load. Check your connection and try again.</p>
            <button type="button" onClick={() => setAttempt((value) => value + 1)}>Try again</button>
            <a href="/docs">Browse all documentation</a>
          </div>
        ) : (
          <SearchDialogList items={query.data !== "empty" ? query.data : null} />
        )}
      </SearchDialogContent>
    </SearchDialog>
  );
}
