import { For, Show, type JSX } from "solid-js";
import { usePlugin } from "@opencode/plugin/gui";

import type { definition, Prefs } from "./definition.js";
import type { Fleet } from "./fleet.js";

type Switch = keyof Prefs;

const SECTIONS: readonly { readonly title: string; readonly switches: readonly Switch[] }[] = [
  { title: "settings.section.notifications", switches: ["notifyBlocked", "notifyFinished"] },
  { title: "settings.section.terminals", switches: ["openSpawned", "fitWidth"] },
];

/**
 * The phux settings page. It reuses the markup of the app's own settings
 * pages so it reads as one of them; unknown to a future app, it still lays
 * out as plain rows.
 */
export function SettingsView(props: { readonly fleet: Fleet }): JSX.Element {
  const ctx = usePlugin<typeof definition>();
  const ui = ctx.ui;

  const row = (title: JSX.Element, description: JSX.Element, control: JSX.Element) => (
    <div data-component="settings-row" class="phux-settings-row">
      <div data-slot="settings-row-copy">
        <div data-slot="settings-row-title">{title}</div>
        <div data-slot="settings-row-description">{description}</div>
      </div>
      <div data-slot="settings-row-control">{control}</div>
    </div>
  );

  return (
    <>
      <div class="settings-tab-header">
        <div class="settings-tab-header-row">
          <div class="flex flex-col gap-1">
            <h2 class="settings-tab-title">{ctx.t("settings.title")}</h2>
            <span class="text-11-regular text-v2-text-text-muted">{ctx.t("settings.subtitle")}</span>
          </div>
        </div>
      </div>
      <div class="settings-tab-body settings-tab-body--sectioned">
        <div class="settings-section">
          <h3 class="settings-section-title">{ctx.t("settings.section.connection")}</h3>
          <div data-component="settings-list">
            {row(
              <span class="phux-settings-status">
                <span class="phux-dot" data-state={props.fleet.state.available ? "done" : "blocked"} />
                {props.fleet.state.available ? ctx.t("settings.status.ok") : ctx.t("fleet.unavailable")}
              </span>,
              <Show
                when={props.fleet.state.available}
                fallback={
                  <>
                    {ctx.t("fleet.unavailable.hint")}
                    <Show when={props.fleet.state.error}>{(error) => <div class="phux-error">{error()}</div>}</Show>
                  </>
                }
              >
                {ctx.t("settings.status.ok.description", { count: props.fleet.state.panes.length })}
              </Show>,
              <ui.Button size="small" variant="neutral" icon="refresh" onClick={() => void props.fleet.refresh()}>
                {ctx.t("fleet.refresh")}
              </ui.Button>,
            )}
          </div>
        </div>
        <For each={SECTIONS}>
          {(section) => (
            <div class="settings-section">
              <h3 class="settings-section-title">{ctx.t(section.title)}</h3>
              <div data-component="settings-list">
                <For each={section.switches}>
                  {(key) =>
                    row(
                      ctx.t(`settings.${key}`),
                      ctx.t(`settings.${key}.description`),
                      <ui.Switch
                        checked={ctx.stores.prefs.value[key]}
                        onChange={(checked) =>
                          ctx.stores.prefs.update((prefs) => {
                            prefs[key] = checked;
                          })}
                        label={ctx.t(`settings.${key}`)}
                        hideLabel
                      />,
                    )}
                </For>
              </div>
            </div>
          )}
        </For>
      </div>
    </>
  );
}
