/* Illustrative state only. This viewer never executes terminal or host actions. */
const $ = (selector) => document.querySelector(selector);
const all = (selector) => [...document.querySelectorAll(selector)];
const state = { context: "remote", sessionKey: "remote", remoteName: "Demo", host: "hill.example.ts.net", empty: false, tabs: ["mitchellh@hill:~"], activeTab: 0, split: true };
const notes = {
  local: "Local terminal · S01–S03 · persistent shell context.",
  remote: "Remote split · S07 / S20 / S22 · two shells, one session.",
  sessions: "Session switcher · S09 / S12 / S17 · sessions grouped by host.",
  commands: "Command palette · S04 / S10 · a subset of visible commands is interactive here.",
  host: "Add host · S05 · input plus connection action; successful connection is simulated.",
  rename: "Rename · S11 · display-name update; no session identity change.",
  empty: "Empty session · S18 · session exists before its first terminal tab.",
  directory: "Go to Directory · S25–S31 · choose Local or Remote first to compare host contexts."
};
const localRoot = "/Users/mitchellh/Documents/ghostty/";
const directories = {
  local: { [localRoot]: ["dist", "example", "flatpak", "images", "include", "macos", "nix", "pkg", "po", "src", "test", "vendor"] },
  remote: { "/": ["bin", "boot", "dev", "etc", "home", "lib64", "nix", "proc", "root", "run", "sys", "tmp"], "/proc/": ["1", "2", "3", "4", "5", "6"], "/home/": ["git", "mitchellh", "rex-it-second", "rex-it-test"] }
};
const pad = (value) => String(value).padStart(2, "0");
const timeLabel = (seconds) => `${pad(Math.floor(seconds / 60))}:${pad(Math.floor(seconds % 60))}`;

function seekVideo(seconds) {
  $("#video").currentTime = seconds;
  $("#source").scrollIntoView();
}

function buildTranscript() {
  for (const segment of window.evidence.segments) {
    const button = document.createElement("button");
    const time = document.createElement("time");
    time.textContent = timeLabel(segment.start);
    button.append(time, document.createTextNode(segment.text));
    button.addEventListener("click", () => seekVideo(segment.start));
    $("#transcript").append(button);
  }
}

function buildGallery() {
  for (const frame of window.evidence.frames) {
    const card = document.createElement("article");
    card.className = "evidence-card";
    // Content comes from the checked-in evidence manifest, not user input.
    card.innerHTML = `<a href="screenshots/${frame.id}.png" target="_blank" rel="noopener"><img src="screenshots/${frame.id}.png" alt="${frame.id}: ${frame.title}" loading="lazy" width="540" height="360"></a><div class="card-body"><div class="card-meta"><span>${frame.id}</span><span>${timeLabel(frame.time)}</span></div><h3>${frame.title}</h3><p>${frame.note}</p><button class="seek-button">Watch at ${timeLabel(frame.time)}</button></div>`;
    card.querySelector("button").addEventListener("click", () => seekVideo(frame.time));
    $("#screenshots").append(card);
  }
  $("#gallery-count").textContent = `${window.evidence.frames.length} evidence frames`;
}

function filterElements(input, selector) {
  const query = input.value.toLowerCase();
  for (const element of all(selector)) {
    element.hidden = !element.textContent.toLowerCase().includes(query);
  }
}

function renderTabs() {
  $("#tabs").replaceChildren();
  state.tabs.forEach((title, index) => {
    const button = document.createElement("button");
    button.className = `tab ${index === state.activeTab ? "active" : ""}`;
    button.textContent = title;
    button.addEventListener("click", () => { state.activeTab = index; renderTerminal(); });
    $("#tabs").append(button);
  });
}

function renderTerminal() {
  const local = state.context === "local";
  $("#session-name").textContent = state.sessionKey === "empty" ? "crisp-sierra" : (local ? "cosmic-summit" : state.remoteName);
  $("#host-name").textContent = local ? "This Mac" : state.host;
  $("#remote-session-row").firstChild.textContent = `${state.remoteName} `;
  $("#remote-group").textContent = state.host;
  all("[data-context]").forEach((button) => button.classList.toggle("selected", button.dataset.context === state.sessionKey));
  $("#empty-state").hidden = !state.empty;
  $("#pane-layout").hidden = state.empty;
  const split = state.split && state.activeTab === 0;
  $("#pane-layout").classList.toggle("single", !split);
  $("#right-pane").hidden = !split;
  const base = local ? "~/Documents/ghostty $ hello\n" : "[mitchellh@hill:~]$ uname -a\nLinux hill 6.18.48 NixOS x86_64 GNU/Linux\n\n[mitchellh@hill:~]$ ";
  const content = state.activeTab === 0 ? base : `${state.tabs[state.activeTab]}\n$ `;
  $("#left-terminal").textContent = state.sessionKey === "empty" ? `${state.tabs[state.activeTab] || ""}\n$ ` : content;
  renderTabs();
}

