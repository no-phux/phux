// Throwaway visual study. In-memory fixtures only; no APIs, storage or mutations.
const groups = [
  { id: 'sidebar', session: 'phux', path: 'no-phux/phux', name: 'Sidebar polish', kind: 'terminal', members: ['Editor', 'Server', 'Tests'], tools: ['claude', 'terminal', 'terminal'], status: 'working' },
  { id: 'release', session: 'phux', name: 'Release', kind: 'terminal', members: ['Shell', 'Build'], tools: ['codex', 'terminal'], status: 'waiting' },
  { id: 'theme', session: 'Rune', path: 'clients/rune', name: 'Theme cleanup', kind: 'terminal', members: ['Editor', 'Preview'], tools: ['opencode', 'terminal'], status: 'working' },
  { id: 'browser', session: 'Rune', name: 'App preview', kind: 'surface', members: ['Page'], tools: ['browser'], detail: 'localhost:3000', status: 'quiet' },
  { id: 'docs', session: 'Docs', path: 'docs', name: 'Architecture notes', kind: 'document', members: ['Markdown'], detail: 'architecture.md', status: 'quiet' },
  { id: 'logs', session: 'Ops', path: 'infra', name: 'Live logs', kind: 'terminal', members: ['Logs'], status: 'quiet' },
  { id: 'deploy', session: 'Ops', name: 'Deploy', kind: 'terminal', members: ['Shell', 'Output'], tools: ['codex', 'terminal'], status: 'working' },
  { id: 'scratch', session: 'Scratch', name: 'Scratch', kind: 'terminal', members: ['Shell'], status: 'quiet' },
  { id: 'future', session: 'Scratch', name: 'Design reference', kind: 'future-resource', members: ['Reference'], detail: 'Unsupported type', status: 'unsupported' },
];
const variants = {
  __proto__: null,
  A: { name: 'Identity first. Depth in the details.', description: 'One compact language for agents, terminals, browser surfaces and documents. Recognizable tool marks sit inside quiet layered surfaces; single resources stay flat. Useful names, state and context stay visible. Unsupported types get an honest neutral fallback. All identities and content here are illustrative fixture data, not live discovery.' },
  B: { name: 'Let the active split open itself.', description: 'Inactive work stays one line. The selected group exposes its panes as a shallow 3D deck—each labelled surface can be selected directly. The small geometry mark distinguishes membership from the literal split layout. No separate expand control or hidden disclosure target.' },
  C: { name: 'The split is the navigator.', description: 'The actual split arrangement becomes a small, clickable window. Layered edges give it depth without surrounding it in a card. Pane labels live inside their real regions; selection has one quiet accent. Singles stay plain rows, rather than miniature windows with nothing to explain.' },
};
let variant = new URLSearchParams(location.search).get('variant') || 'A';
if (!variants[variant]) variant = 'A';
let selected = new URLSearchParams(location.search).get('resource') || 'sidebar';
if (!groups.some(group => group.id === selected)) selected = 'sidebar';
let focused = 0;
let filter = '';
const $ = id => document.getElementById(id);
const statusClasses = { __proto__: null, working: '', waiting: 'waiting', quiet: 'quiet', unsupported: 'quiet' };
const marker = status => `<span class="row-state ${statusClasses[status] ?? 'quiet'}" aria-label="${status}"></span>`;
const tools = {
  __proto__: null,
  claude: { name: 'Claude', mark: 'claude' },
  codex: { name: 'Codex', mark: 'codex' },
  opencode: { name: 'OpenCode', mark: 'opencode' },
  terminal: { name: 'Terminal', mark: 'terminal' },
  browser: { name: 'Browser', mark: 'browser' },
};
const resourceKinds = {
  __proto__: null,
  terminal: { name: 'Terminal', mark: 'terminal', unit: 'pane', render: terminalWorkspace },
  surface: { name: 'Surface', mark: 'surface', unit: 'resource', render: resourcePreview },
  document: { name: 'Document', mark: 'document', unit: 'resource', render: resourcePreview },
  unknown: { name: 'Resource', mark: 'resource', unit: 'resource', render: resourcePreview },
};
function resourceKind(group) {
  return resourceKinds[group.kind] || resourceKinds.unknown;
}
function identity(group, index) {
  const kind = group.tools?.[index];
  return tools[kind] || resourceKind(group);
}
function toolMark(tool) {
  return `<span class="tool-mark tool-${tool.mark}" aria-hidden="true"></span>`;
}
function identityStack(group) {
  const tool = identity(group, 0);
  const layers = Math.min(group.members.length - 1, 2);
  const unit = resourceKind(group).unit;
  return `<span class="identity-stack" title="${tool.name} · ${group.members.length} ${unit}s">${Array.from({ length: layers }, (_, index) => `<span class="identity-layer layer-${index}" aria-hidden="true"></span>`).join('')}<span class="identity-front">${toolMark(tool)}</span></span>`;
}
function inlineMembers(group) {
  return group.members.map((name, index) => `<button class="inline-member ${group.id === selected && index === focused ? 'active' : ''}" data-group="${group.id}" data-pane="${index}" aria-pressed="${group.id === selected && index === focused}" aria-label="Focus ${name}, ${identity(group, index).name}, in ${group.name}" title="${identity(group, index).name}">${name}</button>`).join('<span class="member-divider" aria-hidden="true">·</span>');
}
function mosaic(group, labels = false) {
  return `<div class="mosaic ${group.members.length === 2 ? 'two' : 'three'}" aria-label="Illustrative literal split arrangement">${group.members.map((name, index) => `<button class="mini-pane ${group.id === selected && index === focused ? 'active' : ''}" data-group="${group.id}" data-pane="${index}" aria-label="Focus ${name} in ${group.name}"><span class="${labels ? 'pane-name' : 'mosaic-lines'}">${labels ? name : ''}</span></button>`).join('')}</div>`;
}
function plain(group) {
  const name = group.name;
  const detail = group.detail || group.members[0];
  const mark = variant === 'A' ? toolMark(identity(group, 0)) : '$';
  return `<div class="group plain-row ${selected === group.id ? 'selected' : ''}" data-select="${group.id}" role="button" tabindex="0" aria-label="${name}, ${identity(group, 0).name}, one ${resourceKind(group).unit}"><span class="plain-identity">${mark}</span><span class="group-title">${name}</span><span class="group-meta" title="${detail}">${detail}</span>${marker(group.status)}</div>`;
}
function inline(group) {
  return `<div class="group inline-row ${selected === group.id ? 'selected' : ''}" role="group" aria-label="${group.name}, ${group.members.length} ${resourceKind(group).unit}s">${identityStack(group)}<div class="group-labels"><button class="group-title" data-select="${group.id}">${group.name}</button><div class="group-meta">${inlineMembers(group)}</div></div>${marker(group.status)}</div>`;
}
function deck(group) {
  if (selected !== group.id) return `<div class="group deck-row" data-select="${group.id}" role="button" tabindex="0" aria-label="${group.name}, ${group.members.length} panes"><div class="mini-stack">${mosaic(group)}</div><span class="group-title">${group.name}</span><span class="pane-count">${group.members.length}</span>${marker(group.status)}</div>`;
  return `<div class="group selected deck-open"><div class="row-main">${marker(group.status)}<span class="group-title">${group.name}</span></div><div class="exposed-stack" style="height:${group.members.length === 2 ? 72 : 92}px">${group.members.map((name, index) => `<button class="exposed-plane ${index === focused ? 'active' : ''}" data-group="${group.id}" data-pane="${index}" aria-label="Focus ${name} in ${group.name}"><span class="plane-title"><span class="row-state ${index === 2 ? 'quiet' : ''}"></span>${name}</span><span class="plane-code">${['$ nvim sidebar.rs', '$ cargo run', '$ just check'][index] || '$'}</span></button>`).join('')}</div><div class="deck-caption"><span>${group.members.length} panes · one split</span><span class="tiny-layout" title="Illustrative literal split arrangement">${mosaic(group)}</span></div></div>`;
}
function spatial(group) {
  return `<div class="group spatial-row ${selected === group.id ? 'selected' : ''}"><div class="row-main" data-select="${group.id}" role="button" tabindex="0">${marker(group.status)}<div class="group-labels"><span class="group-title">${group.name}</span></div><span class="pane-count">${group.members.length}</span></div><div class="spatial-stack">${mosaic(group, true)}</div></div>`;
}
function matchesFilter(group) {
  const fields = [group.session, group.name, group.detail, resourceKind(group).name, ...group.members, ...group.members.map((_, index) => identity(group, index).name)];
  return fields.join(' ').toLowerCase().includes(filter);
}
function renderGroups() {
  let previous = '';
  $('groups').innerHTML = groups.filter(matchesFilter).map(group => {
    const repeated = groups.filter(item => item.session === group.session).length > 1;
    const label = repeated && group.session !== previous ? `<div class="session-label"><span>${group.session}</span><span class="session-path">${group.path || ''}</span></div>` : '';
    previous = group.session;
    return label + (group.members.length === 1 ? plain(group) : { A: inline, B: deck, C: spatial }[variant](group));
  }).join('');
  $('groups').querySelectorAll('[data-select]').forEach(element => {
    const select = () => choose(element.dataset.select, 0);
    element.addEventListener('click', event => { if (!event.target.closest('[data-pane]')) select(); });
    element.addEventListener('keydown', event => { if (event.target === element && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); select(); } });
  });
  $('groups').querySelectorAll('[data-pane]').forEach(element => element.addEventListener('click', () => choose(element.dataset.group, Number(element.dataset.pane))));
  requestAnimationFrame(() => {
    const elements = [...$('groups').children];
    const height = elements.length ? Math.round(elements.at(-1).getBoundingClientRect().bottom - elements[0].getBoundingClientRect().top) : 0;
    $('density').textContent = `${height} px of work`;
  });
}
const examples = [
  `<span class="comment">// sidebar.rs</span>\n<span class="bright">fn pane_stack(item: &amp;Item) {</span>\n    let panes = item.members();\n    draw_literal_layout(panes);\n}\n\n<span class="comment">// No new grouping model.\n// No hidden disclosure target.\n// Keep the actual split.</span>\n\n$ <span class="cursor"></span>`,
  `<span class="comment">~/no-phux/phux</span>\n$ cargo run --profile dev\n\n<span class="success">Connected</span> to local development server\n\nsession   phux\npanes     3\ntransport unix\n\n<span class="comment">Watching for changes…</span>`,
  `$ just check\n\n<span class="success">pass</span>  layout keeps the selected pane\n<span class="success">pass</span>  focus belongs to the terminal\n<span class="success">pass</span>  attention remains discoverable\n\n<span class="comment">Illustrative output only.\nNo commands run in this prototype.</span>`,
];
function renderWorkspace() {
  const group = groups.find(group => group.id === selected);
  $('workspace-name').textContent = group.name;
  document.querySelector('.workspace-session').textContent = group.session;
  $('terminals').style.gridTemplateColumns = group.members.length === 1 ? '1fr' : group.members.length === 2 ? '1fr 1fr' : '1.35fr 1fr';
  $('terminals').style.gridTemplateRows = group.members.length > 2 ? '1fr 1fr' : '1fr';
  $('terminals').innerHTML = resourceKind(group).render(group);
  const unit = resourceKind(group).unit;
  $('focus-label').textContent = `${group.members[focused]} · ${group.members.length} ${unit}${group.members.length === 1 ? '' : 's'}`;
  document.querySelector('.workspace-bottom span:last-child').textContent = unit === 'pane' ? 'Literal split arrangement · illustrative data' : 'Illustrative resource · no live connection or input';
  $('state').textContent = `${group.session} / ${group.name} / ${group.members[focused]}`;
}
function terminalWorkspace(group) {
  return group.members.map((name, index) => terminalPane(group, name, index)).join('');
}
function terminalPane(group, name, index) {
  return `<section class="terminal ${index === focused ? 'focused' : ''}" style="${group.members.length <= 2 ? 'grid-row:auto' : ''}"><header class="terminal-bar"><span>${name}</span><span>${index === focused ? 'focused' : group.status}</span></header><pre>${examples[index] || examples[0]}</pre></section>`;
}
function resourcePreview(group) {
  const kind = resourceKind(group);
  return `<section class="resource-preview"><div class="preview-identity">${toolMark(identity(group, 0))}</div><h2>${group.name}</h2><p>${group.detail || kind.name}</p><p>${kind === resourceKinds.unknown ? 'This resource type has no renderer here. Its identity remains visible; no capabilities or controls are invented.' : 'Illustrative read-only resource. A real resource keeps its own renderer and capabilities; it is not made to look or behave like a terminal.'}</p></section>`;
}
function choose(id, pane) {
  selected = id;
  focused = pane;
  renderGroups();
  renderWorkspace();
}
function switchVariant(key) {
  variant = key;
  const url = new URL(location.href);
  url.searchParams.set('variant', key);
  history.replaceState(null, '', url);
  document.querySelectorAll('[data-variant]').forEach(button => button.setAttribute('aria-current', String(button.dataset.variant === variant)));
  $('variant-kicker').textContent = `VARIANT ${variant} / ${groups.length} GROUPS · ${groups.reduce((count, group) => count + group.members.length, 0)} RESOURCES`;
  $('variant-name').textContent = variants[variant].name;
  $('variant-description').textContent = variants[variant].description;
  renderGroups();
  renderWorkspace();
}
document.querySelectorAll('[data-variant]').forEach(button => button.addEventListener('click', () => switchVariant(button.dataset.variant)));
$('width').addEventListener('input', event => {
  document.documentElement.style.setProperty('--sidebar-width', `${event.target.value}px`);
  $('width-value').textContent = event.target.value;
  renderGroups();
});
$('theme').addEventListener('click', () => {
  document.body.classList.toggle('light');
  $('theme').textContent = document.body.classList.contains('light') ? 'Dark' : 'Light';
});
$('search-button').addEventListener('click', () => { $('search').hidden = !$('search').hidden; if (!$('search').hidden) $('search').focus(); });
$('search').addEventListener('input', event => { filter = event.target.value.toLowerCase(); renderGroups(); });
$('new-session').addEventListener('click', () => { $('state').textContent = 'Prototype only · no server mutation'; });
const parameters = new URLSearchParams(location.search);
const initialWidth = Math.min(340, Math.max(220, Number(parameters.get('width')) || 280));
$('width').value = initialWidth;
$('width-value').textContent = initialWidth;
document.documentElement.style.setProperty('--sidebar-width', `${initialWidth}px`);
if (parameters.get('theme') === 'light') {
  document.body.classList.add('light');
  $('theme').textContent = 'Dark';
}
switchVariant(variant);
