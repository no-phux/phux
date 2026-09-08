"use client";
// Fumadocs search dialog, wired to the static client. It fetches the
// pre-built index at /api/search (emitted by scripts/build-search.mjs during
// astro build) — no server endpoint, so the site stays fully static.
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

export default function SearchDialogComponent(props: SharedProps) {
  const { search, setSearch, query } = useDocsSearch({
    client: staticClient({
      // The build emits the index at api/search.json (see build-search.mjs);
      // the default /api/search has no file behind it on a static host.
      from: "/api/search.json",
    }),
  });

  return (
    <SearchDialog
      search={search}
      onSearchChange={setSearch}
      isLoading={query.isLoading}
      {...props}
    >
      <SearchDialogOverlay />
      <SearchDialogContent>
        <SearchDialogHeader>
          <SearchDialogIcon />
          <SearchDialogInput />
          <SearchDialogClose />
        </SearchDialogHeader>
        <SearchDialogList items={query.data !== "empty" ? query.data : null} />
      </SearchDialogContent>
    </SearchDialog>
  );
}
