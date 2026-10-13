import { getCollection, type CollectionEntry } from "astro:content";
import type { APIRoute } from "astro";
import { SITE } from "../lib/site";

// Publish the same collection as HTML, without converting rendered UI chrome.
export async function getStaticPaths() {
  return (await getCollection("docs")).map((page) => ({
    params: { slug: page.id },
    props: { page },
  }));
}

export const GET: APIRoute = ({ props }) => {
  const page = props.page as CollectionEntry<"docs">;
  const { title, summary, sourcePath, sourceRevision, projectVersion, protocolVersion, stability } = page.data;
  const metadata = {
    title,
    url: `${SITE.docsUrl}/${page.id}`,
    source: `${SITE.github}/blob/${sourceRevision}/${sourcePath}`,
    projectVersion,
    protocolVersion,
    stability,
  };
  const frontmatter = Object.entries(metadata).map(([key, value]) => `${key}: ${JSON.stringify(value)}`).join("\n");
  // The custom overview's source already includes its title and summary.
  const body = page.id === "overview" ? page.body : `# ${title}\n\n${summary}\n\n${page.body}`;
  return new Response(`---\n${frontmatter}\n---\n\n${body}`, {
    headers: { "content-type": "text/markdown; charset=utf-8" },
  });
};
