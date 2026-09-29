/**
 * Monochrome 16px stroke icons, rendered by GPUI's `<svg source>` and tinted
 * through `style.color`. Drawn for this app; keep them on a 16-unit grid.
 */
const svg = (body: string): string =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;

export const icons = {
  plus: svg('<path d="M8 3v10M3 8h10"/>'),
  close: svg('<path d="M4 4l8 8M12 4l-8 8"/>'),
  splitRight: svg('<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M8 2.5v11"/>'),
  splitDown: svg('<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M2 8h12"/>'),
  sidebar: svg('<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M6 2.5v11"/>'),
  search: svg('<circle cx="7" cy="7" r="4"/><path d="M10 10l3.5 3.5"/>'),
  settings: svg(
    '<circle cx="8" cy="8" r="2"/><path d="M8 1.8v1.7M8 12.5v1.7M1.8 8h1.7M12.5 8h1.7M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M3.6 12.4l1.2-1.2M11.2 4.8l1.2-1.2"/>',
  ),
  terminal: svg(
    '<rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M5 6.5l2 1.5-2 1.5M8.5 10h2.5"/>',
  ),
  command: svg(
    '<path d="M6 6h4v4H6zM6 6V4.5A1.5 1.5 0 1 0 4.5 6H6M10 6V4.5A1.5 1.5 0 1 1 11.5 6H10M6 10v1.5A1.5 1.5 0 1 1 4.5 10H6M10 10v1.5a1.5 1.5 0 1 0 1.5-1.5H10"/>',
  ),
  folder: svg(
    '<path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.8l1.5 1.5h4.7A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/>',
  ),
  follow: svg('<path d="M8 3v9M4 8.5l4 4 4-4"/>'),
  maximize: svg('<path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9"/>'),
  minimize: svg('<path d="M13.5 6.5h-4v-4M2.5 9.5h4v4M9.5 6.5L14 2M6.5 9.5L2 14"/>'),
  bell: svg('<path d="M4 11V7a4 4 0 0 1 8 0v4l1 1.5H3zM6.5 14h3"/>'),
  bolt: svg('<path d="M9 1.5L3.5 9H8l-1 5.5L12.5 7H8z"/>'),
  window: svg('<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M2 6h12"/>'),
  copy: svg(
    '<rect x="5" y="5" width="8.5" height="8.5" rx="1.5"/><path d="M11 5V3.5A1.5 1.5 0 0 0 9.5 2h-6A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5"/>',
  ),
  refresh: svg(
    '<path d="M13 3v3.5H9.5M3 13V9.5h3.5"/><path d="M12.6 6.5A5 5 0 0 0 3.8 5M3.4 9.5A5 5 0 0 0 12.2 11"/>',
  ),
  chevronDown: svg('<path d="M4 6l4 4 4-4"/>'),
  chevronRight: svg('<path d="M6 4l4 4-4 4"/>'),
  chevronUp: svg('<path d="M4 10l4-4 4 4"/>'),
  eye: svg(
    '<path d="M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>',
  ),
  palette: svg(
    '<path d="M8 2a6 6 0 1 0 0 12c1 0 1.2-.8.8-1.5-.5-.9 0-1.8 1-1.8H11a3 3 0 0 0 3-3C14 4.6 11.3 2 8 2z"/><circle cx="5" cy="7" r=".6"/><circle cx="7.5" cy="4.8" r=".6"/><circle cx="10.5" cy="5.6" r=".6"/>',
  ),
  skull: svg(
    '<path d="M4 11.5V10a5 5 0 1 1 8 0v1.5H4zM6 14v-2.5M10 14v-2.5"/><circle cx="6.3" cy="7.5" r=".8"/><circle cx="9.7" cy="7.5" r=".8"/>',
  ),
} as const;

export type IconName = keyof typeof icons;