function dismissPanel() {
  $("#mock-overlay").hidden = true;
  $("#session-trigger").focus({ preventScroll: true });
}

function setContext(context) {
  state.sessionKey = context;
  state.context = context === "local" ? "local" : "remote";
  state.empty = context === "empty";
  state.tabs = state.empty ? [] : [state.context === "local" ? "~/Documents/ghostty" : "mitchellh@hill:~"];
  state.activeTab = 0;
  state.split = context === "remote";
  dismissPanel();
  renderTerminal();
}

function directoryPanel() {
  const local = state.context === "local";
  $("#directory-input").value = local ? localRoot : "/";
  $("#directory-host").textContent = local ? "This Mac · example directories" : `${state.host} · example directories`;
  renderDirectories();
}

function renderDirectories() {
  const path = $("#directory-input").value.replace(/\/?$/, "/");
  const children = directories[state.context][path] || [];
  $("#directory-options").replaceChildren();
  for (const child of children) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = `▱ ${child}`;
    button.addEventListener("click", () => { $("#directory-input").value = `${path}${child}/`; renderDirectories(); });
    $("#directory-options").append(button);
  }
}

function showPanel(panel) {
  all("[data-panel]").forEach((element) => { element.hidden = element.dataset.panel !== panel; });
  $("#mock-overlay").hidden = false;
  $("#mock-overlay").classList.toggle("switcher", panel === "sessions");
  $("#popover-title").textContent = panel === "sessions" ? "Change session" : "Command";
  $("#rename-input").value = state.remoteName;
  directoryPanel();
  $(`[data-panel="${panel}"] input`)?.focus({ preventScroll: true });
}

function showScene(scene) {
  $("#scene-note").textContent = notes[scene];
  all(".scene-controls button").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.scene === scene)));
  if (["local", "remote", "empty"].includes(scene)) {
    setContext(scene);
    return;
  }
  if (scene === "rename") setContext("remote");
  if (scene === "directory" && state.empty) setContext("remote");
  showPanel(scene);
}

function newTab(title = "New shell") {
  state.empty = false;
  state.tabs.push(title);
  state.activeTab = state.tabs.length - 1;
  dismissPanel();
  renderTerminal();
}

function handleMockKey(event) {
  if (event.key === "Escape") { dismissPanel(); return; }
  if (!(event.metaKey || event.ctrlKey)) return;
  const key = `${event.shiftKey ? "shift+" : ""}${event.key.toLowerCase()}`;
  const scene = { k: "sessions", "shift+p": "commands", "shift+g": "directory" }[key];
  if (!scene) return;
  event.preventDefault();
  showScene(scene);
}

all("[data-scene]").forEach((button) => button.addEventListener("click", () => showScene(button.dataset.scene)));
all("[data-context]").forEach((button) => button.addEventListener("click", () => setContext(button.dataset.context)));
$("#session-trigger").addEventListener("click", () => showScene("sessions"));
$("#dismiss").addEventListener("click", dismissPanel);
$("#terminal-window").addEventListener("keydown", handleMockKey);
for (const selector of ["#new-tab", "#empty-new-tab", "#command-new-tab"]) $(selector).addEventListener("click", () => newTab());
$("#host-form").addEventListener("submit", (event) => {
  event.preventDefault();
  state.host = $("#host-input").value;
  setContext("remote");
  state.split = false;
  renderTerminal();
});
$("#rename-form").addEventListener("submit", (event) => {
  event.preventDefault();
  state.remoteName = $("#rename-input").value;
  dismissPanel();
  renderTerminal();
});
$("#directory-form").addEventListener("submit", (event) => { event.preventDefault(); newTab($("#directory-input").value); });
$("#directory-input").addEventListener("input", renderDirectories);
$("#transcript-search").addEventListener("input", (event) => filterElements(event.target, "#transcript button"));
$("#gallery-search").addEventListener("input", (event) => {
  filterElements(event.target, ".evidence-card");
  $("#gallery-count").textContent = `${all(".evidence-card:not([hidden])").length} of ${window.evidence.frames.length} evidence frames`;
});
$("#session-filter").addEventListener("input", (event) => filterElements(event.target, "#session-options button"));
$("#command-filter").addEventListener("input", (event) => filterElements(event.target, "#command-options button"));
buildTranscript();
buildGallery();
showScene("remote");
