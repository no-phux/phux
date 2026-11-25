// Throwaway visual study. In-memory fixtures only; no APIs, storage or mutations.
const groups = [
  { id: 'sidebar', session: 'phux', path: 'no-phux/phux', name: 'Sidebar polish', panes: ['Editor', 'Server', 'Tests'], status: 'working' },
  { id: 'release', session: 'phux', name: 'Release', panes: ['Shell', 'Build'], status: 'waiting' },
  { id: 'theme', session: 'Rune', path: 'clients/rune', name: 'Theme cleanup', panes: ['Editor', 'Preview'], status: 'working' },
  { id: 'docs', session: 'Docs', path: 'docs', name: 'Architecture notes', panes: ['Notes'], status: 'quiet' },
  { id: 'logs', session: 'Ops', path: 'infra', name: 'Live logs', panes: ['Logs'], status: 'quiet' },
  { id: 'deploy', session: 'Ops', name: 'Deploy', panes: ['Shell', 'Output'], status: 'working' },
  { id: 'scratch', session: 'Scratch', name: 'Terminal', panes: ['Shell'], status: 'quiet' },
];
const variants = {
  A: { name: 'An inline stack, not a tree.', description: 'Every split gets one compact row. A shallow stack shows that panes belong together; the front thumbnail preserves their split geometry. All member names stay visible beside it. Singles use one plain line—no duplicate header, chevron or reserved disclosure gutter.' },
  B: { name: 'Let the active split open itself.', description: 'Inactive work stays one line. The selected group exposes its panes as a shallow 3D deck—each labelled surface can be selected directly. The small geometry mark distinguishes membership from the literal split layout. No separate expand control or hidden disclosure target.' },
  C: { name: 'The split is the navigator.', description: 'The actual split arrangement becomes a small, clickable window. Layered edges give it depth without surrounding it in a card. Pane labels live inside their real regions; selection has one quiet accent. Singles stay plain rows, rather than miniature windows with nothing to explain.' },
};
let variant = new URLSearchParams(location.search).get('variant') || 'B';
if (!variants[variant]) variant = 'B';
let selected = 'sidebar';
let focused = 0;
let filter = '';
const $ = id => document.getElementById(id);
const marker = status => `<span class="row-state ${status === 'waiting' ? 'waiting' : status === 'quiet' ? 'quiet' : ''}" aria-label="${status}"></span>`;
function mosaic(group, labels = false) {
  return `<div class="mosaic ${group.panes.length === 2 ? 'two' : 'three'}" aria-label="Illustrative literal split arrangement">${group.panes.map((name, index) => `<button class="mini-pane ${group.id === selected && index === focused ? 'active' : ''}" data-group="${group.id}" data-pane="${index}" aria-label="Focus ${name} in ${group.name}"><span class="${labels ? 'pane-name' : 'mosaic-lines'}">${labels ? name : ''}</span></button>`).join('')}</div>`;
}
function plain(group) {
  const name = group.session === 'Scratch' ? 'Scratch' : group.name;
  return `<div class="group plain-row ${selected === group.id ? 'selected' : ''}" data-select="${group.id}" role="button" tabindex="0" aria-label="${name}, one pane"><span class="plain-identity">$</span><span class="group-title">${name}</span><span class="group-meta">${group.panes[0]}</span>${marker(group.status)}</div>`;
}
function inline(group) {
  return `<div class="group inline-row ${selected === group.id ? 'selected' : ''}" data-select="${group.id}" role="button" tabindex="0" aria-label="${group.name}, ${group.panes.length} panes"><div class="mini-stack">${mosaic(group)}</div><div class="group-labels"><div class="group-title">${group.name}</div><div class="group-meta">${group.panes.join(' · ')}</div></div>${marker(group.status)}</div>`;
}
function deck(group) {
  if (selected !== group.id) return `<div class="group deck-row" data-select="${group.id}" role="button" tabindex="0" aria-label="${group.name}, ${group.panes.length} panes"><div class="mini-stack">${mosaic(group)}</div><span class="group-title">${group.name}</span><span class="pane-count">${group.panes.length}</span>${marker(group.status)}</div>`;
  return `<div class="group selected deck-open"><div class="row-main">${marker(group.status)}<span class="group-title">${group.name}</span></div><div class="exposed-stack" style="height:${group.panes.length === 2 ? 72 : 92}px">${group.panes.map((name, index) => `<button class="exposed-plane ${index === focused ? 'active' : ''}" data-group="${group.id}" data-pane="${index}" aria-label="Focus ${name} in ${group.name}"><span class="plane-title"><span class="row-state ${index === 2 ? 'quiet' : ''}"></span>${name}</span><span class="plane-code">${['$ nvim sidebar.rs', '$ cargo run', '$ just check'][index] || '$'}</span></button>`).join('')}</div><div class="deck-caption"><span>${group.panes.length} panes · one split</span><span class="tiny-layout" title="Illustrative literal split arrangement">${mosaic(group)}</span></div></div>`;
}
function spatial(group) {
  return `<div class="group spatial-row ${selected === group.id ? 'selected' : ''}"><div class="row-main" data-select="${group.id}" role="button" tabindex="0">${marker(group.status)}<div class="group-labels"><span class="group-title">${group.name}</span></div><span class="pane-count">${group.panes.length}</span></div><div class="spatial-stack">${mosaic(group, true)}</div></div>`;
}
function renderGroups() {
  let previous = '';
  $('groups').innerHTML = groups.filter(group => `${group.session} ${group.name} ${group.panes.join(' ')}`.toLowerCase().includes(filter)).map(group => {
    const repeated = groups.filter(item => item.session === group.session).length > 1;
    const label = repeated && group.session !== previous ? `<div class="session-label"><span>${group.session}</span><span class="session-path">${group.path || ''}</span></div>` : '';
    previous = group.session;
    return label + (group.panes.length === 1 ? plain(group) : { A: inline, B: deck, C: spatial }[variant](group));
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
  $('terminals').style.gridTemplateColumns = group.panes.length === 1 ? '1fr' : group.panes.length === 2 ? '1fr 1fr' : '1.35fr 1fr';
  $('terminals').style.gridTemplateRows = group.panes.length > 2 ? '1fr 1fr' : '1fr';
  $('terminals').innerHTML = group.panes.map((name, index) => `<section class="terminal ${index === focused ? 'focused' : ''}" style="${group.panes.length <= 2 ? 'grid-row:auto' : ''}"><header class="terminal-bar"><span>${name}</span><span>${index === focused ? 'focused' : group.status}</span></header><pre>${examples[index] || examples[0]}</pre></section>`).join('');
  $('focus-label').textContent = `${group.panes[focused]} · ${group.panes.length} ${group.panes.length === 1 ? 'pane' : 'panes'}`;
  $('state').textContent = `${group.session} / ${group.name} / ${group.panes[focused]}`;
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
  $('variant-kicker').textContent = `VARIANT ${variant} / SAME SEVEN GROUPS · TWELVE PANES`;
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
