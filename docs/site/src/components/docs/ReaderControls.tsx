import { useEffect, useRef, useSyncExternalStore, type ComponentProps } from "react";
import { Sidebar, useSidebar } from "fumadocs-ui/layouts/docs/slots/sidebar";
import { useSearchContext } from "fumadocs-ui/contexts/search";
import { useTheme } from "fumadocs-ui/provider/base";

const subscribe = () => () => {};

export function ReaderTheme({ className }: { className?: string }) {
  const { theme, setTheme } = useTheme();
  const mounted = useSyncExternalStore(subscribe, () => true, () => false);
  return (
    <label className={`docs-theme ${className ?? ""}`}>
      <span>Theme</span>
      <select aria-label="Reading theme" value={mounted ? theme : "system"} onChange={(event) => setTheme(event.target.value)}>
        <option value="light">Light</option>
        <option value="dark">Dark</option>
        <option value="system">System</option>
      </select>
    </label>
  );
}

/** Keep the framework's tree and controls; give its mobile drawer native modal behavior. */
export function ReaderSidebar(props: ComponentProps<typeof Sidebar>) {
  const { open, setOpen, mode } = useSidebar();
  const { open: searchOpen } = useSearchContext();
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    if (searchOpen) setOpen(false);
  }, [searchOpen, setOpen]);

  useEffect(() => {
    const element = dialog.current;
    if (mode !== "drawer" || !open || searchOpen || !element) return;
    element.showModal();
    // Focus the close control, not a theme selector or an arbitrary first link.
    element.querySelector<HTMLButtonElement>('[aria-controls="nd-sidebar-mobile"]')?.focus();
    return () => element.close();
  }, [mode, open, searchOpen]);

  if (mode !== "drawer") return <Sidebar {...props} />;
  return (
    <dialog
      ref={dialog}
      className="docs-navigation-dialog"
      aria-label="Documentation navigation"
      onCancel={(event) => { event.preventDefault(); setOpen(false); }}
      onClose={() => setOpen(false)}
    >
      <Sidebar {...props} />
    </dialog>
  );
}
